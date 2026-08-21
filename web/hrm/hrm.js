// Simble Web HRM page glue: a running Simble (wasm) whose device is defined
// by the editable Rhai script. All Bluetooth logic is in Rust/wasm; this file
// only wires DOM <-> WebPeripheral.

import init, { WebPeripheral, default_heart_rate_script } from "../pkg/simble.js";

const WS_URL =
  "ws://localhost:7681/v1/websocket/bt?name=web-hrm&address=CC:1E:57:00:00:02";

// ---------------------------------------------------------------------------
// The "Generate with AI" prompt. Teaches the exact scripting surface shipped
// in simble::scripting (bindings.rs / constants.rs) plus this page's web
// runtime conventions. The worked example is the page's default script shape,
// which is exercised by a native unit test in src/transport/wasm_ws.rs.
const AI_PROMPT = `You write Rhai scripts that define virtual Bluetooth LE peripherals for Simble (a Rust BLE simulator that runs the script in a web page). Reply with ONLY a Rhai script in a single code block — no explanations.

RHAI IS NOT RUST:
- \`let x = ...;\` declares everything: no types, no \`mut\`, no \`::new()\`.
- Constructors are plain calls of the type name: \`android::BluetoothGattServer("name")\`.
- Byte payloads are arrays of integers: \`[0x00, 72]\`. Strings use "double quotes". Comments use //.
- No imports, no crates. NO infinite loops, NO sleep, NO blocking waits — the script body runs ONCE to build the device.

RUNTIME MODEL (the web page hosts the device):
- The script body must create a server and keep it in a top-level variable:
    let server = android::BluetoothGattServer("my-device");
- Optionally define \`fn tick(server, t)\` — the page calls it ~10 times per second; \`t\` is seconds since the script was run (a float). IMPORTANT: Rhai functions are encapsulated and CANNOT see top-level variables — use only the \`server\` and \`t\` parameters, and keep tick stateless (derive everything from \`t\`: \`sin(t)\`, \`t % 5.0\`, \`(2.0*t).to_int()\`...).
- \`server.update_value(uuid, [bytes])\` (web-runtime extension) writes a characteristic's value into the live GATT database; the page automatically sends a real BLE notification to any subscribed central when the value changes. This is the preferred way to animate values from tick().
- Advertising (device name + 16-bit service UUIDs) is derived from the server you build and issued by the page — do not try to advertise from the script.

API SURFACE (all real, backed by Simble's GATT stack):
- android::BluetoothGattServer(name) -> server
- android::BluetoothGattService(uuid, android::SERVICE_TYPE_PRIMARY) -> svc
- android::BluetoothGattCharacteristic(uuid, properties, permissions) -> chr
- android::BluetoothGattDescriptor(uuid, permissions) -> desc
- chr.set_value([bytes]) / chr.get_value() / chr.value / chr.add_descriptor(desc)
- svc.add_characteristic(chr) / svc.get_characteristic(uuid)
- server.add_service(svc) / server.get_service(uuid) / server.name
- server.notify_characteristic_changed(device, chr, confirm) — needs a connected \`device\` taken from an event; in this web runtime prefer server.update_value.
- server.send_response(device, request_id, status, offset, value)
- take_events() or server.take_events() -> array of event maps {event, server, device, uuid, value, request_id, offset, status, mtu, response_needed}. Event kinds: "connected", "disconnected", "service_added", "characteristic_read", "characteristic_write", "descriptor_read", "descriptor_write", "notification_sent", "mtu_changed". Call inside tick() to react to peer writes.
- wait_for "connected" { /* \`event\` is bound here */ } — consumes queued events, ERRORS if none is pending; use in tests, not in tick().
- assert(condition, "message")

CONSTANTS:
- android::PROPERTY_READ, PROPERTY_WRITE, PROPERTY_WRITE_NO_RESPONSE, PROPERTY_NOTIFY, PROPERTY_INDICATE, PROPERTY_BROADCAST (combine with |)
- android::PERMISSION_READ, PERMISSION_WRITE (plus _ENCRYPTED / _MITM variants)
- android::SERVICE_TYPE_PRIMARY, SERVICE_TYPE_SECONDARY; android::GATT_SUCCESS, GATT_FAILURE
- uuid::HEART_RATE_SERVICE, uuid::HEART_RATE_MEASUREMENT, uuid::BODY_SENSOR_LOCATION, uuid::BATTERY_SERVICE, uuid::BATTERY_LEVEL, uuid::CLIENT_CHARACTERISTIC_CONFIGURATION, uuid::MANUFACTURER_NAME, uuid::MODEL_NUMBER, uuid::SERIAL_NUMBER (Device Information), and more.
- Any other UUID: uuid::of("2A38") for a 16-bit assigned number, or uuid::of("12345678-1234-5678-1234-56789abcdef0") for a custom 128-bit UUID.

RULES:
- Every notify-capable characteristic MUST attach a CCCD descriptor, or centrals cannot subscribe and the runtime will not notify:
    let cccd = android::BluetoothGattDescriptor(uuid::CLIENT_CHARACTERISTIC_CONFIGURATION, android::PERMISSION_READ | android::PERMISSION_WRITE);
    chr.add_descriptor(cccd);
- Standard encodings: Heart Rate Measurement = [flags, bpm] with flags 0x00 for an 8-bit bpm. Battery Level = one byte 0-100.

COMPLETE WORKED EXAMPLE (a heart-rate monitor whose bpm breathes over time):
\`\`\`rhai
let server = android::BluetoothGattServer("web-hrm");

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
\`\`\`

MY DEVICE REQUEST:
`;

// --- DOM handles -----------------------------------------------------------
const $ = (id) => document.getElementById(id);
const editor = $("script");
const connPill = $("conn");
const setupPanel = $("setup");

let peripheral = null;
let runStart = performance.now();
let lastConnectAttempt = 0;
let openedOnce = false;

function setPill(text, cls) {
  connPill.textContent = text;
  connPill.className = "pill" + (cls ? " " + cls : "");
}

function showScriptError(message) {
  $("script-error").textContent = message ? String(message) : "";
}

function createPeripheral(script) {
  // A fresh WebPeripheral opens a fresh WebSocket; used at page load and on
  // reconnect. Re-Runs on a live connection go through run_script() instead.
  if (peripheral) {
    try { peripheral.free(); } catch (_) { /* already gone */ }
  }
  peripheral = new WebPeripheral(WS_URL, script); // throws on script errors
  runStart = performance.now();
}

function run() {
  showScriptError(null);
  try {
    if (peripheral) {
      peripheral.run_script(editor.value); // same socket, new device
      runStart = performance.now();
    } else {
      createPeripheral(editor.value);
    }
    $("run-state").textContent = "device rebuilt from script";
    setTimeout(() => ($("run-state").textContent = ""), 2500);
  } catch (e) {
    showScriptError(e); // the previous device keeps running
  }
}

// --- rendering -------------------------------------------------------------
function bpmFromHex(hex) {
  // Heart Rate Measurement (0x2A37): flags byte, then u8 or LE u16 bpm.
  if (!hex || hex.length < 4) return null;
  const flags = parseInt(hex.slice(0, 2), 16);
  if (flags & 0x01) {
    if (hex.length < 6) return null;
    return parseInt(hex.slice(2, 4), 16) | (parseInt(hex.slice(4, 6), 16) << 8);
  }
  return parseInt(hex.slice(2, 4), 16);
}

function render(status) {
  $("dev-name").textContent = `${status.name} (${status.address})`;
  $("dev-conn").textContent = status.connected
    ? `connected to ${status.peer}`
    : "advertising, no central connected";
  const anySubscribed = status.services.some((s) =>
    s.characteristics.some((c) => c.subscribed));
  $("dev-sub").textContent = anySubscribed
    ? "central subscribed — notifications flowing"
    : "no subscriber yet";

  const rows = [];
  for (const service of status.services) {
    rows.push(`<tr><td class="tag" colspan="2">service ${service.uuid}</td></tr>`);
    for (const c of service.characteristics) {
      const sub = c.subscribed ? " ⚡" : "";
      rows.push(
        `<tr><td class="mono">${c.uuid}${sub}</td><td class="mono">${c.value || "—"}</td></tr>`
      );
    }
  }
  $("gatt-body").innerHTML = rows.join("");

  // Heart animation from the GATT database, not from page state: the script
  // owns the behavior, the UI just reflects the attribute value.
  const hrChar = status.services
    .flatMap((s) => s.characteristics)
    .find((c) => c.uuid === "2A37");
  const bpm = hrChar ? bpmFromHex(hrChar.value) : null;
  const heart = $("heart");
  if (bpm && bpm > 0) {
    $("bpm").textContent = bpm;
    heart.classList.remove("flat");
    heart.style.animationDuration = `${(60 / bpm).toFixed(3)}s`;
  } else {
    $("bpm").textContent = "—";
    heart.classList.add("flat");
  }

  if (status.last_error) showScriptError(`tick error: ${status.last_error}`);
}

// --- main loop -------------------------------------------------------------
function loop() {
  if (!peripheral) {
    const now = performance.now();
    if (now - lastConnectAttempt > 3000) {
      lastConnectAttempt = now;
      try { createPeripheral(editor.value); } catch (e) { showScriptError(e); }
    }
    return;
  }
  const state = peripheral.ready_state(); // 0 connecting 1 open 2 closing 3 closed
  if (state === 3) {
    setPill("netsimd not reachable", "bad");
    setupPanel.classList.add("visible");
    try { peripheral.free(); } catch (_) { /* already gone */ }
    peripheral = null; // the next loop pass schedules a reconnect
    return;
  }
  if (state === 0) {
    if (openedOnce) setPill("reconnecting…", "");
    else setPill("connecting to localhost:7681…", "");
    return;
  }
  openedOnce = true;
  setupPanel.classList.remove("visible");
  try {
    const status = JSON.parse(peripheral.tick((performance.now() - runStart) / 1000));
    setPill(status.connected ? "on air · central connected" : "on air · advertising", "ok");
    render(status);
  } catch (e) {
    showScriptError(e);
  }
}

// --- AI affordance ---------------------------------------------------------
function wireAi() {
  const encoded = encodeURIComponent(AI_PROMPT);
  $("ai-claude").href = `https://claude.ai/new?q=${encoded}`;
  $("ai-chatgpt").href = `https://chatgpt.com/?q=${encoded}`;
  $("ai-prompt-view").textContent = AI_PROMPT;
  const hint = (t) => { $("ai-hint").textContent = t; setTimeout(() => ($("ai-hint").textContent = ""), 4000); };
  $("ai-gemini").addEventListener("click", async () => {
    await navigator.clipboard.writeText(AI_PROMPT);
    window.open("https://gemini.google.com/app", "_blank", "noopener");
    hint("prompt copied — paste it into Gemini, add your device description, and send");
  });
  $("ai-copy").addEventListener("click", async () => {
    await navigator.clipboard.writeText(AI_PROMPT);
    hint("prompt copied — paste into any LLM, append your device description, then paste the returned Rhai here and press Run");
  });
}

// --- boot ------------------------------------------------------------------
await init();
editor.value = default_heart_rate_script();
$("run").addEventListener("click", run);
wireAi();
try { createPeripheral(editor.value); } catch (e) { showScriptError(e); }
setInterval(loop, 100);
