// Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
//
// SimBLE Health domain: a heart-rate monitor, as a mountable domain module.
//
// Exports mount(root)/unmount() so the Devices shell can host it as a tab. The
// authoring half of the old scripted-device page -- editor, Run/Stop and the AI
// prompt -- is gone: that is the Playground's job, and this is a demo that
// starts itself. The script comes straight from the library's
// default_heart_rate_script(), so the tab and MCP's `example` tool serve the
// same device; the pen in the device header shows it, read-only.
//
// The page used to carry a private copy of the whole GATT viewer -- the
// assigned-number table, the decoders, the structure-signature rebuild, all of
// it duplicated from common/viewer.js and free to drift. It now uses the shared
// widget, and keeps only the one thing that is genuinely this domain's: a heart
// that beats at whatever rate the Heart Rate Measurement characteristic
// currently holds.

import init, { WebPeripheral, default_heart_rate_script } from "../pkg/simble.js";
import { createDeviceHeader } from "../common/device-header.js";
import { createGattView } from "../common/gatt-view.js";
import { bpmFromHex } from "../common/viewer-format.js";

const STYLE_ID = "simble-health-style";

// Only what this domain owns. Everything shared -- panels, pills, the GATT
// viewer, the device header, the netsim setup box -- is in common/simble.css,
// which every page that can host this module links. The old copy of that file
// lived here and restyled the shell's own <header> out from under it.
// One column: the authoring half that used to fill the second one is gone, and
// a device panel squeezed into half the width wraps its own header.
const STYLE = `main {
    display: grid; gap: 1.25rem; padding: 1.25rem 1.5rem;
    max-width: 52rem; margin: 0 auto;
  }
  /* The heart, which is the whole reason this domain has a page of its own:
     the animation period is 60/bpm, so what you see beating is the
     characteristic's value and not a fixed loop. */
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
  .bpm small { color: var(--dim); font-size: 0.9rem; font-weight: 400; }`;

const MARKUP = `<section id="setup" class="panel setup">
    <h2>netsim is not reachable</h2>
    <p>Could not reach netsim at <code>localhost:7681</code> — is <code>netsimd</code> running with its
       WebSocket frontend enabled? This page is served from the cloud, but the Bluetooth scene runs
       <strong>on your machine</strong>: the wasm build of SimBLE in this tab connects to a local
       <code>netsimd</code> over <code>ws://localhost:7681</code>. Start it with:</p>
    <pre><code>netsimd --logtostderr --no-shutdown --ws-port 7681</code></pre>
    <p class="hint">Needs the canary-channel emulator (see the README's <em>Testing Against netsim</em>
       section). Tip: open <a href="../scanner/">the scanner</a> in a second tab — both tabs join the
       same netsim scene.</p>
  </section>

  <section class="panel">
    <div id="device-head"></div>
    <div id="device-script"></div>
    <div id="hr-box" class="heart-box" hidden>
      <span id="heart" class="heart flat">❤</span>
      <div class="bpm"><span id="bpm">—</span> <small>bpm</small></div>
    </div>
    <dl class="kv">
      <dt>connection</dt><dd id="dev-conn">—</dd>
      <dt>subscription</dt><dd id="dev-sub">—</dd>
    </dl>
    <div id="gatt"></div>
    <p class="hint">This viewer renders whatever GATT structure the running script builds — every
       service and characteristic, with properties, live values, and subscription state read from
       the wasm stack's real attribute database. A central on the scene can connect and subscribe
       (an emulator, or just watch the advertisement with the scanner tab).</p>
  </section>`;

const ADDRESS = "CC:1E:57:00:00:02";
const WS_URL =
  `ws://localhost:7681/v1/websocket/bt?name=web-device&address=${ADDRESS}`;

const SCRIPT_NOTE =
  `<strong>Read-only here.</strong> This is <code>default_heart_rate_script()</code> from the ` +
  `library — the same device MCP's <code>example</code> tool serves. To change it, take it to the ` +
  `<a href="../playground/">Playground</a>, which is where authoring lives.`;

// --- state -----------------------------------------------------------------
// All of it belongs to one mount and is cleared by unmount(). Every DOM handle
// is resolved against the mounted root: the shell hosts one domain at a time
// in a shared stage, and ids like `script` and `conn` have collided across
// modules before.
let root = null;
let head = null;
let gatt = null;
let script = "";

let peripheral = null;
let runStart = performance.now();
let lastConnectAttempt = 0;
let openedOnce = false;
let stopped = true;
let lastBpm = null; // avoid restarting the heartbeat animation every tick

const $ = (id) => root.querySelector(`#${id}`);

function showScriptError(message) {
  // The device is built from a library script, so an error here is a library
  // bug rather than something a reader typed. It goes to the console and to
  // the device's own state line, where it belongs.
  if (!message) return;
  console.error("device script error:", message);
  head?.setState(false, `script error — see console`, "bad");
}

/// A fresh WebPeripheral opens a fresh WebSocket. Because this device owns its
/// socket outright, freeing it is a real stop: unlike the in-page WebLink
/// pages, nothing else goes down with it.
function createPeripheral() {
  if (peripheral) {
    try { peripheral.free(); } catch (_) { /* already gone */ }
  }
  peripheral = new WebPeripheral(WS_URL, script); // throws on script errors
  runStart = performance.now();
}

function run() {
  stopped = false;
  try {
    if (peripheral) {
      peripheral.run_script(script); // same socket, new device
      runStart = performance.now();
    } else {
      createPeripheral();
    }
    head.setRunning(true);
    head.setState(false, "starting…");
  } catch (e) {
    showScriptError(e);
  }
}

/// Tears the device down and holds it off the air until Run is pressed again.
function stop() {
  stopped = true;
  if (peripheral) {
    try { peripheral.free(); } catch (_) { /* already gone */ }
    peripheral = null;
  }
  lastBpm = null;
  gatt?.update({ services: [] });
  $("dev-conn").textContent = "stopped";
  $("dev-sub").textContent = "—";
  $("hr-box").hidden = true;
  $("setup").classList.remove("visible");
  head.setRunning(false);
  head.setState(false, "stopped");
}

// --- rendering -------------------------------------------------------------

function render(status) {
  // The name is whatever the script's GATT server calls itself -- the header
  // must not assert a device the script does not build. (The library's
  // `default_heart_rate_script` keeps its legacy name and now builds a
  // thermometer, which is exactly the drift a hard-coded label would hide.)
  head.setName(status.name);
  head.setAddress(status.address || ADDRESS);
  $("dev-conn").textContent = status.connected
    ? `connected to ${status.peer}`
    : "advertising, no central connected";
  const anySubscribed = (status.services || []).some((s) =>
    s.characteristics.some((c) => c.subscribed));
  $("dev-sub").textContent = anySubscribed
    ? "central subscribed — notifications flowing"
    : "no subscriber yet";

  gatt.update(status);

  const hrChar = (status.services || [])
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
      try { createPeripheral(); } catch (e) { showScriptError(e); }
    }
    return;
  }
  const state = peripheral.ready_state(); // 0 connecting 1 open 2 closing 3 closed
  if (state === 3) {
    // Distinguish "never reached netsim" (connection refused) from a
    // mid-session drop: only the former shows the setup instructions.
    if (openedOnce) {
      head.setState(false, "connection lost — reconnecting…", "bad");
    } else {
      head.setState(false, "netsim not reachable", "bad");
      $("setup").classList.add("visible");
    }
    try { peripheral.free(); } catch (_) { /* already gone */ }
    peripheral = null; // the next loop pass schedules a reconnect
    return;
  }
  if (state === 0) {
    head.setState(false, openedOnce ? "reconnecting…" : "connecting to localhost:7681…");
    return;
  }
  openedOnce = true;
  $("setup").classList.remove("visible");
  try {
    const status = JSON.parse(peripheral.tick((performance.now() - runStart) / 1000));
    // The dot is on when the device is genuinely on the air; the label says
    // whether anyone has connected to it.
    head.setState(true, status.connected ? "on air · central connected" : "on air · advertising", "ok");
    render(status);
  } catch (e) {
    showScriptError(e);
  }
}

// --- lifecycle -------------------------------------------------------------

let timer = null;

/// Builds the domain into `root` and starts it. Async because the wasm module
/// must be initialised before a device can exist.
export async function mount(container) {
  await init();

  if (!document.getElementById(STYLE_ID)) {
    const el = document.createElement("style");
    el.id = STYLE_ID;
    el.textContent = STYLE;
    document.head.append(el);
  }
  root = container;
  root.innerHTML = MARKUP;
  script = default_heart_rate_script();

  head = createDeviceHeader({
    name: "starting…", // replaced by the device's own advertised name
    kind: "peripheral · Rhai script · netsim",
    accent: "good",
    address: ADDRESS,
    dotMeans: "the device is on the air over netsim",
    script: { text: script, editable: false, note: SCRIPT_NOTE },
    // This device owns its WebSocket, so freeing it really does take it off
    // the air and nothing else with it -- which is why the toggle is live here
    // and disabled on the pages whose devices share one in-page link.
    run: { running: false, onRun: run, onStop: stop },
  });
  $("device-head").append(head.el);
  $("device-script").append(head.panel);

  gatt = createGattView({ mode: "server" });
  $("gatt").append(gatt.el);

  // Unlike the old authoring page, a domain tab starts itself -- there is no
  // Run button to press first, because the script is not editable here.
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
  gatt?.destroy();
  head?.destroy();
  document.getElementById(STYLE_ID)?.remove();
  if (root) root.innerHTML = "";
  root = null;
  head = null;
  gatt = null;
}
