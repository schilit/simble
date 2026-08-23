// SimBLE Scene creator. The user assembles a whole BLE scene — scripted
// peripherals (servers), passive scanners, and centrals (clients) — then clicks
// any device to inspect it. The page keeps an array of device *specs* (the
// source of truth: there is no single source inside an engine) and drives them
// through one of two controller backends chosen with the selector at the top:
//   • in-page  — one wasm `Link` in this tab hosts every device; any change
//                rebuilds a fresh Link from the specs. No netsim needed.
//   • websocket — each device is its own WebSocket engine (WebPeripheral /
//                WebScanner) joining a real netsim / rootcanal-ws scene over
//                ws://localhost:7681, alongside the Android emulator. The
//                central (client) role only exists in the in-page Link, so it
//                is disabled over netsim (there the emulator app is the central).
// Rendering helpers are shared with the other pages via viewer.js.

import init, { WebLink, WebPeripheral, WebScanner } from "../pkg/simble.js";
import { renderGatt, gattViewFor, escapeHtml } from "../common/viewer.js";
import { createControllerBar } from "../common/controller-bar.js";
import { createAboutBox } from "../common/about-box.js";

const $ = (id) => document.getElementById(id);

// netsim WebSocket endpoint (same shape as the scanner page).
const wsUrl = (node, addr) =>
  `ws://localhost:7681/v1/websocket/bt?name=${encodeURIComponent(node)}&address=${addr}`;

// --- default scripts for a new server ---------------------------------------
const HRM_SCRIPT = `// Heart-rate monitor: notifies BPM, plus a battery level.
let server = android::BluetoothGattServer("HRM Server");

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

let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
let lvl = android::BluetoothGattCharacteristic(
    uuid::BATTERY_LEVEL, android::PROPERTY_READ, android::PERMISSION_READ);
lvl.set_value([100]);
bas.add_characteristic(lvl);
server.add_service(bas);

fn tick(server, t) {
    let bpm = 76 + (12.0 * sin(t / 4.0)).to_int();
    server.update_value(uuid::HEART_RATE_MEASUREMENT, [0x00, bpm]);
}
`;

// --- scene model ------------------------------------------------------------
// The source of truth. Each spec: { id, kind, address, target, script, ... }
//   kind    : "peripheral" | "scanner" | "central"
//   address : this device's own BD address (assigned automatically)
//   target  : central only — address of the peripheral it connects to
//   script  : peripheral only — its Rhai source
// Backend-private fields added at runtime:
//   index   : in-page — the device's index inside the current WebLink
//   _engine : websocket — this device's own WebPeripheral / WebScanner
//   _t0     : websocket peripheral — perf clock when its script last (re)ran
//   _status : websocket peripheral — last status JSON string from tick
//   _reportsJson : websocket scanner — last reports JSON string from tick
//   _openedOnce  : websocket — has this engine's socket ever been open
//   _reports     : per-scanner advertisement aggregation
const specs = [];
let nextId = 1;      // stable per-device id (never reused)
let nextAddr = 1;    // next auto-assigned address suffix
let openId = null;   // id of the device shown in the inspect panel

let mode = "in-page";     // controller backend: "in-page" | "websocket"
let link = null;          // in-page mode: the shared WebLink (null in websocket)
let t0 = performance.now();
const prevValues = new Map(); // peripheral GATT value pulse tracking

const fmtAddr = (n) => "AA:BB:CC:00:00:" + n.toString(16).toUpperCase().padStart(2, "0");
const roleLabel = { peripheral: "Server", scanner: "Scanner", central: "Client" };
const roleKind = { peripheral: "peripheral · Rhai script", scanner: "scanner · passive", central: "central · connects & discovers" };

// A stable netsim node name for a device (websocket mode).
const nodeName = (spec) =>
  spec.kind === "scanner" ? "web-scanner" : `web-peripheral-${spec.id}`;

function setPill(text, cls) {
  const pill = $("conn");
  pill.textContent = text;
  pill.className = "pill" + (cls ? " " + cls : "");
}

// ===========================================================================
//  In-page backend (one shared WebLink)
// ===========================================================================

// Rebuild a fresh WebLink from the current specs. Built into a temp Link first
// so a bad peripheral script throws before we discard the working Link.
function rebuild() {
  let nl = null;
  try {
    nl = new WebLink();
    for (const s of specs) {
      if (s.kind === "peripheral") s.index = nl.add_peripheral(s.address, s.script);
      else if (s.kind === "scanner") s.index = nl.add_scanner(s.address);
      else if (s.kind === "central") s.index = nl.add_central(s.address, s.target);
      s._reports = new Map(); // per-scanner aggregation, reset on rebuild
    }
  } catch (e) {
    try { nl && nl.free(); } catch (_) { /* gone */ }
    throw e;
  }
  if (link) { try { link.free(); } catch (_) { /* gone */ } }
  link = nl;
  t0 = performance.now();
  prevValues.clear();
}

// ===========================================================================
//  WebSocket backend (each device is its own netsim engine)
// ===========================================================================

// (Re)build one device's engine. Constructs the new engine first, then frees
// any old one, so a throwing constructor (e.g. a bad script) leaves the
// previous engine untouched. Centrals have no netsim engine.
function buildEngine(spec) {
  if (spec.kind === "central") return;
  let ne = null;
  if (spec.kind === "peripheral") ne = new WebPeripheral(wsUrl(nodeName(spec), spec.address), spec.script);
  else if (spec.kind === "scanner") ne = new WebScanner(wsUrl(nodeName(spec), spec.address));
  if (spec._engine) { try { spec._engine.free(); } catch (_) { /* gone */ } }
  spec._engine = ne;
  spec._t0 = performance.now();
  spec._status = null;
  spec._reportsJson = null;
  spec._openedOnce = false;
  spec._reports = new Map();
}

function freeEngine(spec) {
  if (spec._engine) { try { spec._engine.free(); } catch (_) { /* gone */ } spec._engine = null; }
}

// ===========================================================================
//  Backend lifecycle (mode switch, add, remove)
// ===========================================================================
function teardownBackend() {
  if (link) { try { link.free(); } catch (_) { /* gone */ } link = null; }
  for (const s of specs) freeEngine(s);
}

function buildBackend() {
  if (mode === "in-page") {
    rebuild();
  } else {
    for (const s of specs) buildEngine(s);
  }
}

// Called by the selector: tear the current backend down and rebuild in the new
// mode from the same specs.
function setMode(m) {
  mode = m;
  try {
    teardownBackend();
    buildBackend();
  } catch (e) {
    console.error("mode switch:", e);
    setPill("backend failed — see inspect panel", "bad");
    renderScene();
    renderInspect(String(e));
    return;
  }
  $("setup").classList.remove("visible");
  renderScene();
  renderInspect();
}

// Apply the effect of adding `spec` to the live backend.
function syncAdd(spec) {
  if (mode === "in-page") rebuild();
  else buildEngine(spec);
}

// Apply the effect of removing `removed` from the live backend.
function syncRemove(removed) {
  if (mode === "in-page") rebuild();
  else freeEngine(removed);
}

// --- scene mutations --------------------------------------------------------
function peripheralOptions() {
  return specs.filter((s) => s.kind === "peripheral");
}

function addServer() {
  const spec = { id: nextId++, kind: "peripheral", address: fmtAddr(nextAddr++), script: HRM_SCRIPT };
  specs.push(spec);
  commit(() => { syncAdd(spec); openId = spec.id; }, () => { specs.pop(); nextAddr--; });
}

function addScanner() {
  const spec = { id: nextId++, kind: "scanner", address: fmtAddr(nextAddr++) };
  specs.push(spec);
  commit(() => { syncAdd(spec); openId = spec.id; }, () => { specs.pop(); nextAddr--; });
}

function addClient() {
  if (mode === "websocket") {
    setPill("clients are in-browser only — the emulator app is the central over netsim", "bad");
    return;
  }
  const servers = peripheralOptions();
  if (!servers.length) {
    setPill("add a server first — a client needs one to connect to", "bad");
    return;
  }
  const spec = { id: nextId++, kind: "central", address: fmtAddr(nextAddr++), target: servers[0].address };
  specs.push(spec);
  commit(() => { syncAdd(spec); openId = spec.id; }, () => { specs.pop(); nextAddr--; });
}

function removeDevice(id) {
  const idx = specs.findIndex((s) => s.id === id);
  if (idx < 0) return;
  const removed = specs.splice(idx, 1)[0];
  if (openId === id) openId = null;
  commit(() => syncRemove(removed), () => specs.splice(idx, 0, removed));
}

// Attempt a mutation; on failure run `revert` and surface the error.
function commit(apply, revert) {
  try {
    apply();
  } catch (e) {
    try { revert(); } catch (_) { /* best effort */ }
    console.error("scene mutation:", e);
    setPill("update failed — see inspect panel", "bad");
    renderScene();
    renderInspect(String(e));
    return;
  }
  renderScene();
  renderInspect();
}

// --- scene list (left) ------------------------------------------------------
function renderScene() {
  const list = $("dev-list");
  list.innerHTML = specs.map((s) => {
    const sub = s.kind === "central" ? `<small>→ ${escapeHtml(s.target)}</small>` : "";
    return `<li class="dev-item ${s.kind}${s.id === openId ? " open" : ""}" data-id="${s.id}">
      <span class="role-dot">●</span>
      <span class="meta">
        <span class="role">${roleLabel[s.kind]}${sub}</span>
        <span class="addr">${escapeHtml(s.address)}</span>
      </span>
      <button class="rm" data-rm="${s.id}" title="Remove">✕</button>
    </li>`;
  }).join("");
  $("scene-empty").style.display = specs.length ? "none" : "block";
  $("scene-count").textContent = specs.length
    ? `${specs.length} device${specs.length === 1 ? "" : "s"}` : "";
  // Clients (centrals) only exist on the in-browser Link.
  const ws = mode === "websocket";
  $("add-client").disabled = ws || !peripheralOptions().length;
  $("client-note").style.display = ws ? "block" : "none";
  $("ws-note").style.display = ws ? "block" : "none";
}

// --- inspect panel (right) --------------------------------------------------
function renderInspect(error) {
  const el = $("inspect");
  const spec = specs.find((s) => s.id === openId);
  if (!spec) {
    el.innerHTML = error
      ? `<div class="error">${escapeHtml(error)}</div>`
      : `<p class="empty">Select a device on the left to inspect it.</p>`;
    return;
  }
  const roleClass = spec.kind + "-role";
  const head = `<div class="insp-head">
      <span class="role ${roleClass}">● ${roleLabel[spec.kind]}</span>
      <span class="kind">${roleKind[spec.kind]}</span>
      <span class="addr">${escapeHtml(spec.address)}</span>
    </div>`;

  if (spec.kind === "peripheral") {
    el.innerHTML = head +
      `<h3 class="sub">GATT database (server view)</h3><div id="insp-gatt"></div>
       <h3 class="sub">Rhai script</h3>
       <textarea class="code" id="insp-script" spellcheck="false"></textarea>
       <div class="row"><button class="primary" id="insp-apply">▶ Apply &amp; rebuild</button>
         <span class="hint" id="insp-apply-state"></span></div>
       ${error ? `<div class="error">${escapeHtml(error)}</div>` : ""}`;
    $("insp-script").value = spec.script;
    $("insp-apply").addEventListener("click", () => applyScript(spec));
    prevValues.clear();
    refreshOpen();
  } else if (spec.kind === "scanner") {
    el.innerHTML = head +
      `<h3 class="sub">Live advertisement reports</h3>
       <div id="insp-reports"><p class="empty">Scanning…</p></div>`;
    refreshOpen();
  } else { // central
    if (mode === "websocket") {
      el.innerHTML = head +
        `<div class="phase">Unavailable in netsim mode</div>
         <p class="empty">The client (central) role only exists on the in-browser Link. Over netsim the
           <a href="../emulator/">Android emulator app</a> is the central that connects to these
           peripherals — switch the controller to <strong>In browser</strong> to drive a client from this page.</p>`;
      return;
    }
    el.innerHTML = head +
      `<div class="phase">Connection: <b id="insp-phase">idle</b> · peer
         <span id="insp-peer">${escapeHtml(spec.target)}</span></div>
       <h3 class="sub">Discovered services (client view)</h3>
       <div id="insp-disc"><p class="empty">Connecting…</p></div>`;
    refreshOpen();
  }
}

function applyScript(spec) {
  const src = $("insp-script").value;
  const prev = spec.script;
  spec.script = src;
  try {
    // in-page rebuilds the whole Link; websocket recreates just this engine.
    if (mode === "in-page") rebuild();
    else buildEngine(spec);
    $("insp-apply-state").textContent = mode === "in-page" ? "rebuilt" : "engine restarted";
    setTimeout(() => { const n = $("insp-apply-state"); if (n) n.textContent = ""; }, 2000);
  } catch (e) {
    spec.script = prev;
    try { if (mode === "in-page") rebuild(); else buildEngine(spec); } catch (_) { /* keep whatever we have */ }
    const n = $("insp-apply-state");
    if (n) n.textContent = "";
    const el = $("inspect");
    const errDiv = document.createElement("div");
    errDiv.className = "error";
    errDiv.textContent = String(e);
    el.appendChild(errDiv);
  }
}

// Discovered-GATT tree from a central's status JSON. This used to be a
// hand-rolled copy of the same tree the Generic page draws; it is now the
// shared widget's client view, which knows the "connecting" and "discovering"
// states a central passes through and shows attribute handles because a client
// addresses things by handle. No operation handlers are passed, so no read /
// write / subscribe buttons appear -- this inspector reads a device, it does
// not drive one.
function renderDiscovered(el, status) {
  const phaseEl = $("insp-phase");
  if (phaseEl) phaseEl.textContent = status.phase || "idle";
  gattViewFor(el, { mode: "client" }).update(status);
}

// Aggregate advertisement reports per address and render them.
function renderReports(el, spec, reports) {
  const now = performance.now();
  for (const r of reports) {
    const prev = spec._reports.get(r.address) || {};
    const m = { ...prev, address: r.address, address_type: r.address_type, rssi: r.rssi, lastSeen: now };
    if (r.name) m.name = r.name;
    if (!r.scan_response) {
      m.connectable = r.connectable;
      if (r.service_uuids && r.service_uuids.length) m.service_uuids = r.service_uuids;
    }
    spec._reports.set(r.address, m);
  }
  const rows = [...spec._reports.values()].sort((a, b) => (a.name ? 0 : 1) - (b.name ? 0 : 1) || b.rssi - a.rssi);
  if (!rows.length) { el.innerHTML = '<p class="empty">No advertisements yet — scanning…</p>'; return; }
  el.innerHTML = rows.map((d) => {
    const name = d.name ? escapeHtml(d.name) : '<span class="anon">(no name)</span>';
    const chips = [];
    if (d.connectable) chips.push("connectable");
    for (const u of d.service_uuids || []) chips.push(`svc ${u}`);
    const stale = now - d.lastSeen > 4000 ? " stale" : "";
    return `<div class="rpt${stale}">
      <div class="name">${name}</div>
      <div class="db">${d.rssi} dBm</div>
      <div class="addr">${escapeHtml(d.address)} · ${escapeHtml(d.address_type)}</div>
      ${chips.length ? `<div class="chips">${chips.map((c) => `<span class="chip">${escapeHtml(c)}</span>`).join("")}</div>` : ""}
    </div>`;
  }).join("");
}

// Refresh just the open device's inspect view from the live backend.
function refreshOpen() {
  const spec = specs.find((s) => s.id === openId);
  if (!spec) return;
  try {
    if (mode === "in-page") refreshOpenInPage(spec);
    else refreshOpenWs(spec);
  } catch (e) {
    console.error("inspect refresh:", e);
  }
}

function refreshOpenInPage(spec) {
  if (!link) return;
  if (spec.kind === "peripheral") {
    const gatt = $("insp-gatt");
    if (gatt) renderGatt(gatt, JSON.parse(link.peripheral_status_json(spec.index)), prevValues);
  } else if (spec.kind === "scanner") {
    const box = $("insp-reports");
    if (box) renderReports(box, spec, JSON.parse(link.scanner_reports_json(spec.index)));
  } else {
    const box = $("insp-disc");
    if (box) renderDiscovered(box, JSON.parse(link.central_status_json(spec.index)));
  }
}

function refreshOpenWs(spec) {
  if (spec.kind === "peripheral") {
    const gatt = $("insp-gatt");
    if (!gatt) return;
    if (spec._status) renderGatt(gatt, JSON.parse(spec._status), prevValues);
    else gatt.innerHTML = '<p class="empty">Waiting for netsim…</p>';
  } else if (spec.kind === "scanner") {
    const box = $("insp-reports");
    if (!box) return;
    if (spec._reportsJson) renderReports(box, spec, JSON.parse(spec._reportsJson));
  }
  // central: rendered statically by renderInspect (unavailable in netsim mode).
}

// ===========================================================================
//  Tick loop
// ===========================================================================
function loop() {
  if (mode === "in-page") loopInPage();
  else loopWs();
}

function loopInPage() {
  if (!link) return;
  try { link.tick((performance.now() - t0) / 1000); } catch (e) { console.error("tick:", e); return; }
  refreshOpen();
  const n = specs.length;
  setPill(n ? `${n} device${n === 1 ? "" : "s"} on one Link` : "empty scene — add a device", n ? "ok" : "");
}

// Pump every device's own netsim socket each frame, storing its latest status /
// reports for the inspect panel. A single closed-before-open socket surfaces the
// "netsim not reachable" hint.
function loopWs() {
  let openCount = 0, connecting = false, closedBeforeOpen = false;
  for (const s of specs) {
    if (!s._engine) continue;
    let st = 3;
    try { st = s._engine.ready_state(); } catch (_) { st = 3; }
    if (st === 1) { s._openedOnce = true; openCount++; }
    else if (st === 0) connecting = true;
    else if (st === 3 && !s._openedOnce) closedBeforeOpen = true;
    if (st === 3) continue; // don't tick a dead socket
    try {
      if (s.kind === "peripheral") s._status = s._engine.tick((performance.now() - s._t0) / 1000);
      else if (s.kind === "scanner") s._reportsJson = s._engine.tick();
    } catch (e) { console.error("ws tick:", e); }
  }
  refreshOpen();

  const anyOpened = specs.some((s) => s._openedOnce);
  const engineCount = specs.filter((s) => s.kind !== "central").length;
  if (!engineCount) {
    $("setup").classList.remove("visible");
    setPill("empty scene — add a device", "");
  } else if (!anyOpened && closedBeforeOpen) {
    $("setup").classList.add("visible");
    setPill("netsim not reachable", "bad");
  } else if (openCount) {
    $("setup").classList.remove("visible");
    setPill(`${openCount} device${openCount === 1 ? "" : "s"} in netsim scene`, "ok");
  } else if (connecting) {
    setPill("connecting to localhost:7681…", "");
  }
}

// ===========================================================================
//  Wiring
// ===========================================================================
await init();

const controllerBar = createControllerBar({
  supports: { "in-page": true, "websocket": true },
  onChange: setMode,
});
controllerBar.el.classList.add("standalone");
$("backend").append(controllerBar.el);
mode = controllerBar.selected;

// This used to be a bare paragraph above the controller card. It is the same
// explanation every other page keeps in its About box, so it lives in one now
// — below the control it talks about, rather than above it. Scene keeps its
// own open/closed preference: see the note in common/about-box.js.
$("about").append(createAboutBox(
  `<p>Build a Bluetooth scene device by device — scripted
   <strong class="peripheral-role">servers</strong> (peripherals), passive
   <strong class="scanner-role">scanners</strong>, and
   <strong class="central-role">clients</strong> (centrals) that connect to a server and
   discover its GATT. Click any device to inspect it.</p>
   <p style="margin-bottom:0">The controller above decides how the scene is hosted:
   <strong>In browser</strong> puts every device on one wasm <code>Link</code> in this tab and
   needs nothing installed, while <strong>WebSocket</strong> gives each device its own engine in
   a real netsim scene — the same scene the
   <a href="../emulator/">Android emulator</a> is on.</p>`,
  { key: "scene" }));

$("add-server").addEventListener("click", addServer);
$("add-scanner").addEventListener("click", addScanner);
$("add-client").addEventListener("click", addClient);

$("dev-list").addEventListener("click", (e) => {
  const rm = e.target.closest("[data-rm]");
  if (rm) { e.stopPropagation(); removeDevice(Number(rm.dataset.rm)); return; }
  const item = e.target.closest(".dev-item");
  if (item) { openId = Number(item.dataset.id); renderScene(); renderInspect(); }
});

// Start with a sensible non-empty scene: a server, a client on it, a scanner.
// addClient no-ops in websocket mode (clients are in-browser only).
addServer();
addClient();
addScanner();
openId = specs[0] ? specs[0].id : null;
renderScene();
renderInspect();
setInterval(loop, 100);
