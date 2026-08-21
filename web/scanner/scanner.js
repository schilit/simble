// Simble Web Scanner page glue. All HCI/GAP work (scan bring-up, advertising
// report parsing, AD-structure decoding) happens in Rust compiled to wasm;
// this file aggregates reports per address, renders DOM, and — so the scanner
// demos something on a bare netsimd — spins up a few built-in advertiser
// devices of its own (each a separate WebSocket into the same netsim scene).

import init, { WebScanner, WebAdvertiser } from "../pkg/simble.js";

const WS_URL =
  "ws://localhost:7681/v1/websocket/bt?name=web-scanner&address=CC:1E:57:00:00:01";

// Built-in demo advertisers: name shown on the air, netsim node name + address
// (unique per device), optional 16-bit service UUID (0 = none), and optional
// manufacturer data (company id + bytes; empty = none).
const DEMO_DEVICES = [
  { node: "demo-beacon", address: "CC:1E:57:00:00:11", name: "Simble Beacon",
    service: 0x0000, company: 0x0059, data: [0x01, 0x02, 0x03, 0x04] },
  { node: "demo-thermo", address: "CC:1E:57:00:00:12", name: "Simble Thermometer",
    service: 0x181a, company: 0x0000, data: [] },
  { node: "demo-hrm", address: "CC:1E:57:00:00:13", name: "Simble Heart Rate",
    service: 0x180d, company: 0x0000, data: [] },
];

const $ = (id) => document.getElementById(id);
const devices = new Map(); // address -> merged report + lastSeen

let scanner = null;
let advertisers = []; // live WebAdvertiser handles when demos are on
let lastConnectAttempt = 0;
let openedOnce = false;
let demosOn = true;

function setPill(text, cls) {
  const pill = $("conn");
  pill.textContent = text;
  pill.className = "pill" + (cls ? " " + cls : "");
}

function connect() {
  try {
    scanner = new WebScanner(WS_URL);
  } catch (e) {
    scanner = null;
    console.error("WebScanner:", e);
  }
}

// --- built-in demo advertisers ---------------------------------------------
function startDemos() {
  if (advertisers.length) return;
  for (const d of DEMO_DEVICES) {
    const url = `ws://localhost:7681/v1/websocket/bt?name=${encodeURIComponent(d.node)}&address=${d.address}`;
    try {
      advertisers.push(new WebAdvertiser(url, d.name, d.service, d.company, new Uint8Array(d.data)));
    } catch (e) {
      console.error("WebAdvertiser:", d.name, e);
    }
  }
}

function stopDemos() {
  for (const a of advertisers) { try { a.free(); } catch (_) { /* gone */ } }
  advertisers = [];
  // Drop the demo cards so they don't linger as stale entries.
  for (const d of DEMO_DEVICES) devices.delete(d.address.toUpperCase());
}

function pumpDemos() {
  for (const a of advertisers) {
    try {
      if (a.ready_state() === 3) continue; // closed; loop will not revive it
      a.tick();
    } catch (e) {
      console.error("advertiser tick:", e);
    }
  }
}

// --- report aggregation + rendering ----------------------------------------
function merge(report) {
  const existing = devices.get(report.address) ?? {};
  const merged = { ...existing };
  merged.address = report.address;
  merged.address_type = report.address_type;
  merged.rssi = report.rssi;
  merged.lastSeen = performance.now();
  // Scan responses often carry only the name; regular reports carry the rest.
  // Merge instead of overwrite so both halves stay visible.
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
  const name = d.name
    ? escapeHtml(d.name)
    : '<span class="anon">(no name)</span>';
  const chips = [];
  if (d.connectable) chips.push("connectable");
  for (const uuid of d.service_uuids ?? []) chips.push(`svc ${uuid}`);
  if (d.tx_power !== undefined && d.tx_power !== null) chips.push(`tx ${d.tx_power} dBm`);
  const details = [];
  for (const sd of d.service_data ?? []) details.push(`service ${sd.tag}: ${sd.data}`);
  if (d.manufacturer_data)
    details.push(`mfg 0x${d.manufacturer_data.tag}: ${d.manufacturer_data.data}`);
  // RSSI bar: map the practical range (-100 dBm .. -30 dBm) onto 0..100%.
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
    : "No devices advertising — turn on demo devices above, run netsimd with --test-beacons, or open the scripted-device page in another tab.";
}

function loop() {
  if (!scanner) {
    const now = performance.now();
    if (now - lastConnectAttempt > 3000) {
      lastConnectAttempt = now;
      connect();
    }
    return;
  }
  const state = scanner.ready_state(); // 0 connecting 1 open 2 closing 3 closed
  if (state === 3) {
    // Never opened => connection refused (show setup instructions); a
    // mid-session drop just reconnects quietly.
    if (openedOnce) {
      setPill("connection lost — reconnecting…", "bad");
    } else {
      setPill("netsim not reachable", "bad");
      $("setup").classList.add("visible");
    }
    try { scanner.free(); } catch (_) { /* already gone */ }
    scanner = null; // next pass schedules a reconnect
    stopDemos();
    render();
    return;
  }
  if (state === 0) {
    setPill(openedOnce ? "reconnecting…" : "connecting to localhost:7681…", "");
    return;
  }
  openedOnce = true;
  $("setup").classList.remove("visible");
  if (demosOn) { startDemos(); pumpDemos(); }
  try {
    for (const report of JSON.parse(scanner.tick())) merge(report);
    setPill(`scanning · ${devices.size} device${devices.size === 1 ? "" : "s"}`, "ok");
  } catch (e) {
    console.error("tick:", e);
  }
  render();
}

await init();
$("demo-toggle").addEventListener("change", (e) => {
  demosOn = e.target.checked;
  if (!demosOn) stopDemos();
  render();
});
connect();
setInterval(loop, 250);
