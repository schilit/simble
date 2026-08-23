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
// the "write" a phone app would send. (A scripted central could send a real
// one -- android::BluetoothGatt, as the Generic domain does -- but the point
// here is the bulb, so the picker writes the device's own database directly.)

import init, { WebPeripheral, WebLink, WebScriptedCentral, catalog_script } from "../pkg/simble.js";
import { createGattView } from "../common/gatt-view.js";
import { createDeviceHeader } from "../common/device-header.js";
import { attachHighlightedEditor } from "../common/highlight.js";
import { currentController } from "../common/controller-bar.js";
import { createAboutBox } from "../common/about-box.js";

/// Which controllers this domain can run on. The shell's controller bar
/// reads this: an option mapped to a string is offered disabled, with that
/// string as the reason, rather than hidden.
export const SUPPORTS = { "in-page": true, "websocket": true };


const STYLE_ID = "simble-home-style";

// The two-column layout is `.domain.two-up` in common/simble.css; only what
// this domain owns lives here.
const STYLE = `
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
    font-size: var(--fs-body); }`;

const ABOUT = `<p>A colour bulb and the client that drives it. The picker writes the colour
   <em>through the client, over GATT</em>, the way a phone app would; the page does not poke the
   value into the device's own database.</p>`;

const MARKUP = `

  <section id="setup" class="panel setup full">
    <h2>netsim is not reachable</h2>
    <p>Could not reach netsim at <code>localhost:7681</code> — is <code>netsimd</code> running with
       its WebSocket frontend enabled? Start it with:</p>
    <pre><code>netsimd --logtostderr --no-shutdown --ws-port 7681</code></pre>
  </section>

  <section class="panel">
    <div id="bulb-head"></div>
    <div id="bulb-script"></div>
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
    <dl class="kv">
      <dt>connection</dt><dd id="dev-conn">—</dd>
      <dt>subscription</dt><dd id="dev-sub">—</dd>
    </dl>
    <h2 class="sub">Its GATT database (server view)</h2>
    <div id="bulb-gatt"></div>
    <p class="hint" id="mode-hint"></p>
    <div id="script-error" class="error"></div>
    <p class="hint">The colour is a writable <code>[R, G, B]</code> characteristic on a custom
       128-bit service. The picker no longer pokes it host-side: the write goes through the client
       beside it, over GATT, the way a phone app's would.</p>
  </section>

  <section class="panel">
    <div id="client-head"></div>
    <div id="client-script"></div>
    <h2 class="sub">Discovered services (client view)</h2>
    <div id="client-gatt"></div>
    <div id="client-log" class="hint"></div>
  </section>`;

const IN_PAGE_ADDR = "CC:1E:57:00:00:05";
const CLIENT_ADDR = "CC:1E:57:00:00:15";
const CLIENT_NETSIM = "CC:1E:57:00:00:15";
const WS_URL =
  "ws://localhost:7681/v1/websocket/bt?name=web-lightbulb&address=CC:1E:57:00:00:05";

// Custom 128-bit UUIDs (Magic-Blue-style). The characteristic value is 3 bytes
// [R, G, B]. Simble renders 128-bit UUIDs lowercase-dashed, which is what
// status_json reports and what uuid::of parses — so this string round-trips.
const CLIENT_WS_URL =
  `ws://localhost:7681/v1/websocket/bt?name=web-bulb-client&address=${CLIENT_NETSIM}`;

const COLOR_SERVICE = "f0ff0001-1234-5678-90ab-cdef01234567";
const COLOR_CHAR = "f0ff0002-1234-5678-90ab-cdef01234567";

const PRESETS = ["#ff3355", "#ff9933", "#ffee33", "#33dd66", "#33ccff", "#7755ff", "#ff66cc", "#ffffff"];

// --- DOM -------------------------------------------------------------------
// Every lookup is scoped to the mounted root. The shell hosts one domain at a
// time in a shared stage, and this module's generic ids (`script`, `run`,
// `conn`) once collided with another domain's, writing one module's script
// into the other's textarea.
let root = null;
const $ = (id) => root.querySelector(`#${id}`);
let setupPanel, picker;
let head = null;        // the bulb's device header
let gatt = null;        // the bulb's GATT view (server side)
let clientHead = null;  // the client's device header
let clientGatt = null;  // what the client discovered
let clientIndex = -1;   // scripted central index within the in-page link
let clientDev = null;   // WebScriptedCentral, netsim backend only
let bulbScript = "";
let clientScript = "";

let mode = "in-page"; // "in-page" (a wasm WebLink in this tab) | "websocket" (netsim)
let peripheral = null; // WebPeripheral, WebSocket backend only
let link = null; // WebLink, in-page backend only
let linkIndex = -1; // peripheral index within the in-page link
let runStart = performance.now();
let lastConnectAttempt = 0;
let openedOnce = false;
let stopped = false; // Stop pressed: stay off the air until Run

function showScriptError(m) { $("script-error").textContent = m ? String(m) : ""; }

function createPeripheral(script) {
  if (peripheral) { try { peripheral.free(); } catch (_) { /* gone */ } }
  if (clientDev) { try { clientDev.free(); } catch (_) { /* gone */ } }
  peripheral = new WebPeripheral(WS_URL, script);
  // Each device owns its own netsim socket -- its own controller -- so the
  // pair meets over the real ether rather than a shared in-page link.
  clientDev = new WebScriptedCentral(CLIENT_WS_URL, clientScript);
  clientDev.set_target(IN_PAGE_ADDR);
  runStart = performance.now();
}

/// The script the device is built from. Read-only on this page: authoring is
/// the Playground's job, and what the pen is for here is showing that the bulb
/// really is a scripted peripheral rather than a picture of one.
const scriptText = () => bulbScript;

// In-page backend: host the bulb on a wasm WebLink in this tab — no netsim.
// A WebLink has no "remove peripheral", so re-running rebuilds the whole link
// from a fresh script; the new link only replaces the old once the script
// parses. Color picks write live via peripheral_set_value (see writeColor).
function buildInPage(script) {
  const next = new WebLink();
  let idx;
  try { idx = next.add_peripheral(IN_PAGE_ADDR, script); }
  catch (e) { try { next.free(); } catch (_) { /* gone */ } throw e; }
  let cidx;
  try {
    cidx = next.add_scripted_central(CLIENT_ADDR, clientScript);
    // The catalog's clients name EXAMPLE_PEER_ADDRESS; this page allocates
    // its own addresses, and topology beats script.
    next.scripted_central_set_target(cidx, IN_PAGE_ADDR);
  } catch (e) { try { next.free(); } catch (_) { /* gone */ } throw e; }
  if (link) { try { link.free(); } catch (_) { /* gone */ } }
  link = next;
  linkIndex = idx;
  clientIndex = cidx;
  runStart = performance.now();
}

function teardownDevices() {
  if (peripheral) { try { peripheral.free(); } catch (_) { /* gone */ } peripheral = null; }
  if (clientDev) { try { clientDev.free(); } catch (_) { /* gone */ } clientDev = null; }
  if (link) { try { link.free(); } catch (_) { /* gone */ } link = null; linkIndex = -1; clientIndex = -1; }
}

/// Puts the bulb on the air. In both backends this device is the only one its
/// controller hosts -- one WebPeripheral with its own socket, or a WebLink with
/// a single peripheral in it -- so run and stop here are genuinely this
/// device's, not the whole scene's.
function run() {
  showScriptError(null);
  stopped = false;
  try {
    if (mode === "in-page") buildInPage(scriptText());
    else if (peripheral) { peripheral.run_script(scriptText()); runStart = performance.now(); }
    else createPeripheral(scriptText());
    head.setRunning(true);
    head.setState(false, "starting…");
  } catch (e) { showScriptError(e); }
}

/// Takes the bulb off the air and keeps it off: the loop rebuilds a missing
/// device by design, so a stop that only freed the object would come back a
/// tenth of a second later.
function stop() {
  stopped = true;
  teardownDevices();
  openedOnce = false;
  gatt?.update({ services: [] });
  $("dev-conn").textContent = "stopped";
  $("dev-sub").textContent = "—";
  setupPanel.classList.remove("visible");
  head.setRunning(false);
  head.setState(false, "stopped");
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
    // Through the client, over GATT. This used to poke the value straight
    // into the device's own database because central-role scripting did not
    // exist; it does now, so the write crosses the link like a phone's would.
    if (mode === "in-page") {
      if (link && clientIndex >= 0) link.scripted_central_write(clientIndex, COLOR_CHAR, bytes);
    } else if (clientDev) {
      clientDev.write(COLOR_CHAR, bytes, true);
    }
  } catch (e) {
    showScriptError(e);
  }
}

// --- rendering -------------------------------------------------------------
function render(status) {
  // The name is the one the script's GATT server advertises, not a constant
  // here that could drift away from it.
  head.setName(status.name);
  if (status.address) head.setAddress(status.address);
  $("dev-conn").textContent = status.connected
    ? `connected to ${status.peer}` : "advertising, no central connected";
  const anySub = (status.services || []).some((s) => s.characteristics.some((c) => c.subscribed));
  $("dev-sub").textContent = anySub ? "central subscribed — notifications flowing" : "no subscriber yet";

  gatt.update(status);

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

/// The client's half of the view. Shared by both backends: what differs is
/// where the status came from, not what it means.
function renderClient(status, failure) {
  if (!clientHead) return;
  clientHead.setState(!!status.connected,
    failure ? `assert failed: ${failure}`
            : status.connected ? "connected to the bulb" : "connecting…",
    failure ? "bad" : status.connected ? "ok" : "");
  clientGatt?.update(status);
}

function loop() {
  if (stopped) return; // Stop pressed: stay torn down until Run
  if (mode === "in-page") {
    if (!link || linkIndex < 0) {
      try { buildInPage(scriptText()); } catch (e) { showScriptError(e); }
      return;
    }
    try {
      link.tick((performance.now() - runStart) / 1000);
      const json = link.peripheral_status_json(linkIndex);
      if (json) {
        const status = JSON.parse(json);
        head.setState(true, status.connected
          ? "in browser · client connected" : "in browser · advertising", "ok");
        render(status);
      }
      renderClient(JSON.parse(link.central_status_json(clientIndex) || "{}"),
                   link.scripted_central_failure(clientIndex));
    } catch (e) { showScriptError(e); }
    return;
  }
  if (!peripheral) {
    const now = performance.now();
    if (now - lastConnectAttempt > 3000) {
      lastConnectAttempt = now;
      try { createPeripheral(scriptText()); } catch (e) { showScriptError(e); }
    }
    return;
  }
  if (clientDev) {
    try {
      renderClient(JSON.parse(clientDev.tick((performance.now() - runStart) / 1000) || "{}"),
                   clientDev.failure());
    } catch (e) { showScriptError(e); }
  }
  const state = peripheral.ready_state();
  if (state === 3) {
    if (openedOnce) head.setState(false, "connection lost — reconnecting…", "bad");
    else { head.setState(false, "netsim not reachable", "bad"); setupPanel.classList.add("visible"); }
    try { peripheral.free(); } catch (_) { /* gone */ }
    peripheral = null;
    return;
  }
  if (state === 0) {
    head.setState(false, openedOnce ? "reconnecting…" : "connecting to localhost:7681…");
    return;
  }
  openedOnce = true;
  setupPanel.classList.remove("visible");
  try {
    const status = JSON.parse(peripheral.tick((performance.now() - runStart) / 1000));
    head.setState(true, status.connected ? "on air · central connected" : "on air · advertising", "ok");
    render(status);
  } catch (e) { showScriptError(e); }
}

// --- lifecycle -------------------------------------------------------------

let timer = null;

/// Builds the domain into `container` and starts it. Async because the wasm
/// module has to be initialised before any device exists.
export async function mount(container) {
  await init();

  if (!document.getElementById(STYLE_ID)) {
    const el = document.createElement("style");
    el.id = STYLE_ID;
    el.textContent = STYLE;
    document.head.append(el);
  }
  root = container;
  root.classList.add("domain", "two-up");
  root.innerHTML = MARKUP;
  (root.querySelector(".domain") ?? root).prepend(createAboutBox(ABOUT));

  // Both halves come from the shared catalog -- the same definitions MCP's
  // `example` tool and the scene loader read -- rather than a copy living in
  // this page.
  bulbScript = catalog_script("color_bulb");
  clientScript = catalog_script("bulb_client");

  setupPanel = $("setup");
  picker = $("picker");

  head = createDeviceHeader({
    name: "Color Bulb",
    kind: "peripheral",
    accent: "good",
    address: IN_PAGE_ADDR,
    dotMeans: "the bulb is on the air and advertising",
    script: {
      text: bulbScript,
      editable: false,
      highlight: attachHighlightedEditor,
      note: "<strong>Read-only here.</strong> This is the device: a writable <code>[R, G, B]</code> " +
        "characteristic on a custom 128-bit service. To change it, take it to the " +
        "<a href=\"../playground/\">Playground</a>, which is where authoring lives.",
    },
    // Whichever backend is selected, this bulb is the only device its
    // controller hosts, so stopping it stops nothing else.
    run: { running: true, onRun: run, onStop: stop },
  });
  $("bulb-head").append(head.el);
  $("bulb-script").append(head.panel);

  // The colour characteristic is this page's own invention, so its decoder
  // lives here rather than in the shared viewer -- that is the seam the widget
  // used to fork along.
  gatt = createGattView({
    mode: "server",
    decode: (c) => {
      if (c.uuid !== COLOR_CHAR || !c.value || c.value.length < 6) return undefined;
      const [r, g, b] = [0, 2, 4].map((i) => parseInt(c.value.slice(i, i + 2), 16));
      return `RGB ${r},${g},${b}`;
    },
  });
  $("bulb-gatt").append(gatt.el);

  clientHead = createDeviceHeader({
    name: "Bulb Client", kind: "central", accent: "accent",
    address: mode === "in-page" ? CLIENT_ADDR : CLIENT_NETSIM,
    dotMeans: "the client is connected to the bulb",
    script: { text: clientScript, editable: false,
      note: "<strong>Read-only here.</strong> The catalog's <code>bulb_client</code>." },
    run: { running: false, disabled: true,
           reason: "the pair shares one controller — the bulb's toggle drives both" },
  });
  $("client-head").append(clientHead.el);
  $("client-script").append(clientHead.panel);
  clientGatt = createGattView({ mode: "client", empty: "Connecting…" });
  $("client-gatt").append(clientGatt.el);

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
    stopped = false;
    setupPanel.classList.remove("visible");
    setModeHint();
    // Same address either way -- the in-page link and the netsim socket both
    // put the bulb on the air as IN_PAGE_ADDR -- but restate it, because the
    // header may be showing whatever the last device reported.
    head.setAddress(IN_PAGE_ADDR);
    head.setRunning(true);
    if (mode === "in-page") {
      head.setState(false, "starting…");
      try { buildInPage(scriptText()); } catch (e) { showScriptError(e); }
    } else {
      head.setState(false, "connecting to localhost:7681…");
      try { createPeripheral(scriptText()); } catch (e) { showScriptError(e); }
    }
  }
  mode = currentController();
  setModeHint();

  if (mode === "in-page") {
    try { buildInPage(scriptText()); } catch (e) { showScriptError(e); }
  } else {
    try { createPeripheral(scriptText()); } catch (e) { showScriptError(e); }
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
  try { clientDev?.free(); } catch (_) { /* already gone */ }
  clientDev = null;
  clientIndex = -1;
  gatt?.destroy();
  clientGatt?.destroy();
  head?.destroy();
  clientHead?.destroy();
  gatt = head = clientGatt = clientHead = null;
  document.getElementById(STYLE_ID)?.remove();
  // Clear our own markup rather than relying on the host to do it: the
  // standalone page has no shell to tidy up after us.
  if (root) {
    root.classList.remove("domain", "two-up");
    root.innerHTML = "";
  }
  root = null;
  setupPanel = picker = undefined;
}
