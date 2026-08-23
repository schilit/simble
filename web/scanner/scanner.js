// Simble Web Scanner page glue. All HCI/GAP work (scan bring-up, advertising
// report parsing, AD-structure decoding) happens in Rust compiled to wasm; this
// file aggregates reports per address and renders DOM. It runs against either
// controller backend (see the selector at the top of the page):
//   • in-page  — a wasm Link in this tab; the scanner and its demo advertisers
//                share it, so the page works with no netsim at all.
//   • websocket — a real netsim / rootcanal-ws scene over ws://localhost:7681.

import init, { WebScanner, WebAdvertiser, WebLink } from "../pkg/simble.js";
import { createControllerBar } from "../common/controller-bar.js";

const SCANNER_ADDR = "CC:1E:57:00:00:01";
const wsUrl = (node, addr) =>
  `ws://localhost:7681/v1/websocket/bt?name=${encodeURIComponent(node)}&address=${addr}`;

// Built-in demo advertisers so the scanner shows something with no real scene:
// name on the air, unique address, optional 16-bit service UUID (0 = none), and
// (WebSocket backend only) optional manufacturer data.
const DEMO_DEVICES = [
  { node: "demo-beacon", address: "CC:1E:57:00:00:11", name: "SimBLE Beacon",
    service: 0x0000, company: 0x0059, data: [0x01, 0x02, 0x03, 0x04] },
  { node: "demo-thermo", address: "CC:1E:57:00:00:12", name: "SimBLE Thermometer",
    service: 0x181a, company: 0x0000, data: [] },
  { node: "demo-hrm", address: "CC:1E:57:00:00:13", name: "SimBLE Heart Rate",
    service: 0x180d, company: 0x0000, data: [] },
];

const $ = (id) => document.getElementById(id);
const devices = new Map(); // address -> merged report + lastSeen

let backend = null;
let mode = "in-page";
let demosOn = true;

function setPill(text, cls) {
  const pill = $("conn");
  pill.textContent = text;
  pill.className = "pill" + (cls ? " " + cls : "");
}

// --- WebSocket backend (netsim / rootcanal-ws) -----------------------------
function makeWsBackend() {
  let scanner = null;
  let advertisers = [];
  let openedOnce = false;
  let lastAttempt = 0;
  const startScanner = () => {
    try { scanner = new WebScanner(wsUrl("web-scanner", SCANNER_ADDR)); }
    catch (e) { scanner = null; console.error("WebScanner:", e); }
  };
  const startDemos = () => {
    if (advertisers.length) return;
    for (const d of DEMO_DEVICES) {
      try {
        advertisers.push(new WebAdvertiser(wsUrl(d.node, d.address), d.name, d.service, d.company, new Uint8Array(d.data)));
      } catch (e) { console.error("WebAdvertiser:", d.name, e); }
    }
  };
  const stopDemos = () => {
    for (const a of advertisers) { try { a.free(); } catch (_) { /* gone */ } }
    advertisers = [];
    for (const d of DEMO_DEVICES) devices.delete(d.address.toUpperCase());
  };
  startScanner();
  return {
    tick() {
      if (!scanner) {
        const now = performance.now();
        if (now - lastAttempt > 3000) { lastAttempt = now; startScanner(); }
        return { reports: [] };
      }
      const state = scanner.ready_state(); // 0 connecting 1 open 2 closing 3 closed
      if (state === 3) {
        setPill(openedOnce ? "connection lost — reconnecting…" : "netsim not reachable", "bad");
        if (!openedOnce) $("setup").classList.add("visible");
        try { scanner.free(); } catch (_) { /* gone */ }
        scanner = null;
        stopDemos();
        return { reports: [] };
      }
      if (state === 0) {
        setPill(openedOnce ? "reconnecting…" : "connecting to localhost:7681…", "");
        return { reports: [] };
      }
      openedOnce = true;
      $("setup").classList.remove("visible");
      if (demosOn) {
        startDemos();
        for (const a of advertisers) { try { if (a.ready_state() !== 3) a.tick(); } catch (_) { /* skip */ } }
      } else {
        stopDemos();
      }
      let reports = [];
      try { reports = JSON.parse(scanner.tick()); } catch (e) { console.error("tick:", e); }
      setPill(`scanning · ${devices.size} device${devices.size === 1 ? "" : "s"}`, "ok");
      return { reports };
    },
    teardown() { stopDemos(); try { scanner?.free(); } catch (_) { /* gone */ } scanner = null; },
  };
}

// --- In-page backend (a wasm Link in this tab) -----------------------------
function makeInPageBackend() {
  const link = new WebLink();
  const scannerIndex = link.add_scanner(SCANNER_ADDR);
  if (demosOn) {
    for (const d of DEMO_DEVICES) {
      let script = `let server = android::BluetoothGattServer(${JSON.stringify(d.name)});`;
      if (d.service) {
        const hex = d.service.toString(16).toUpperCase().padStart(4, "0");
        script += ` let s = android::BluetoothGattService(uuid::of("${hex}"), android::SERVICE_TYPE_PRIMARY); server.add_service(s);`;
      }
      try { link.add_peripheral(d.address, script); } catch (e) { console.error("in-page advertiser:", d.name, e); }
    }
  }
  const t0 = performance.now();
  $("setup").classList.remove("visible");
  return {
    tick() {
      link.tick((performance.now() - t0) / 1000);
      let reports = [];
      try { reports = JSON.parse(link.scanner_reports_json(scannerIndex)); } catch (e) { console.error("tick:", e); }
      const n = link.device_count() - 1;
      setPill(`scanning · ${n} advertiser${n === 1 ? "" : "s"}`, "ok");
      return { reports };
    },
    teardown() { try { link.free(); } catch (_) { /* gone */ } },
  };
}

function rebuildBackend() {
  backend?.teardown();
  devices.clear();
  backend = mode === "websocket" ? makeWsBackend() : makeInPageBackend();
  render();
}

// --- report aggregation + rendering ----------------------------------------
function merge(report) {
  const existing = devices.get(report.address) ?? {};
  const merged = { ...existing };
  merged.address = report.address;
  merged.address_type = report.address_type;
  merged.rssi = report.rssi;
  merged.lastSeen = performance.now();
  if (report.name) merged.name = report.name;
  if (!report.scan_response) {
    merged.connectable = report.connectable;
    merged.raw = report.raw;
    if (report.service_uuids.length) merged.service_uuids = report.service_uuids;
    if (report.service_data.length) merged.service_data = report.service_data;
    if (report.manufacturer_data) merged.manufacturer_data = report.manufacturer_data;
    if (report.flags !== null) merged.flags = report.flags;
    if (report.tx_power !== null) merged.tx_power = report.tx_power;
  }
  devices.set(report.address, merged);
}

const escapeHtml = (s) =>
  String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

function deviceHtml(d) {
  const name = d.name ? escapeHtml(d.name) : '<span class="anon">(no name)</span>';
  const chips = [];
  if (d.connectable) chips.push("connectable");
  for (const uuid of d.service_uuids ?? []) chips.push(`svc ${uuid}`);
  if (d.tx_power !== undefined && d.tx_power !== null) chips.push(`tx ${d.tx_power} dBm`);
  const details = [];
  for (const sd of d.service_data ?? []) details.push(`service ${sd.tag}: ${sd.data}`);
  if (d.manufacturer_data)
    details.push(`mfg 0x${d.manufacturer_data.tag}: ${d.manufacturer_data.data}`);
  const pct = Math.max(4, Math.min(100, Math.round(((d.rssi + 100) / 70) * 100)));
  const stale = performance.now() - d.lastSeen > 5000 ? " stale" : "";
  return `<div class="device${stale}">
    <div>
      <div class="name">${name}
        ${chips.map((c) => `<span class="chip">${escapeHtml(c)}</span>`).join("")}</div>
      <div class="addr">${d.address} · ${d.address_type}</div>
    </div>
    <div class="rssi"><span class="db">${d.rssi} dBm</span>
      <div class="bar"><span style="width:${pct}%"></span></div></div>
    ${details.length ? `<div class="ad">${details.map(escapeHtml).join(" · ")}</div>` : ""}
  </div>`;
}

function render() {
  const list = [...devices.values()].sort(
    (a, b) => (a.name ? 0 : 1) - (b.name ? 0 : 1) || b.rssi - a.rssi
  );
  $("devices").innerHTML = list.map(deviceHtml).join("");
  const empty = $("empty");
  empty.style.display = list.length ? "none" : "block";
  empty.textContent = demosOn
    ? "No advertisements yet — scanning…"
    : (mode === "websocket"
        ? "No devices advertising — turn on demo devices above, run netsimd with --test-beacons, or open the scripted-device page in another tab."
        : "No devices — turn on demo devices above.");
}

function loop() {
  if (!backend) return;
  const { reports } = backend.tick();
  for (const report of reports) merge(report);
  render();
}

await init();

const controllerBar = createControllerBar({
  supports: { "in-page": true, "websocket": true },
  onChange: (m) => { mode = m; rebuildBackend(); },
});
controllerBar.el.classList.add("standalone");
$("backend").append(controllerBar.el);
mode = controllerBar.selected;
$("demo-toggle").addEventListener("change", (e) => {
  demosOn = e.target.checked;
  rebuildBackend();
});
rebuildBackend();
setInterval(loop, 250);
