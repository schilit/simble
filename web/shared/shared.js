// SimBLE Shared Scene page glue.
//
// This page talks to common/link-worker.js — a module SharedWorker that owns
// ONE wasm WebLink for the whole origin — so devices added in different tabs
// share a single BLE scene. See link-worker.js for the wire protocol.
//
// GRACEFUL DEGRADATION: where SharedWorker (or module workers) is unavailable
// — Safari has no SharedWorker at all — we fall back to a private in-page
// WebLink that runs only in this tab. Both paths expose the same `hub`
// interface { addPeripheral, addScanner, addCentral } and drive the same
// renderer with the same { peripherals, scanners, centrals } snapshot shape, so
// the UI code below doesn't care which one is live.
//
// The renderer is deliberately self-contained (no viewer.js import): a compact
// services -> characteristics -> value view is enough for this demo.

import init, { WebLink } from "../pkg/simble.js";

const $ = (id) => document.getElementById(id);
const escapeHtml = (s) =>
  String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

// --- addresses --------------------------------------------------------------
// Random per-device addresses keep two tabs from colliding on the same address.
const hex2 = () => Math.floor(Math.random() * 256).toString(16).toUpperCase().padStart(2, "0");
const randomAddress = () => `DE:5C:${hex2()}:${hex2()}:${hex2()}:${hex2()}`;

// A small default peripheral so "Add" does something interesting immediately.
const scriptForName = (name) => `let server = android::BluetoothGattServer(${JSON.stringify(name)});

let hrs = android::BluetoothGattService(uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY);
let hr = android::BluetoothGattCharacteristic(
    uuid::HEART_RATE_MEASUREMENT,
    android::PROPERTY_READ | android::PROPERTY_NOTIFY,
    android::PERMISSION_READ,
);
hr.set_value([0x00, 72]);
let cccd = android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE,
);
hr.add_descriptor(cccd);
hrs.add_characteristic(hr);
server.add_service(hrs);

fn tick(server, t) {
    let bpm = 76 + (12.0 * sin(t / 4.0)).to_int();
    server.update_value(uuid::HEART_RATE_MEASUREMENT, [0x00, bpm]);
}
`;

// --- connection pill --------------------------------------------------------
function setPill(text, cls) {
  const pill = $("conn");
  pill.textContent = text;
  pill.className = "pill" + (cls ? " " + cls : "");
}

// ===========================================================================
// Hub A: SharedWorker-backed (the real cross-tab path).
// ===========================================================================
function makeWorkerHub(onStatus) {
  let worker;
  try {
    worker = new SharedWorker("../common/link-worker.js", { type: "module" });
  } catch (e) {
    // Some engines expose SharedWorker but not module workers; treat as absent.
    return null;
  }
  const port = worker.port;

  worker.onerror = (e) => setPill("worker error", "bad");
  port.onmessage = (ev) => {
    const msg = ev.data;
    if (!msg || typeof msg !== "object") return;
    switch (msg.type) {
      case "hello":
        setPill(msg.ready ? "shared · connected" : "shared · starting…", msg.ready ? "ok" : "");
        break;
      case "ready":
        setPill("shared · connected", "ok");
        break;
      case "fatal":
        showFatal(msg.message);
        break;
      case "added":
        $("add-addr").textContent =
          `added ${msg.role} ${msg.address || ""}`.trim();
        break;
      case "error":
        $("add-error").textContent = `${msg.cmd}: ${msg.message}`;
        break;
      case "status":
        onStatus(msg);
        break;
    }
  };
  port.start();

  // Heartbeat so the worker can detect this tab going away (MessagePort has no
  // reliable close event). pagehide/beforeunload send an explicit "bye" too,
  // but those aren't guaranteed to fire, hence the periodic ping + worker sweep.
  setInterval(() => port.postMessage({ cmd: "ping" }), 1000);
  const bye = () => { try { port.postMessage({ cmd: "bye" }); } catch (_) { /* gone */ } };
  window.addEventListener("pagehide", bye);
  window.addEventListener("beforeunload", bye);

  return {
    kind: "worker",
    addPeripheral: (address, script) => port.postMessage({ cmd: "addPeripheral", address, script }),
    addScanner: (address) => port.postMessage({ cmd: "addScanner", address }),
    addCentral: (address, target) => port.postMessage({ cmd: "addCentral", address, target }),
  };
}

// ===========================================================================
// Hub B: local in-page WebLink (fallback, single tab only).
// Mirrors the worker's status shape so the renderer is identical.
// ===========================================================================
async function makeLocalHub(onStatus) {
  await init();
  const link = new WebLink();
  const devices = []; // { index, role, address }
  const t0 = performance.now();

  const add = (role, fn, address) => {
    try {
      const index = fn();
      devices.push({ index, role, address });
      $("add-addr").textContent = `added ${role} ${address}`;
      return index;
    } catch (e) {
      $("add-error").textContent = `${role}: ${String((e && e.message) || e)}`;
    }
  };

  setInterval(() => {
    link.tick((performance.now() - t0) / 1000);
    const peripherals = {};
    const scanners = {};
    const centrals = {};
    for (const d of devices) {
      try {
        if (d.role === "peripheral") {
          peripherals[d.index] = { ...JSON.parse(link.peripheral_status_json(d.index)), mine: true, index: d.index };
        } else if (d.role === "central") {
          centrals[d.index] = { ...JSON.parse(link.central_status_json(d.index)), mine: true, index: d.index };
        } else {
          scanners[d.index] = { address: d.address, reports: JSON.parse(link.scanner_reports_json(d.index)), mine: true, index: d.index };
        }
      } catch (_) { /* skip a bad device this tick */ }
    }
    onStatus({ type: "status", t: (performance.now() - t0) / 1000, peripherals, scanners, centrals });
  }, 250);

  setPill("single-tab · in-browser Link", "");
  return {
    kind: "local",
    addPeripheral: (address, script) => add("peripheral", () => link.add_peripheral(address, script), address),
    addScanner: (address) => add("scanner", () => link.add_scanner(address), address),
    addCentral: (address, target) => add("central", () => link.add_central(address, target), address),
  };
}

function showFatal(message) {
  $("fatal").classList.add("visible");
  $("fatal-msg").textContent = message;
  setPill("failed", "bad");
}

// ===========================================================================
// Rendering (shared by both hubs).
// ===========================================================================
const UUID_NAMES = {
  "180D": "Heart Rate", "2A37": "Heart Rate Measurement",
  "180F": "Battery", "2A19": "Battery Level",
  "181A": "Environmental Sensing", "2A6E": "Temperature", "2A6F": "Humidity",
  "180A": "Device Information", "1800": "Generic Access", "1801": "Generic Attribute",
  "2902": "Client Characteristic Configuration",
};
const nameFor = (u) => UUID_NAMES[u] || null;

function mineBadge(mine) {
  return mine ? '<span class="mine-badge">mine</span>' : "";
}

function gattMini(status) {
  const services = status.services || [];
  if (!services.length) return "";
  return `<div class="gattmini">${services.map((s) => {
    const sName = nameFor(s.uuid);
    const head = `<div class="s">${sName ? escapeHtml(sName) + " " : ""}0x${s.uuid}</div>`;
    const chrs = (s.characteristics || []).map((c) => {
      const cName = nameFor(c.uuid);
      const label = cName ? escapeHtml(cName) : `0x${c.uuid}`;
      const val = c.value ? ` = <span class="v">${escapeHtml(c.value)}</span>` : "";
      return `<div class="c">${label} <span class="u">0x${c.uuid}</span>${val}</div>`;
    }).join("");
    return head + chrs;
  }).join("")}</div>`;
}

function renderPeripherals(map) {
  const items = Object.values(map);
  const el = $("peripherals");
  if (!items.length) { el.innerHTML = '<p class="empty">No peripherals yet.</p>'; return; }
  el.innerHTML = items.map((p) => {
    if (p.error) return `<div class="card2"><div class="meta">error: ${escapeHtml(p.error)}</div></div>`;
    const name = p.name ? escapeHtml(p.name) : "(unnamed)";
    const conn = p.connected ? `connected to ${escapeHtml(p.peer || "central")}` : "advertising";
    return `<div class="card2">
      <div class="top"><span class="nm">${name}</span>${mineBadge(p.mine)}
        <span class="addr">${escapeHtml(p.address || "")}</span></div>
      <div class="meta">${conn}</div>
      ${gattMini(p)}
    </div>`;
  }).join("");
}

function renderCentrals(map) {
  const items = Object.values(map);
  const el = $("centrals");
  if (!items.length) { el.innerHTML = '<p class="empty">No centrals yet.</p>'; return; }
  el.innerHTML = items.map((c) => {
    if (c.error) return `<div class="card2"><div class="meta">error: ${escapeHtml(c.error)}</div></div>`;
    const svcCount = (c.services || []).length;
    const status = c.connected
      ? `connected to ${escapeHtml(c.peer || "?")} · ${escapeHtml(c.phase || "")}${svcCount ? ` · ${svcCount} service${svcCount === 1 ? "" : "s"} discovered` : ""}`
      : `connecting to ${escapeHtml(c.peer || "?")}…`;
    return `<div class="card2">
      <div class="top"><span class="nm">Central</span>${mineBadge(c.mine)}
        <span class="addr">${escapeHtml(c.address || "")}</span></div>
      <div class="meta">${status}</div>
    </div>`;
  }).join("");
}

// The scanner's report queue is drained incrementally by the worker/hub, so we
// aggregate what we've seen by address (like the Scanner page does) and age
// entries out. Only our own scanner(s) are shown.
const seen = new Map(); // address -> { name, rssi, service_uuids, lastSeen }
function ingestScanner(map) {
  for (const s of Object.values(map)) {
    if (!s.mine || !Array.isArray(s.reports)) continue;
    for (const r of s.reports) {
      const prev = seen.get(r.address) || {};
      const merged = { ...prev, address: r.address, rssi: r.rssi, lastSeen: performance.now() };
      if (r.name) merged.name = r.name;
      if (r.service_uuids && r.service_uuids.length) merged.service_uuids = r.service_uuids;
      seen.set(r.address, merged);
    }
  }
}

function renderScanner() {
  const el = $("scanner");
  const nowT = performance.now();
  for (const [addr, d] of seen) if (nowT - d.lastSeen > 6000) seen.delete(addr); // age out gone advertisers
  const list = [...seen.values()].sort((a, b) => b.rssi - a.rssi);
  if (!list.length) {
    el.innerHTML = '<p class="empty">Add a scanner to see advertisers on the air.</p>';
    return;
  }
  el.innerHTML = list.map((d) => {
    const name = d.name ? escapeHtml(d.name) : '<span class="hint">(no name)</span>';
    const svcs = (d.service_uuids || []).map((u) => `svc ${escapeHtml(u)}`).join(" · ");
    return `<div class="card2">
      <div class="top"><span class="nm">${name}</span>
        <span class="addr">${escapeHtml(d.address)}</span></div>
      <div class="meta">${d.rssi} dBm${svcs ? " · " + svcs : ""}</div>
    </div>`;
  }).join("");
}

// Track known peripheral addresses so the Central target dropdown stays current.
function refreshTargets(peripherals) {
  const sel = $("target");
  const addrs = Object.values(peripherals).map((p) => p.address).filter(Boolean);
  const chosen = sel.value;
  sel.innerHTML = addrs.length
    ? addrs.map((a) => `<option value="${escapeHtml(a)}">${escapeHtml(a)}</option>`).join("")
    : '<option value="">(no peripherals in scene yet)</option>';
  if (addrs.includes(chosen)) sel.value = chosen;
}

function onStatus(msg) {
  renderPeripherals(msg.peripherals || {});
  renderCentrals(msg.centrals || {});
  ingestScanner(msg.scanners || {});
  renderScanner();
  refreshTargets(msg.peripherals || {});
  const n = Object.keys(msg.peripherals || {}).length +
    Object.keys(msg.scanners || {}).length +
    Object.keys(msg.centrals || {}).length;
  $("scene-count").textContent = n ? `· ${n} device${n === 1 ? "" : "s"} in scene` : "";
}

// ===========================================================================
// UI wiring.
// ===========================================================================
let role = "peripheral";

function selectRole(r) {
  role = r;
  for (const b of $("role-seg").querySelectorAll("button")) {
    b.classList.toggle("active", b.dataset.role === r);
  }
  $("fld-name").style.display = r === "peripheral" ? "" : "none";
  $("fld-target").style.display = r === "central" ? "" : "none";
}

let hub = null;

function doAdd() {
  $("add-error").textContent = "";
  $("add-addr").textContent = "";
  if (!hub) { $("add-error").textContent = "not ready yet"; return; }
  const address = randomAddress();
  if (role === "peripheral") {
    hub.addPeripheral(address, $("script").value);
  } else if (role === "scanner") {
    hub.addScanner(address);
  } else {
    const target = $("target").value;
    if (!target) { $("add-error").textContent = "no peripheral to connect to yet"; return; }
    hub.addCentral(address, target);
  }
}

async function boot() {
  $("script").value = scriptForName("Tab Device");
  $("name").addEventListener("input", () => { $("script").value = scriptForName($("name").value.trim() || "Tab Device"); });
  for (const b of $("role-seg").querySelectorAll("button")) {
    b.addEventListener("click", () => selectRole(b.dataset.role));
  }
  $("add").addEventListener("click", doAdd);

  if (typeof SharedWorker !== "undefined") {
    hub = makeWorkerHub(onStatus);
  }
  if (!hub) {
    // No SharedWorker (or module workers): private in-page Link.
    $("fallback").classList.add("visible");
    hub = await makeLocalHub(onStatus);
  }
}

boot();
