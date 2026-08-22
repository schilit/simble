// Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
//
// SimBLE Home domain: a colour bulb, as a mountable domain module.
//
// Exports mount(root)/unmount() so the Devices shell can host it as a tab and
// tear it down on switch. The markup and styles live here rather than in an
// index.html, because a mounted domain is injected into the shell's stage --
// index.html is now only a thin standalone entry point.
//
// A curated visual demo (like the scripted-device page's beating heart, but
// for a light). A Rhai-scripted peripheral exposes a
// writable [R, G, B] "color" characteristic on a custom 128-bit service; the
// page renders a glowing bulb whose color reflects the characteristic's live
// value, and a color picker writes new values into the device's own GATT
// database via the host set_value path — so the bulb changes here AND a
// connected central is notified. The script is the device; the page supplies
// the "write" a phone app would send (central-role scripting doesn't exist yet).

import init, { WebPeripheral, WebLink } from "../pkg/simble.js";
import { renderGatt } from "../common/viewer.js";
import { attachHighlightedEditor } from "../common/highlight.js";
import { createBackendSelector } from "../common/backend.js";

const STYLE_ID = "simble-home-style";

const STYLE = `main { display: grid; grid-template-columns: minmax(20rem, 1fr) minmax(18rem, 1fr);
    gap: 1.25rem; padding: 1.25rem 1.5rem; max-width: 72rem; margin: 0 auto; }
  @media (max-width: 56rem) { main { grid-template-columns: 1fr; } }
  .bulb-stage { display: flex; flex-direction: column; align-items: center;
    padding: 1rem 0 0.5rem; }
  #bulbSvg { width: 190px; height: auto; transition: filter 0.25s; }
  #glass { transition: fill 0.25s; }
  .swatches { display: flex; gap: 0.5rem; flex-wrap: wrap; justify-content: center;
    margin-top: 0.8rem; }
  .swatch { width: 2rem; height: 2rem; border-radius: 50%; border: 2px solid var(--border);
    cursor: pointer; }
  .swatch:hover { border-color: var(--text); }
  .picker-row { display: flex; align-items: center; gap: 0.8rem; justify-content: center;
    margin-top: 0.8rem; flex-wrap: wrap; }
  input[type=color] { width: 3rem; height: 2rem; border: 1px solid var(--border);
    border-radius: 6px; background: var(--panel); cursor: pointer; }
  .rgb-readout { font-family: ui-monospace, Menlo, monospace; color: var(--dim);
    font-size: 0.85rem; }`;

const MARKUP = `<div id="backend" style="grid-column:1/-1"></div>
  <section class="panel">
    <h2>The light</h2>
    <div class="bulb-stage">
      <svg id="bulbSvg" viewBox="0 0 120 170" role="img" aria-label="color bulb">
        <g>
          <path id="glass" d="M60 10 C25 10 8 38 20 68 C26 84 40 92 42 112 L78 112 C80 92 94 84 100 68 C112 38 95 10 60 10 Z" fill="#33ccff"/>
          <path d="M40 55 Q50 40 60 55 Q70 70 80 55" stroke="rgba(255,255,255,0.55)" stroke-width="3" fill="none"/>
        </g>
        <rect x="44" y="112" width="32" height="10" rx="2" fill="#656d76"/>
        <rect x="46" y="124" width="28" height="8" rx="2" fill="#6e7681"/>
        <rect x="48" y="134" width="24" height="7" rx="2" fill="#57606a"/>
        <path d="M52 148 Q60 158 68 148" stroke="#444c56" fill="none" stroke-width="3"/>
      </svg>
      <div class="picker-row">
        <label for="picker">Pick a color:</label>
        <input type="color" id="picker" value="#33ccff">
        <span class="rgb-readout" id="rgb">RGB 33,204,255</span>
      </div>
      <div class="swatches" id="swatches"></div>
    </div>
    <p class="hint" style="margin-top:1rem">
      The bulb color is a writable <code>[R, G, B]</code> characteristic on a custom 128-bit service
      (Magic-Blue-style). The picker writes the value into the device's live GATT database via the
      host <code>set_value</code> path — the same one the script's <code>update_value</code> uses — so
      you see the bulb change here <strong>and</strong> any connected central is notified of the new
      color. (The script is the peripheral; central-role scripting doesn't exist yet, so the page
      supplies the "write" a phone app would send.)
    </p>
    <p class="hint" id="mode-hint" style="margin-top:0.5rem"></p>
    <div id="script-error" class="error"></div>
  </section>

  <section class="panel">
    <h2>Live device</h2>
    <dl class="kv">
      <dt>advertising as</dt><dd id="dev-name">—</dd>
      <dt>connection</dt><dd id="dev-conn">—</dd>
      <dt>subscription</dt><dd id="dev-sub">—</dd>
    </dl>
    <div id="gatt"></div>
    <details>
      <summary>view / edit the device script (Rhai)</summary>
      <textarea id="script" class="code" spellcheck="false" style="height:16rem;margin-top:0.5rem"></textarea>
      <div class="row"><button id="run" class="primary">▶ Run</button>
        <span id="run-state" class="hint"></span></div>
    </details>
  </section>

  <section id="setup" class="panel setup" style="grid-column:1/-1">
    <h2>netsim is not reachable</h2>
    <p>Could not reach netsim at <code>localhost:7681</code> — is <code>netsimd</code> running with its
       WebSocket frontend enabled? This page is served from the cloud, but the Bluetooth scene runs
       <strong>on your machine</strong>: the wasm build of SimBLE in this tab connects to a local
       <code>netsimd</code> over <code>ws://localhost:7681</code>. Start it with:</p>
    <pre><code>netsimd --logtostderr --no-shutdown --ws-port 7681</code></pre>
    <p class="hint">Needs the canary-channel emulator. The bulb still works offline — the color you
       pick is written into the in-browser device; connect netsim to expose it on the scene and notify a
       real central.</p>
  </section>`;

const IN_PAGE_ADDR = "CC:1E:57:00:00:05";
const WS_URL =
  "ws://localhost:7681/v1/websocket/bt?name=web-lightbulb&address=CC:1E:57:00:00:05";

// Custom 128-bit UUIDs (Magic-Blue-style). The characteristic value is 3 bytes
// [R, G, B]. Simble renders 128-bit UUIDs lowercase-dashed, which is what
// status_json reports and what uuid::of parses — so this string round-trips.
const COLOR_SERVICE = "f0ff0001-1234-5678-90ab-cdef01234567";
const COLOR_CHAR = "f0ff0002-1234-5678-90ab-cdef01234567";

const DEFAULT_SCRIPT = `// SimBLE Color Bulb — a Magic-Blue-style RGB light.
// A writable [R, G, B] color characteristic on a custom 128-bit service. The
// page's color picker writes this value (host glue), and a connected central
// is notified. No tick() needed — the color is driven by writes, not by time.
let server = android::BluetoothGattServer("web-lightbulb");

let svc = android::BluetoothGattService(
    uuid::of("${COLOR_SERVICE}"),
    android::SERVICE_TYPE_PRIMARY,
);
let color = android::BluetoothGattCharacteristic(
    uuid::of("${COLOR_CHAR}"),
    android::PROPERTY_READ | android::PROPERTY_WRITE
        | android::PROPERTY_WRITE_NO_RESPONSE | android::PROPERTY_NOTIFY,
    android::PERMISSION_READ | android::PERMISSION_WRITE,
);
color.set_value([0x33, 0xCC, 0xFF]); // a cool cyan to start
let cccd = android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE,
);
color.add_descriptor(cccd);
svc.add_characteristic(color);
server.add_service(svc);
`;

const PRESETS = ["#ff3355", "#ff9933", "#ffee33", "#33dd66", "#33ccff", "#7755ff", "#ff66cc", "#ffffff"];

// --- DOM -------------------------------------------------------------------
const $ = (id) => document.getElementById(id);
let editor, connPill, setupPanel, picker;

let mode = "in-page"; // "in-page" (a wasm WebLink in this tab) | "websocket" (netsim)
let peripheral = null; // WebPeripheral, WebSocket backend only
let link = null; // WebLink, in-page backend only
let linkIndex = -1; // peripheral index within the in-page link
let runStart = performance.now();
let lastConnectAttempt = 0;
let openedOnce = false;
const prevValues = new Map();

function setPill(text, cls) {
  connPill.textContent = text;
  connPill.className = "pill" + (cls ? " " + cls : "");
}
function showScriptError(m) { $("script-error").textContent = m ? String(m) : ""; }

function createPeripheral(script) {
  if (peripheral) { try { peripheral.free(); } catch (_) { /* gone */ } }
  peripheral = new WebPeripheral(WS_URL, script);
  runStart = performance.now();
}

// In-page backend: host the bulb on a wasm WebLink in this tab — no netsim.
// A WebLink has no "remove peripheral", so re-running rebuilds the whole link
// from a fresh script; the new link only replaces the old once the script
// parses. Color picks write live via peripheral_set_value (see writeColor).
function buildInPage(script) {
  const next = new WebLink();
  let idx;
  try { idx = next.add_peripheral(IN_PAGE_ADDR, script); }
  catch (e) { try { next.free(); } catch (_) { /* gone */ } throw e; }
  if (link) { try { link.free(); } catch (_) { /* gone */ } }
  link = next;
  linkIndex = idx;
  runStart = performance.now();
}

function teardownDevices() {
  if (peripheral) { try { peripheral.free(); } catch (_) { /* gone */ } peripheral = null; }
  if (link) { try { link.free(); } catch (_) { /* gone */ } link = null; linkIndex = -1; }
}

function run() {
  showScriptError(null);
  try {
    if (mode === "in-page") buildInPage(editor.value);
    else if (peripheral) { peripheral.run_script(editor.value); runStart = performance.now(); }
    else createPeripheral(editor.value);
    prevValues.clear();
    $("run-state").textContent = "device rebuilt from script";
    setTimeout(() => ($("run-state").textContent = ""), 2500);
  } catch (e) { showScriptError(e); }
}

// --- color helpers ---------------------------------------------------------
const clamp = (n) => Math.max(0, Math.min(255, n | 0));
function hexToRgb(hex) {
  const m = /^#?([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(hex.trim());
  if (!m) return null;
  return [parseInt(m[1], 16), parseInt(m[2], 16), parseInt(m[3], 16)];
}
const rgbToHex = (r, g, b) =>
  "#" + [r, g, b].map((n) => clamp(n).toString(16).padStart(2, "0")).join("");

function applyBulb(r, g, b) {
  const css = `rgb(${r},${g},${b})`;
  $("glass").setAttribute("fill", css);
  $("bulbSvg").style.filter = `drop-shadow(0 0 26px ${css}) drop-shadow(0 0 10px ${css})`;
  $("rgb").textContent = `RGB ${r},${g},${b}`;
}

// Writes the picked color into the device's live GATT database (host glue), and
// a subscribed central is notified. Both backends now write live: WebSocket via
// WebPeripheral.set_value, in-page via WebLink.peripheral_set_value.
function writeColor(r, g, b) {
  const bytes = new Uint8Array([clamp(r), clamp(g), clamp(b)]);
  try {
    if (mode === "in-page") {
      if (link && linkIndex >= 0) link.peripheral_set_value(linkIndex, COLOR_CHAR, bytes);
    } else if (peripheral) {
      peripheral.set_value(COLOR_CHAR, bytes);
    }
  } catch (e) {
    showScriptError(e);
  }
}

// --- rendering -------------------------------------------------------------
function render(status) {
  $("dev-name").textContent = status.name ? `${status.name} (${status.address})` : "—";
  $("dev-conn").textContent = status.connected
    ? `connected to ${status.peer}` : "advertising, no central connected";
  const anySub = (status.services || []).some((s) => s.characteristics.some((c) => c.subscribed));
  $("dev-sub").textContent = anySub ? "central subscribed — notifications flowing" : "no subscriber yet";

  renderGatt($("gatt"), status, prevValues);

  // Reflect the characteristic's current value onto the bulb + picker.
  const colorChar = (status.services || [])
    .flatMap((s) => s.characteristics)
    .find((c) => c.uuid === COLOR_CHAR);
  if (colorChar && colorChar.value && colorChar.value.length >= 6) {
    const r = parseInt(colorChar.value.slice(0, 2), 16);
    const g = parseInt(colorChar.value.slice(2, 4), 16);
    const b = parseInt(colorChar.value.slice(4, 6), 16);
    applyBulb(r, g, b);
    // Keep the picker in sync unless the user is actively holding it.
    if (document.activeElement !== picker) picker.value = rgbToHex(r, g, b);
  }
  if (status.last_error) showScriptError(`tick error: ${status.last_error}`);
}

function loop() {
  if (mode === "in-page") {
    if (!link || linkIndex < 0) {
      try { buildInPage(editor.value); } catch (e) { showScriptError(e); }
      return;
    }
    try {
      link.tick((performance.now() - runStart) / 1000);
      const json = link.peripheral_status_json(linkIndex);
      if (json) {
        setPill("in browser · advertising", "ok");
        render(JSON.parse(json));
      }
    } catch (e) { showScriptError(e); }
    return;
  }
  if (!peripheral) {
    const now = performance.now();
    if (now - lastConnectAttempt > 3000) {
      lastConnectAttempt = now;
      try { createPeripheral(editor.value); } catch (e) { showScriptError(e); }
    }
    return;
  }
  const state = peripheral.ready_state();
  if (state === 3) {
    if (openedOnce) setPill("connection lost — reconnecting…", "bad");
    else { setPill("netsim not reachable", "bad"); setupPanel.classList.add("visible"); }
    try { peripheral.free(); } catch (_) { /* gone */ }
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
  } catch (e) { showScriptError(e); }
}

// --- lifecycle -------------------------------------------------------------

let timer = null;
let container = null;

/// Builds the domain into `root` and starts it. Async because the wasm module
/// has to be initialised before any device exists.
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

  editor = $("script");
  connPill = $("conn");
  setupPanel = $("setup");
  picker = $("picker");

  editor.value = DEFAULT_SCRIPT;
  attachHighlightedEditor(editor); // syntax highlighting overlay (degrades to plain)

  // swatches
  const sw = $("swatches");
  for (const hex of PRESETS) {
    const el = document.createElement("div");
    el.className = "swatch";
    el.style.background = hex;
    el.title = hex;
    el.addEventListener("click", () => {
      picker.value = hex;
      const [r, g, b] = hexToRgb(hex);
      applyBulb(r, g, b);
      writeColor(r, g, b);
    });
    sw.appendChild(el);
  }

  picker.addEventListener("input", () => {
    const rgb = hexToRgb(picker.value);
    if (!rgb) return;
    applyBulb(...rgb);
    writeColor(...rgb);
  });

  $("run").addEventListener("click", run);

  const initial = hexToRgb(picker.value);
  if (initial) applyBulb(...initial);

  // Controller backend: "in-page" (a wasm WebLink in this tab, no netsim) or
  // "websocket" (a real netsim scene). Both write the color live into the GATT
  // database and notify subscribers; websocket also puts the bulb on the air for
  // a real central (e.g. the emulator) to connect and subscribe.
  function setModeHint() {
    $("mode-hint").textContent = mode === "in-page"
      ? "In-browser controller — no netsim; the bulb runs entirely in this tab."
      : "";
  }
  function switchBackend() {
    teardownDevices();
    openedOnce = false;
    setupPanel.classList.remove("visible");
    setModeHint();
    if (mode === "in-page") {
      try { buildInPage(editor.value); } catch (e) { showScriptError(e); }
    } else {
      setPill("starting…", "");
      try { createPeripheral(editor.value); } catch (e) { showScriptError(e); }
    }
  }
  mode = createBackendSelector($("backend"), {
    onChange: (m) => { mode = m; switchBackend(); },
  });
  setModeHint();

  if (mode === "in-page") {
    try { buildInPage(editor.value); } catch (e) { showScriptError(e); }
  } else {
    try { createPeripheral(editor.value); } catch (e) { showScriptError(e); }
  }
  timer = setInterval(loop, 100);
}

/// Releases everything this domain owns. The shell mounts one domain at a
/// time, so anything left running here becomes a leak -- and on netsim a
/// device whose socket is dropped without a disconnect lingers as a ghost at
/// the same address.
export function unmount() {
  if (timer !== null) {
    clearInterval(timer);
    timer = null;
  }
  try { peripheral?.free(); } catch (_) { /* already gone */ }
  try { link?.free(); } catch (_) { /* already gone */ }
  peripheral = null;
  link = null;
  linkIndex = -1;
  document.getElementById(STYLE_ID)?.remove();
  // Clear our own markup rather than relying on the host to do it: the
  // standalone page has no shell to tidy up after us.
  if (container) container.innerHTML = "";
  container = null;
  editor = connPill = setupPanel = picker = undefined;
}
