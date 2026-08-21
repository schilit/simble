// Simble scripted-device page glue: a running Simble (wasm) whose device is
// defined by the editable Rhai script. All Bluetooth logic is in Rust/wasm;
// this file only wires DOM <-> WebPeripheral and renders the live GATT
// database the script builds — whatever kind of device that is.

import init, { WebPeripheral, default_heart_rate_script } from "../pkg/simble.js";

const WS_URL =
  "ws://localhost:7681/v1/websocket/bt?name=web-device&address=CC:1E:57:00:00:02";

// ---------------------------------------------------------------------------
// The "Generate with AI" prompt. Teaches the exact scripting surface shipped
// in simble::scripting (bindings.rs / constants.rs) plus this page's web
// runtime conventions. The worked example builds a HEART-RATE MONITOR — on
// purpose a DIFFERENT device from this page's on-screen default (a
// thermometer), so pasting the AI's result yields something visibly different
// from what's already running.
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
- Any other UUID: uuid::of("2A6E") for a 16-bit assigned number, or uuid::of("12345678-1234-5678-1234-56789abcdef0") for a custom 128-bit UUID. Use uuid::of for anything without a named constant (e.g. Environmental Sensing 181A, Temperature 2A6E, Humidity 2A6F, Cycling Speed and Cadence 1816 / CSC Measurement 2A5B).

RULES:
- Every notify-capable characteristic MUST attach a CCCD descriptor, or centrals cannot subscribe and the runtime will not notify:
    let cccd = android::BluetoothGattDescriptor(uuid::CLIENT_CHARACTERISTIC_CONFIGURATION, android::PERMISSION_READ | android::PERMISSION_WRITE);
    chr.add_descriptor(cccd);
- Standard encodings:
    Heart Rate Measurement (2A37) = [flags, bpm], flags 0x00 for an 8-bit bpm.
    Battery Level (2A19) = one byte, 0-100.
    Temperature (2A6E, Environmental Sensing) = signed 16-bit little-endian, hundredths of a degree C: 21.5C -> value 2150 -> [2150 & 0xFF, (2150 >> 8) & 0xFF].
    Humidity (2A6F) = unsigned 16-bit little-endian, hundredths of a percent.

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

// Clickable suggestions that seed the "MY DEVICE REQUEST:" line. Each is
// buildable with the bindings above (named uuid::* consts, or uuid::of for the
// rest). One is picked at random on load so a first-time visitor always has an
// interesting, non-default device to generate in a single click.
const SUGGESTIONS = [
  { label: "🔋 battery monitor",
    request: "a battery monitor: a Battery Service with a Battery Level characteristic (uuid::BATTERY_LEVEL, notify + a CCCD) whose percentage slowly drains from 100 toward 5 and then jumps back to full." },
  { label: "🚴 cycling speed sensor",
    request: "a cycling speed and cadence sensor: service uuid::of(\"1816\") with a CSC Measurement characteristic uuid::of(\"2A5B\") (notify + a CCCD) whose cumulative wheel revolutions increase steadily over time." },
  { label: "💡 RGB smart light",
    request: "an RGB smart light: a custom 128-bit service via uuid::of(\"f0000001-1234-5678-1234-56789abcdef0\") with a writable+notify color characteristic holding [R, G, B] bytes that cycle through the rainbow over time." },
  { label: "❤️ heart-rate monitor",
    request: "a heart-rate monitor whose bpm rises and falls like exercise intervals (uuid::HEART_RATE_MEASUREMENT, notify + a CCCD, payload [0x00, bpm])." },
  { label: "🌫 humidity sensor",
    request: "a humidity sensor: Environmental Sensing service uuid::of(\"181A\") with a Humidity characteristic uuid::of(\"2A6F\") (notify + a CCCD), an unsigned 16-bit little-endian value in hundredths of a percent drifting around 45%." },
  { label: "🎲 surprise me",
    request: "a surprising, fun made-up BLE device of your choice — pick something delightful and make its values animate over time." },
];

let currentRequest = "";

// --- DOM handles -----------------------------------------------------------
const $ = (id) => document.getElementById(id);
const editor = $("script");
const connPill = $("conn");
const setupPanel = $("setup");

let peripheral = null;
let runStart = performance.now();
let lastConnectAttempt = 0;
let openedOnce = false;
let stopped = false; // Stop pressed: hold the device torn down, don't auto-reconnect
const prevValues = new Map(); // "service/char" -> last value hex, for the pulse

// Flicker guard: the GATT structure is rebuilt only when it actually changes
// (keyed by this signature); every other tick updates values in place, so the
// viewer isn't torn down and repainted 10×/second.
let renderedSig = null;
const valNodes = new Map(); // "service/char" -> the .chr-val element to update
let lastBpm = null;         // avoid restarting the heartbeat animation each tick

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
  stopped = false;
  try {
    if (peripheral) {
      peripheral.run_script(editor.value); // same socket, new device
      runStart = performance.now();
    } else {
      createPeripheral(editor.value);
    }
    prevValues.clear();
    setStopEnabled(true);
    $("run-state").textContent = "device rebuilt from script";
    setTimeout(() => ($("run-state").textContent = ""), 2500);
  } catch (e) {
    showScriptError(e); // the previous device keeps running
  }
}

// Tear the device down and hold it off the air until Run is pressed again.
function stop() {
  stopped = true;
  if (peripheral) {
    try { peripheral.free(); } catch (_) { /* already gone */ }
    peripheral = null;
  }
  renderedSig = null;
  valNodes.clear();
  prevValues.clear();
  lastBpm = null;
  $("gatt").innerHTML = "";
  $("dev-conn").textContent = "stopped";
  $("dev-sub").textContent = "—";
  $("hr-box").hidden = true;
  setupPanel.classList.remove("visible");
  setPill("stopped", "");
  setStopEnabled(false);
  $("run-state").textContent = "device stopped";
  setTimeout(() => ($("run-state").textContent = ""), 2500);
}

function setStopEnabled(on) {
  const btn = $("stop");
  if (btn) btn.disabled = !on;
}

// --- decoding helpers ------------------------------------------------------
const escapeHtml = (s) =>
  String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

// A small assigned-number -> friendly-name table for the viewer. Keys are the
// uppercase 16-bit hex forms Simble emits (uuid.to_string()).
const UUID_NAMES = {
  "180D": "Heart Rate", "2A37": "Heart Rate Measurement", "2A38": "Body Sensor Location",
  "180F": "Battery", "2A19": "Battery Level",
  "181A": "Environmental Sensing", "2A6E": "Temperature", "2A6F": "Humidity",
  "1809": "Health Thermometer", "2A1C": "Temperature Measurement",
  "1816": "Cycling Speed and Cadence", "2A5B": "CSC Measurement", "2A5C": "CSC Feature",
  "180A": "Device Information", "2A29": "Manufacturer Name", "2A24": "Model Number",
  "2A25": "Serial Number", "2A26": "Firmware Revision",
  "1800": "Generic Access", "2A00": "Device Name", "1801": "Generic Attribute",
  "2902": "Client Characteristic Configuration",
};
const nameFor = (uuid) => UUID_NAMES[uuid] || null;

function bytesFromHex(hex) {
  const out = [];
  for (let i = 0; i + 1 < hex.length; i += 2) out.push(parseInt(hex.slice(i, i + 2), 16));
  return out;
}

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

// Returns a human string for a known characteristic type, or null to fall back
// to hex / auto text.
function decodeValue(uuid, hex) {
  if (!hex) return null;
  const b = bytesFromHex(hex);
  switch (uuid) {
    case "2A37": { const bpm = bpmFromHex(hex); return bpm == null ? null : `${bpm} bpm`; }
    case "2A19": return b.length ? `${b[0]}%` : null;
    case "2A38": return b.length ? bodySensorLocation(b[0]) : null;
    case "2A6E": { // Temperature, sint16 LE, 0.01 C
      if (b.length < 2) return null;
      let v = b[0] | (b[1] << 8); if (v & 0x8000) v -= 0x10000;
      return `${(v / 100).toFixed(2)} °C`;
    }
    case "2A6F": { // Humidity, uint16 LE, 0.01 %
      if (b.length < 2) return null;
      return `${((b[0] | (b[1] << 8)) / 100).toFixed(1)} %`;
    }
    default: return autoText(b);
  }
}

function bodySensorLocation(n) {
  return ["Other", "Chest", "Wrist", "Finger", "Hand", "Ear Lobe", "Foot"][n] ?? `location ${n}`;
}

// If every byte is printable ASCII, show it as text (manufacturer/model names).
function autoText(bytes) {
  if (!bytes.length) return null;
  if (bytes.every((c) => c >= 0x20 && c <= 0x7e)) {
    return `"${String.fromCharCode(...bytes)}"`;
  }
  return null;
}

function propChips(props, subscribed) {
  const chips = [];
  if (props & 0x02) chips.push("R");
  if (props & (0x08 | 0x04)) chips.push("W");
  if (props & 0x10) chips.push("N");
  if (props & 0x20) chips.push("I");
  if (props & 0x01) chips.push("B");
  return chips
    .map((c) => `<span class="prop${subscribed && (c === "N" || c === "I") ? " sub" : ""}">${c}</span>`)
    .join(" ");
}

// --- rendering -------------------------------------------------------------
// The inner HTML of a characteristic's value cell (decoded + raw hex).
function valInnerHtml(c) {
  const decoded = decodeValue(c.uuid, c.value);
  return c.value
    ? `${decoded ? `<span class="decoded">${escapeHtml(decoded)}</span>` : ""}<span class="raw">${c.value}</span>`
    : `<span class="raw">—</span>`;
}

// A signature of the GATT *structure* (services, characteristics, properties,
// subscription state) — everything except the fast-changing values. The DOM is
// rebuilt only when this changes; values update in place otherwise.
function structureSig(status) {
  return JSON.stringify(status.services.map((s) => [
    s.uuid,
    s.characteristics.map((c) => [c.uuid, c.properties, c.subscribed]),
  ]));
}

function buildGatt(status) {
  const cards = [];
  for (const service of status.services) {
    const sName = nameFor(service.uuid);
    const head = sName
      ? `${escapeHtml(sName)}<span class="u">0x${service.uuid}</span>`
      : `<span class="u">Service 0x${service.uuid}</span>`;
    const rows = [];
    for (const c of service.characteristics) {
      const key = `${service.uuid}/${c.uuid}`;
      const cName = nameFor(c.uuid);
      const nameHtml = cName
        ? `<span class="chr-name">${escapeHtml(cName)}</span><span class="chr-uuid">0x${c.uuid}</span>`
        : `<span class="chr-name chr-uuid">0x${c.uuid}</span>`;
      const subNote = c.subscribed ? `<span class="sub-note">⚡ subscribed</span>` : "";
      rows.push(
        `<div class="chr" data-key="${key}">
          <div class="chr-top">${nameHtml} ${propChips(c.properties, c.subscribed)}${subNote}</div>
          <div class="chr-val">${valInnerHtml(c)}</div>
        </div>`
      );
    }
    cards.push(`<div class="svc"><div class="svc-head">${head}</div>${rows.join("")}</div>`);
  }
  $("gatt").innerHTML = cards.join("");
  // Cache the value cells so later ticks patch text without touching structure.
  valNodes.clear();
  for (const el of $("gatt").querySelectorAll(".chr")) {
    valNodes.set(el.dataset.key, el.querySelector(".chr-val"));
  }
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

  // Rebuild the DOM only when the structure changes; a full innerHTML rewrite
  // every tick is what made the page flicker.
  const sig = structureSig(status);
  const rebuilt = sig !== renderedSig;
  if (rebuilt) {
    buildGatt(status);
    renderedSig = sig;
  }

  // Patch changed values in place, and note the most-recently-changed one for
  // the generic pulse (the heart animation is the HR special case).
  let changedKey = null;
  for (const service of status.services) {
    for (const c of service.characteristics) {
      const key = `${service.uuid}/${c.uuid}`;
      if (prevValues.has(key) && prevValues.get(key) !== c.value) {
        changedKey = key;
        const cell = valNodes.get(key);
        if (cell) cell.innerHTML = valInnerHtml(c);
      }
      prevValues.set(key, c.value);
    }
  }

  const hrChar = status.services
    .flatMap((s) => s.characteristics)
    .find((c) => c.uuid === "2A37");
  const hrBox = $("hr-box");
  if (hrChar) {
    hrBox.hidden = false;
    const bpm = bpmFromHex(hrChar.value);
    const heart = $("heart");
    if (bpm && bpm > 0) {
      $("bpm").textContent = bpm;
      heart.classList.remove("flat");
      // Only reset the animation when the rate actually changes, otherwise the
      // heartbeat restarts every tick and stutters.
      if (bpm !== lastBpm) heart.style.animationDuration = `${(60 / bpm).toFixed(3)}s`;
      lastBpm = bpm;
    } else {
      $("bpm").textContent = "—";
      heart.classList.add("flat");
      lastBpm = null;
    }
  } else {
    hrBox.hidden = true;
    // Don't pulse on the same tick the row was just created, or it flashes on load.
    if (changedKey && !rebuilt) {
      const el = document.querySelector(`.chr[data-key="${CSS.escape(changedKey)}"]`);
      if (el) { el.classList.remove("pulse"); void el.offsetWidth; el.classList.add("pulse"); }
    }
  }

  if (status.last_error) showScriptError(`tick error: ${status.last_error}`);
}

// --- main loop -------------------------------------------------------------
function loop() {
  if (stopped) return; // Stop pressed: stay torn down until Run
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
    // Distinguish "never reached netsim" (connection refused) from a
    // mid-session drop: only the former shows the setup instructions.
    if (openedOnce) {
      setPill("connection lost — reconnecting…", "bad");
    } else {
      setPill("netsim not reachable", "bad");
      setupPanel.classList.add("visible");
    }
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
function effectivePrompt() {
  return AI_PROMPT + (currentRequest ? currentRequest + "\n" : "");
}

function refreshAi() {
  const encoded = encodeURIComponent(effectivePrompt());
  $("ai-claude").href = `https://claude.ai/new?q=${encoded}`;
  $("ai-chatgpt").href = `https://chatgpt.com/?q=${encoded}`;
  $("ai-prompt-view").textContent = effectivePrompt();
  $("req-echo").innerHTML = currentRequest
    ? `Request: <b>${escapeHtml(currentRequest)}</b>`
    : "Pick a suggestion above, or type your own after “MY DEVICE REQUEST:”.";
}

function setRequest(request, chipEl) {
  currentRequest = request;
  for (const el of document.querySelectorAll("#suggest .chip")) el.classList.remove("active");
  if (chipEl) chipEl.classList.add("active");
  refreshAi();
}

function wireAi() {
  const suggest = $("suggest");
  suggest.innerHTML = SUGGESTIONS
    .map((s, i) => `<span class="chip" data-i="${i}">${escapeHtml(s.label)}</span>`)
    .join("");
  for (const chip of suggest.querySelectorAll(".chip")) {
    chip.addEventListener("click", () =>
      setRequest(SUGGESTIONS[+chip.dataset.i].request, chip));
  }
  // Rotating seed: a random suggestion is pre-filled so the prompt is
  // immediately useful, and it's a non-default device type.
  const seed = Math.floor(Math.random() * SUGGESTIONS.length);
  setRequest(SUGGESTIONS[seed].request, suggest.querySelector(`.chip[data-i="${seed}"]`));

  const hint = (t) => { $("ai-hint").textContent = t; setTimeout(() => ($("ai-hint").textContent = ""), 4000); };
  $("ai-gemini").addEventListener("click", async () => {
    await navigator.clipboard.writeText(effectivePrompt());
    window.open("https://gemini.google.com/app", "_blank", "noopener");
    hint("prompt copied — paste it into Gemini and send");
  });
  $("ai-copy").addEventListener("click", async () => {
    await navigator.clipboard.writeText(effectivePrompt());
    hint("prompt copied — paste into any LLM, then paste the returned Rhai here and press Run");
  });
}

// --- boot ------------------------------------------------------------------
await init();
editor.value = default_heart_rate_script();
$("run").addEventListener("click", run);
$("stop").addEventListener("click", stop);
wireAi();
try { createPeripheral(editor.value); } catch (e) { showScriptError(e); }
setInterval(loop, 100);
