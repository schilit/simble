// SimBLE Health domain: a heart-rate monitor, as a mountable domain module.
//
// Exports mount(root)/unmount() so the Devices shell can host it as a tab.
// The authoring half of the old scripted-device page -- editor, Run/Stop and
// the AI prompt -- is gone: that is the Playground's job, and this is a demo
// that starts itself. The script now comes straight from the library's
// default_heart_rate_script(), so the tab and MCP's `example` tool serve the
// same device.
//
// Original note:
// Simble scripted-device page glue: a running Simble (wasm) whose device is
// defined by the editable Rhai script. All Bluetooth logic is in Rust/wasm;
// this file only wires DOM <-> WebPeripheral and renders the live GATT
// database the script builds — whatever kind of device that is.

import init, { WebPeripheral, default_heart_rate_script } from "../pkg/simble.js";

const STYLE_ID = "simble-health-style";

const STYLE = `:root {
    --bg: #ffffff; --panel: #f6f8fa; --border: #d0d7de; --text: #1f2328;
    --dim: #656d76; --accent: #0969da; --good: #1a7f37; --bad: #cf222e;
    --warn: #9a6700; --heart: #cf222e;
  }
  * { box-sizing: border-box; }
  body {
    margin: 0; background: var(--bg); color: var(--text);
    font: 15px/1.5 -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
  }
  header {
    display: flex; align-items: baseline; gap: 1rem; flex-wrap: wrap;
    padding: 1rem 1.5rem; border-bottom: 1px solid var(--border);
  }
  header h1 { font-size: 1.15rem; margin: 0; }
  header .sub { color: var(--dim); font-size: 0.85rem; }
  header a { color: var(--accent); text-decoration: none; margin-left: auto; font-size: 0.85rem; }
  .pill {
    font-size: 0.75rem; padding: 0.1rem 0.6rem; border-radius: 1rem;
    border: 1px solid var(--border); color: var(--dim);
  }
  .pill.ok { color: var(--good); border-color: var(--good); }
  .pill.bad { color: var(--bad); border-color: var(--bad); }
  main {
    display: grid; grid-template-columns: minmax(24rem, 1fr) minmax(20rem, 1fr);
    gap: 1.25rem; padding: 1.25rem 1.5rem; max-width: 75rem; margin: 0 auto;
  }
  @media (max-width: 56rem) { main { grid-template-columns: 1fr; } }
  .panel {
    background: var(--panel); border: 1px solid var(--border);
    border-radius: 8px; padding: 1rem;
  }
  .panel h2 { font-size: 0.85rem; text-transform: uppercase; letter-spacing: 0.06em;
    color: var(--dim); margin: 0 0 0.75rem; }
  textarea#script {
    width: 100%; height: 26rem; resize: vertical; background: var(--bg);
    color: var(--text); border: 1px solid var(--border); border-radius: 6px;
    font: 12.5px/1.45 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    padding: 0.6rem; tab-size: 4;
  }
  button, .btn {
    background: #eaeef2; color: var(--text); border: 1px solid var(--border);
    border-radius: 6px; padding: 0.35rem 0.9rem; font-size: 0.85rem;
    cursor: pointer; text-decoration: none; display: inline-block;
  }
  button:hover, .btn:hover { border-color: var(--dim); }
  button.primary { background: #0969da; border-color: #0969da; color: #fff; }
  .row { display: flex; gap: 0.5rem; align-items: center; flex-wrap: wrap; margin-top: 0.6rem; }
  .hint { color: var(--dim); font-size: 0.8rem; }
  .error {
    color: var(--bad); font-family: ui-monospace, Menlo, monospace;
    font-size: 0.8rem; white-space: pre-wrap; margin-top: 0.5rem;
  }
  details { margin-top: 0.75rem; }
  details summary { cursor: pointer; color: var(--dim); font-size: 0.85rem; }
  details pre {
    max-height: 18rem; overflow: auto; background: var(--bg); border: 1px solid var(--border);
    border-radius: 6px; padding: 0.6rem; font-size: 11.5px; white-space: pre-wrap;
  }
  /* Suggestion chips that seed the AI request */
  .suggest { margin-top: 0.5rem; }
  .suggest .chip {
    display: inline-block; border: 1px solid var(--border); border-radius: 1rem;
    padding: 0.15rem 0.7rem; margin: 0.2rem 0.3rem 0 0; font-size: 0.78rem;
    cursor: pointer; background: #eaeef2; color: var(--text);
  }
  .suggest .chip:hover { border-color: var(--accent); }
  .suggest .chip.active { border-color: var(--accent); color: var(--accent); }
  #req-echo { margin-top: 0.45rem; font-size: 0.8rem; color: var(--dim); }
  #req-echo b { color: var(--text); font-weight: 500; }
  /* Heart (only shown for a Heart Rate Measurement characteristic) */
  .heart-box { text-align: center; padding: 0.5rem 0 0.75rem; }
  .heart {
    display: inline-block; font-size: 4rem; color: var(--heart);
    transform-origin: center; animation: beat 0.83s ease-in-out infinite;
  }
  .heart.flat { animation: none; opacity: 0.3; }
  @keyframes beat {
    0%, 100% { transform: scale(1); }
    12% { transform: scale(1.22); }
    24% { transform: scale(1); }
    36% { transform: scale(1.12); }
    50% { transform: scale(1); }
  }
  .bpm { font-size: 1.8rem; font-weight: 600; }
  .bpm small { color: var(--dim); font-size: 0.9rem; font-weight: 400; }
  /* Generic GATT viewer */
  .kv { display: grid; grid-template-columns: auto 1fr; gap: 0.15rem 0.9rem;
    font-size: 0.85rem; margin-bottom: 0.75rem; }
  .kv dt { color: var(--dim); } .kv dd { margin: 0; }
  .svc { border: 1px solid var(--border); border-radius: 8px; margin-bottom: 0.7rem;
    overflow: hidden; }
  .svc-head { background: #eaeef2; padding: 0.4rem 0.7rem; font-size: 0.82rem; }
  .svc-head .u { font-family: ui-monospace, Menlo, monospace; color: var(--dim);
    font-size: 0.76rem; margin-left: 0.4rem; }
  .chr { padding: 0.5rem 0.7rem; border-top: 1px solid var(--border); }
  .chr.pulse { animation: pulse 0.9s ease-out; }
  @keyframes pulse {
    0% { background: rgba(88,166,255,0.28); }
    100% { background: transparent; }
  }
  .chr-top { display: flex; align-items: baseline; gap: 0.5rem; flex-wrap: wrap; }
  .chr-name { font-weight: 500; }
  .chr-uuid { font-family: ui-monospace, Menlo, monospace; color: var(--dim); font-size: 0.75rem; }
  .prop {
    display: inline-block; border: 1px solid var(--border); border-radius: 4px;
    padding: 0 0.3rem; font-size: 0.68rem; color: var(--dim);
    font-family: ui-monospace, Menlo, monospace;
  }
  .prop.sub { color: var(--good); border-color: var(--good); }
  .chr-val { margin-top: 0.25rem; font-size: 0.9rem; }
  .chr-val .decoded { font-weight: 600; }
  .chr-val .raw { font-family: ui-monospace, Menlo, monospace; color: var(--dim);
    font-size: 0.76rem; margin-left: 0.4rem; }
  .sub-note { color: var(--good); font-size: 0.72rem; margin-left: 0.4rem; }
  #setup { display: none; grid-column: 1 / -1; border-color: var(--warn); }
  #setup.visible { display: block; }
  #setup pre {
    background: var(--bg); border: 1px solid var(--border); border-radius: 6px;
    padding: 0.6rem; overflow-x: auto; font-size: 0.8rem;
  }
  #setup code { color: var(--good); }
  #setup a { color: var(--accent); }
  footer { color: var(--dim); font-size: 0.78rem; padding: 0 1.5rem 1.5rem;
    max-width: 75rem; margin: 0 auto; }`;

const MARKUP = `<div class="domain-status"><span id="conn" class="pill">starting…</span></div>
<section id="setup" class="panel">
    <h2>netsim is not reachable</h2>
    <p>Could not reach netsim at <code>localhost:7681</code> — is <code>netsimd</code> running with its
       WebSocket frontend enabled? This page is served from the cloud, but the Bluetooth scene runs
       <strong>on your machine</strong>: the wasm build of SimBLE in this tab connects to a local
       <code>netsimd</code> over <code>ws://localhost:7681</code>. Start it with:</p>
    <pre><code>netsimd --logtostderr --no-shutdown --ws-port 7681</code></pre>
    <p class="hint">Needs the canary-channel emulator (see the README's <em>Testing Against netsim</em>
       section). Tip: open <a href="../scanner/">the scanner</a> in a second tab — both tabs join the
       same netsim scene. Edit the script here, press Run, and watch the scanner tab pick up the change.</p>
  </section>

  <section class="panel">
    <h2>Live device</h2>
    <dl class="kv">
      <dt>advertising as</dt><dd id="dev-name">—</dd>
      <dt>connection</dt><dd id="dev-conn">—</dd>
      <dt>subscription</dt><dd id="dev-sub">—</dd>
    </dl>
    <div id="hr-box" class="heart-box" hidden>
      <span id="heart" class="heart flat">❤</span>
      <div class="bpm"><span id="bpm">—</span> <small>bpm</small></div>
    </div>
    <div id="gatt"></div>
    <p class="hint">This viewer renders whatever GATT structure the running script builds — every
       service and characteristic, with properties, live values, and subscription state read from
       the wasm stack's real attribute database. A central on the scene can connect and subscribe
       (an emulator, or just watch the advertisement with the scanner tab).</p>
  </section>`;

const WS_URL =
  "ws://localhost:7681/v1/websocket/bt?name=web-device&address=CC:1E:57:00:00:02";

// ---------------------------------------------------------------------------
// The "Generate with AI" prompt. Teaches the exact scripting surface shipped
// in simble::scripting (bindings.rs / constants.rs) plus this page's web
// runtime conventions. The worked example builds a HEART-RATE MONITOR — on
// purpose a DIFFERENT device from this page's on-screen default (a
// thermometer), so pasting the AI's result yields something visibly different
// from what's already running.

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


// --- DOM handles -----------------------------------------------------------
const $ = (id) => document.getElementById(id);
let script = "";
let connPill, setupPanel;

let peripheral = null;
let runStart = performance.now();
let lastConnectAttempt = 0;
let openedOnce = false;
let stopped = true; // start stopped: the device runs only after Run is pressed
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
  // The authoring panel that held #script-error is gone; a script error here
  // is a library bug rather than something the viewer typed, so it goes to
  // the console and the status pill instead of a silent null dereference.
  const box = $("script-error");
  if (box) box.textContent = message ? String(message) : "";
  else if (message) {
    console.error("device script error:", message);
    setPill("device script error — see console", "bad");
  }
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
      peripheral.run_script(script); // same socket, new device
      runStart = performance.now();
    } else {
      createPeripheral(script);
    }
    prevValues.clear();
    setRunning(true);
    setPill("device rebuilt from script", "ok");
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
  setRunning(false);
  setPill("device stopped", "");
}

// Run is highlighted only when stopped (the call to action); Stop is enabled
// only while running.
function setRunning(on) {
  // The authoring controls are gone; kept as a hook so run()/stop() read the
  // same as they did rather than growing conditionals at every call site.
  $("run")?.classList.toggle("primary", !on);
  const stopBtn = $("stop");
  if (stopBtn) stopBtn.disabled = !on;
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
      try { createPeripheral(script); } catch (e) { showScriptError(e); }
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

// --- lifecycle -------------------------------------------------------------

let timer = null;
let container = null;

/// Builds the domain into `root` and starts it. Async because the wasm module
/// must be initialised before a device can exist.
export async function mount(root) {
  await init();

  if (!document.getElementById(STYLE_ID)) {
    const el = document.createElement("style");
    el.id = STYLE_ID;
    el.textContent = STYLE;
    document.head.append(el);
  }
  container = root;
  root.innerHTML = MARKUP;

  connPill = $("conn");
  setupPanel = $("setup");
  script = default_heart_rate_script();

  // Unlike the old authoring page, a domain tab starts itself -- there is no
  // Run button to press, because the script is not editable here.
  run();
  timer = setInterval(loop, 100);
}

/// Releases everything this domain owns. The shell hosts one domain at a
/// time, so anything left running becomes a leak -- and on netsim a device
/// whose socket drops without a disconnect lingers as a ghost at the same
/// address.
export function unmount() {
  if (timer !== null) {
    clearInterval(timer);
    timer = null;
  }
  try { stop(); } catch (_) { /* already down */ }
  document.getElementById(STYLE_ID)?.remove();
  if (container) container.innerHTML = "";
  container = null;
  connPill = setupPanel = undefined;
}
