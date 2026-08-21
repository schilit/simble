// Simble Web Scanner page glue. All HCI/GAP work (scan bring-up, advertising
// report parsing, AD-structure decoding) happens in Rust compiled to wasm;
// this file only aggregates reports per address and renders DOM.

import init, { WebScanner } from "../pkg/simble.js";

const WS_URL =
  "ws://localhost:7681/v1/websocket/bt?name=web-scanner&address=CC:1E:57:00:00:01";

const $ = (id) => document.getElementById(id);
const devices = new Map(); // address -> merged report + lastSeen

let scanner = null;
let lastConnectAttempt = 0;
let openedOnce = false;

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
  $("empty").style.display = list.length ? "none" : "block";
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
    setPill("netsimd not reachable", "bad");
    $("setup").classList.add("visible");
    try { scanner.free(); } catch (_) { /* already gone */ }
    scanner = null; // next pass schedules a reconnect
    render();
    return;
  }
  if (state === 0) {
    setPill(openedOnce ? "reconnecting…" : "connecting to localhost:7681…", "");
    return;
  }
  openedOnce = true;
  $("setup").classList.remove("visible");
  try {
    for (const report of JSON.parse(scanner.tick())) merge(report);
    setPill(`scanning · ${devices.size} device${devices.size === 1 ? "" : "s"}`, "ok");
  } catch (e) {
    console.error("tick:", e);
  }
  render();
}

await init();
connect();
setInterval(loop, 250);
