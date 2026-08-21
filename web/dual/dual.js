// SimBLE Server + Client dual view. Two real devices share one in-page wasm
// Link: a scripted peripheral (server) and a central (client) that connects to
// it and discovers its GATT. The left panel renders the server's own database;
// the right panel renders what the client discovered — the nRF-Connect flow.

import init, { WebLink } from "../pkg/simble.js";
import { renderGatt, nameFor, propChips, escapeHtml } from "../common/viewer.js";

const $ = (id) => document.getElementById(id);
const SERVER_ADDR = "AA:BB:CC:00:00:01";
const CLIENT_ADDR = "AA:BB:CC:00:00:02";

const DEFAULT_SCRIPT = `// The server the client will connect to and discover.
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

let link = null;
let serverIndex = 0;
let clientIndex = 1;
let t0 = performance.now();
const prevValues = new Map();

function setPill(text, cls) {
  const pill = $("conn");
  pill.textContent = text;
  pill.className = "pill" + (cls ? " " + cls : "");
}

function build(script) {
  if (link) { try { link.free(); } catch (_) { /* gone */ } }
  link = new WebLink();
  serverIndex = link.add_peripheral(SERVER_ADDR, script);
  clientIndex = link.add_central(CLIENT_ADDR, SERVER_ADDR);
  t0 = performance.now();
  prevValues.clear();
  $("server-addr").textContent = SERVER_ADDR;
  $("client-addr").textContent = CLIENT_ADDR;
  $("client-gatt").innerHTML = '<p class="empty">Connecting…</p>';
}

// nRF-Connect-style discovered-GATT tree from the central's status JSON.
function renderClient(status) {
  $("client-phase").textContent = status.phase || "idle";
  $("client-peer").textContent = status.peer || "—";
  const el = $("client-gatt");
  if (!status.connected) { el.innerHTML = '<p class="empty">Connecting…</p>'; return; }
  if (!status.services || !status.services.length) {
    el.innerHTML = `<p class="empty">Connected — ${escapeHtml(status.phase || "discovering")}…</p>`;
    return;
  }
  el.innerHTML = status.services.map((s) => {
    const sName = nameFor(s.uuid);
    const head = sName
      ? `${escapeHtml(sName)}<span class="u">0x${s.uuid}</span>`
      : `<span class="u">Service 0x${s.uuid}</span>`;
    const chrs = (s.characteristics || []).map((c) => {
      const cName = nameFor(c.uuid);
      const nameHtml = cName
        ? `<span class="chr-name">${escapeHtml(cName)}</span><span class="chr-uuid">0x${c.uuid}</span>`
        : `<span class="chr-name chr-uuid">0x${c.uuid}</span>`;
      const handle = "0x" + c.value_handle.toString(16).padStart(4, "0");
      return `<div class="chr"><div class="chr-top">${nameHtml} ${propChips(c.properties, false)}
        <span class="chr-h">handle ${handle}</span></div></div>`;
    }).join("");
    return `<div class="svc"><div class="svc-head">${head}</div>${chrs}</div>`;
  }).join("");
}

function loop() {
  if (!link) return;
  link.tick((performance.now() - t0) / 1000);
  try {
    const server = JSON.parse(link.peripheral_status_json(serverIndex));
    renderGatt($("server-gatt"), server, prevValues);
  } catch (e) { console.error("server render:", e); }
  try {
    const client = JSON.parse(link.central_status_json(clientIndex));
    renderClient(client);
    setPill(client.connected ? `client · ${client.phase}` : "client connecting…", client.connected ? "ok" : "");
  } catch (e) { console.error("client render:", e); }
}

await init();
const editor = $("script");
editor.value = DEFAULT_SCRIPT;
$("run").addEventListener("click", () => {
  try {
    build(editor.value);
    $("run-state").textContent = "rebuilt — client reconnecting";
    setTimeout(() => ($("run-state").textContent = ""), 2500);
  } catch (e) {
    $("run-state").textContent = String(e);
  }
});
build(DEFAULT_SCRIPT);
setInterval(loop, 100);
