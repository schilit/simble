// Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
//
// SimBLE Playground glue: a free-form Rhai editor whose script IS the device.
// Reuses the same wasm engine (WebPeripheral) and live-viewer pattern as the
// scripted-device page, adds URL-sharing (deflate + base64url), an Examples
// menu of starter scripts, and the shared AI authoring aid.

import init, { WebPeripheral, default_heart_rate_script } from "../pkg/simble.js";
import { renderGatt } from "../common/viewer.js";
import { wireAi } from "../common/ai.js";
import { encodeScript, decodeScript } from "../common/share.js";
import { attachHighlightedEditor } from "../common/highlight.js";

const WS_URL =
  "ws://localhost:7681/v1/websocket/bt?name=web-playground&address=CC:1E:57:00:00:03";

// --- starter scripts (Examples menu) ---------------------------------------
const EXAMPLES = {
  "heart-rate": {
    label: "❤️ Heart-rate monitor",
    script: `// Heart-rate monitor: bpm breathes over time.
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
`,
  },
  battery: {
    label: "🔋 Battery monitor",
    script: `// Battery monitor: the level drains from 100 toward 5, then jumps back.
let server = android::BluetoothGattServer("web-battery");

let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
let level = android::BluetoothGattCharacteristic(
    uuid::BATTERY_LEVEL,
    android::PROPERTY_READ | android::PROPERTY_NOTIFY,
    android::PERMISSION_READ,
);
level.set_value([100]);
let cccd = android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE,
);
level.add_descriptor(cccd);
bas.add_characteristic(level);
server.add_service(bas);

fn tick(server, t) {
    // 95-second drain cycle from 100 down to 5, then recharge.
    let pct = 100 - ((t % 95.0) / 95.0 * 95.0).to_int();
    server.update_value(uuid::BATTERY_LEVEL, [pct]);
}
`,
  },
  thermometer: {
    label: "🌡 Thermometer (default)",
    script: default_heart_rate_script(),
  },
  misbehaving: {
    label: "⚠️ Misbehaving device",
    script: `// A deliberately misbehaving sensor: a Battery Level that lurches around
// and even reports impossible values above 100% — handy for seeing how a
// scanner / central copes with a badly-behaved peripheral. The viewer renders
// the raw byte regardless, so the misbehavior is visible.
let server = android::BluetoothGattServer("web-glitch");

let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
let level = android::BluetoothGattCharacteristic(
    uuid::BATTERY_LEVEL,
    android::PROPERTY_READ | android::PROPERTY_NOTIFY,
    android::PERMISSION_READ,
);
level.set_value([50]);
let cccd = android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE,
);
level.add_descriptor(cccd);
bas.add_characteristic(level);
server.add_service(bas);

fn tick(server, t) {
    // A jittery pseudo-random-looking value 0..255 derived from t (stateless).
    let n = ((sin(t * 5.7) + sin(t * 2.3) + 2.0) * 63.0).to_int();
    server.update_value(uuid::BATTERY_LEVEL, [n & 0xFF]);
}
`,
  },
  test: {
    label: "✅ Test with assert",
    script: `// A test-style script: build a device, then assert() its structure. assert
// throws a loud Rhai error on failure (shown in the error line); on success
// the device is built normally. This is the same primitive scenario tests use.
let server = android::BluetoothGattServer("web-selftest");

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

// --- assertions (run once, at build time) ---
assert(server.name == "web-selftest", "server keeps its name");
let svc = server.get_service(uuid::HEART_RATE_SERVICE);
assert(svc.characteristics.len() == 1, "service has one characteristic");
let chr = svc.get_characteristic(uuid::HEART_RATE_MEASUREMENT);
assert((chr.properties & android::PROPERTY_NOTIFY) != 0, "measurement is notify-capable");
`,
  },
};

// --- DOM handles -----------------------------------------------------------
const $ = (id) => document.getElementById(id);
const editor = $("script");
const connPill = $("conn");
const setupPanel = $("setup");

let peripheral = null;
let runStart = performance.now();
let lastConnectAttempt = 0;
let openedOnce = false;
const prevValues = new Map();

function setPill(text, cls) {
  connPill.textContent = text;
  connPill.className = "pill" + (cls ? " " + cls : "");
}

function log(msg) {
  $("log").textContent = msg;
}

function showScriptError(message) {
  $("script-error").textContent = message ? String(message) : "";
}

function createPeripheral(script) {
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
    prevValues.clear();
    log("Device rebuilt from script. Advertising as its declared name.");
  } catch (e) {
    showScriptError(e); // the previous device keeps running
    log("Script error — see below. The previous device (if any) keeps running.");
  }
}

// --- live rendering --------------------------------------------------------
function render(status) {
  $("dev-name").textContent = status.name
    ? `${status.name} (${status.address})`
    : "—";
  $("dev-conn").textContent = status.connected
    ? `connected to ${status.peer}`
    : "advertising, no central connected";
  const anySubscribed = (status.services || []).some((s) =>
    s.characteristics.some((c) => c.subscribed));
  $("dev-sub").textContent = anySubscribed
    ? "central subscribed — notifications flowing"
    : "no subscriber yet";
  renderGatt($("gatt"), status, prevValues);
  if (status.last_error) showScriptError(`tick error: ${status.last_error}`);
}

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
    if (openedOnce) {
      setPill("connection lost — reconnecting…", "bad");
    } else {
      setPill("netsim not reachable", "bad");
      setupPanel.classList.add("visible");
    }
    try { peripheral.free(); } catch (_) { /* already gone */ }
    peripheral = null;
    return;
  }
  if (state === 0) {
    setPill(openedOnce ? "reconnecting…" : "connecting to localhost:7681…", "");
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

// --- URL sharing -----------------------------------------------------------
async function share() {
  try {
    const query = await encodeScript(editor.value);
    const url = `${location.origin}${location.pathname}?${query}`;
    history.replaceState(null, "", url);
    await navigator.clipboard.writeText(url);
    log(`link copied — ${url.length} chars`);
  } catch (e) {
    log(`could not build share link: ${e}`);
  }
}

// --- examples menu ---------------------------------------------------------
function wireExamples() {
  const sel = $("examples");
  for (const [key, ex] of Object.entries(EXAMPLES)) {
    const opt = document.createElement("option");
    opt.value = key;
    opt.textContent = ex.label;
    sel.appendChild(opt);
  }
  sel.addEventListener("change", () => {
    const ex = EXAMPLES[sel.value];
    if (!ex) return;
    editor.value = ex.script;
    sel.value = "";
    log(`Loaded example: ${ex.label} — press Run.`);
    run();
  });
}

// --- boot ------------------------------------------------------------------
await init();

const shared = await decodeScript(location.search);
editor.value = shared || default_heart_rate_script();
attachHighlightedEditor(editor); // syntax highlighting overlay (degrades to plain)
if (shared) log("Loaded a shared script from the URL — press Run (or it runs on connect).");

$("run").addEventListener("click", run);
$("share").addEventListener("click", share);
$("reset").addEventListener("click", () => {
  editor.value = default_heart_rate_script();
  history.replaceState(null, "", location.pathname);
  showScriptError(null);
  prevValues.clear();
  log("Reset to the default script — press Run.");
  run();
});
wireExamples();
wireAi();
try { createPeripheral(editor.value); } catch (e) { showScriptError(e); }
setInterval(loop, 100);
