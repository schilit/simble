// Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
//
// SimBLE Health domain: a heart-rate monitor and the client that listens to it.
//
// Exports mount(root)/unmount() so the Devices shell can host it as a tab.
//
// Both halves are scripts. The monitor is the catalog's `hrm` peripheral; the
// listener is its `hrm_client` central, written in the same Android vocabulary
// -- android::BluetoothGatt is Android's BluetoothGatt, and the on_* functions
// are its BluetoothGattCallback. Until central-role scripting existed this tab
// could only show a device advertising into an empty room.
//
// The heart beats at the rate the *client received*, not at the value the
// monitor holds. That distinction is the point of the pair: what you see has
// crossed a GATT subscription rather than been read out of the device that
// produced it.

import init, { WebLink, WebPeripheral, WebScriptedCentral, catalog_script } from "../pkg/simble.js";
import { currentController } from "../common/controller-bar.js";
import { createDeviceHeader } from "../common/device-header.js";
import { createGattView } from "../common/gatt-view.js";
import { bpmFromHex } from "../common/viewer-format.js";
import { createAboutBox } from "../common/about-box.js";

/// Which controllers this domain can run on. The shell's controller bar
/// reads this: an option mapped to a string is offered disabled, with that
/// string as the reason, rather than hidden.
export const SUPPORTS = { "in-page": true, "websocket": true };


const STYLE_ID = "simble-health-style";
// The catalog's central examples connect to EXAMPLE_PEER_ADDRESS, and a test
// in catalog.rs asserts they all do. Using it here is what pairs the two
// scripts -- overriding the target afterwards is fighting the convention.
const MONITOR_ADDR = "AA:BB:CC:00:00:01";
const CLIENT_ADDR = "AA:BB:CC:00:00:02";

// On netsim each device owns its own socket -- its own controller, as two
// separate machines would have -- so the pair meets over the real ether
// instead of a shared in-page link. That is also what lets an Android
// emulator connect to the monitor, which the in-page link cannot offer.
const MONITOR_NETSIM = "CC:1E:57:00:00:02";
const CLIENT_NETSIM = "CC:1E:57:00:00:12";
const WS = (name, address) =>
  `ws://localhost:7681/v1/websocket/bt?name=${name}&address=${address}`;

// Only what this domain owns. The two-column layout is `.domain.two-up` in
// common/simble.css -- the pair reads left to right, the monitor and then
// what the client heard from it.
const STYLE = `
  /* The heart, which is the whole reason this domain has a page of its own:
     the animation period is 60/bpm, so what you see beating is the
     characteristic's value and not a fixed loop. */
  .heart-box { text-align: center; padding: 0.5rem 0 0.75rem; }
  .heart {
    display: inline-block; font-size: var(--fs-display); color: var(--heart);
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
  .bpm { font-size: var(--fs-hero); font-weight: 600; }
  .bpm small { color: var(--dim); font-size: var(--fs-body); font-weight: 400; }`;

const ABOUT = `<p>A heart-rate monitor and the client listening to it, both defined by scripts. The heart
   beats at the rate the <em>client received</em> over a GATT subscription, not at the value the
   monitor holds — what you see crossed the link.</p>`;

const MARKUP = `

  <section class="panel">
    <div id="monitor-head"></div>
    <div id="monitor-script"></div>
    <h2 class="sub">Its GATT database (server view)</h2>
    <div id="monitor-gatt"></div>
    <p class="hint">The monitor is a script: it builds the Heart Rate service and updates the
       measurement over time. Nothing here reaches into it — the value below crossed a real
       subscription.</p>
  </section>

  <section class="panel">
    <div id="client-head"></div>
    <div id="client-script"></div>
    <div id="hr-box" class="heart-box" hidden>
      <span id="heart" class="heart flat">❤</span>
      <div class="bpm"><span id="bpm">—</span> <small>bpm received</small></div>
    </div>
    <h2 class="sub">Discovered services (client view)</h2>
    <div id="client-gatt"></div>
    <div id="client-log" class="hint"></div>
  </section>`;

const MONITOR_NOTE =
  `<strong>Read-only here.</strong> The catalog's <code>hrm</code> device — the same one MCP's ` +
  `<code>example</code> tool serves. Authoring lives in the <a href="../playground/">Playground</a>.`;
const CLIENT_NOTE =
  `<strong>Read-only here.</strong> The catalog's <code>hrm_client</code> — a real central script. ` +
  `Its <code>on_characteristic_changed</code> callback is what drives the heartbeat above.`;

// --- state -----------------------------------------------------------------
// Every handle is resolved against the mounted root: the shell hosts one
// domain at a time in a shared stage, and ids like `script` have collided
// across modules before.
let root = null;
let mode = "in-page";     // "in-page" (one wasm WebLink) | "websocket" (netsim)
let link = null;          // in-page backend
let monitorDev = null;    // netsim backend: WebPeripheral
let clientDev = null;     // netsim backend: WebScriptedCentral
let monitorHead = null, clientHead = null;
let monitorGatt = null, clientGatt = null;
let monitorIndex = -1, clientIndex = -1;
let monitorScript = "", clientScript = "";
let runStart = performance.now();
let stopped = true;
let lastBpm = null; // avoid restarting the heartbeat animation every tick
let timer = null;

const $ = (id) => root.querySelector(`#${id}`);

/// Builds both devices into one in-page link. They share it, so they start and
/// stop together -- which is why a single toggle drives the pair rather than
/// each header carrying its own.
function build() {
  release();
  if (mode === "in-page") {
    link = new WebLink();
    monitorIndex = link.add_peripheral(MONITOR_ADDR, monitorScript);
    clientIndex = link.add_scripted_central(CLIENT_ADDR, clientScript);
  } else {
    monitorDev = new WebPeripheral(WS("web-hrm", MONITOR_NETSIM), monitorScript);
    clientDev = new WebScriptedCentral(WS("web-hr-client", CLIENT_NETSIM), clientScript);
    // The catalog's client scripts all connect to EXAMPLE_PEER_ADDRESS, which
    // is the in-page convention; on netsim the monitor lives elsewhere.
    clientDev.set_target(MONITOR_NETSIM);
  }
  runStart = performance.now();
  lastBpm = null;
}

/// Frees whichever backend is live. A netsim device whose socket drops without
/// a disconnect lingers as a ghost at the same address, so this is not
/// optional bookkeeping.
function release() {
  for (const d of [link, monitorDev, clientDev]) {
    if (d) { try { d.free(); } catch (_) { /* already gone */ } }
  }
  link = monitorDev = clientDev = null;
}

function run() {
  stopped = false;
  try {
    build();
    monitorHead.setRunning(true);
    clientHead.setRunning(true);
  } catch (e) {
    showError(e);
  }
}

function stop() {
  stopped = true;
  release();
  lastBpm = null;
  monitorGatt?.update({ services: [] });
  clientGatt?.update({ services: [] });
  $("hr-box").hidden = true;
  monitorHead.setRunning(false);
  monitorHead.setState(false, "stopped");
  clientHead.setRunning(false);
  clientHead.setState(false, "stopped");
}

function showError(message) {
  if (!message) return;
  console.error("health domain:", message);
  clientHead?.setState(false, "script error — see console", "bad");
}

// --- rendering -------------------------------------------------------------

/// The heartbeat is driven by what the client received, so it is evidence the
/// subscription is live rather than a readout of the monitor's own state.
function renderHeart(clientStatus) {
  const hr = (clientStatus.services || [])
    .flatMap((s) => s.characteristics || [])
    .find((c) => c.uuid === "2A37");
  const box = $("hr-box");
  if (!hr || !hr.value) { box.hidden = true; return; }
  box.hidden = false;
  const bpm = bpmFromHex(hr.value);
  const heart = $("heart");
  if (bpm && bpm > 0) {
    $("bpm").textContent = bpm;
    heart.classList.remove("flat");
    // Only reset the animation when the rate actually changes, or the
    // heartbeat restarts every tick and stutters.
    if (bpm !== lastBpm) heart.style.animationDuration = `${(60 / bpm).toFixed(3)}s`;
    lastBpm = bpm;
  } else {
    $("bpm").textContent = "—";
    heart.classList.add("flat");
    lastBpm = null;
  }
}

function loop() {
  if (stopped) return;
  const t = (performance.now() - runStart) / 1000;
  try {
    let monitor, client;
    if (mode === "in-page") {
      if (!link) return;
      link.tick(t);
      monitor = JSON.parse(link.peripheral_status_json(monitorIndex));
      client = JSON.parse(link.central_status_json(clientIndex));
    } else {
      if (!monitorDev || !clientDev) return;
      // Each socket is pumped by its own tick; a device still connecting
      // returns no status yet, which is not an error.
      monitor = JSON.parse(monitorDev.tick(t));
      client = JSON.parse(clientDev.tick(t));
    }

    monitorHead.setName(monitor.name);
    monitorHead.setState(true,
      monitor.connected ? "on air · client connected" : "on air · advertising", "ok");
    monitorGatt.update(monitor);

    clientHead.setState(!!client.connected,
      client.connected ? `client · ${client.phase || "connected"}` : "connecting…",
      client.connected ? "ok" : "");
    clientGatt.update(client);
    renderHeart(client);

    // A failed assert in a callback is the run's verdict and never clears.
    const failure = mode === "in-page"
      ? link.scripted_central_failure(clientIndex)
      : clientDev.failure();
    if (failure) clientHead.setState(false, `assert failed: ${failure}`, "bad");

    const emitted = mode === "in-page"
      ? link.scripted_central_emitted(clientIndex)
      : clientDev.emitted();
    if (emitted && emitted.length) {
      $("client-log").textContent = Array.from(emitted).slice(-3).join(" · ");
    }
  } catch (e) {
    showError(e);
  }
}

// --- lifecycle -------------------------------------------------------------

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

  monitorScript = catalog_script("hrm");
  clientScript = catalog_script("hrm_client");

  monitorHead = createDeviceHeader({
    name: "starting…", kind: "peripheral", accent: "good",
    address: MONITOR_ADDR,
    dotMeans: "the monitor is on the air",
    script: { text: monitorScript, editable: false, note: MONITOR_NOTE },
    run: { running: false, onRun: run, onStop: stop },
  });
  $("monitor-head").append(monitorHead.el);
  $("monitor-script").append(monitorHead.panel);

  clientHead = createDeviceHeader({
    name: "HR Client", kind: "central", accent: "accent",
    address: CLIENT_ADDR,
    dotMeans: "the client is connected to the monitor",
    script: { text: clientScript, editable: false, note: CLIENT_NOTE },
    // Both devices share one in-page link, so stopping either stops both.
    // The monitor's toggle drives the pair; this one says why it cannot.
    run: { running: false, disabled: true,
           reason: "one in-page link — the pair starts and stops together" },
  });
  $("client-head").append(clientHead.el);
  $("client-script").append(clientHead.panel);

  monitorGatt = createGattView({ mode: "server" });
  $("monitor-gatt").append(monitorGatt.el);
  clientGatt = createGattView({ mode: "client", empty: "Connecting…" });
  $("client-gatt").append(clientGatt.el);

  mode = currentController();
  run();
  timer = setInterval(loop, 100);
}

export function unmount() {
  if (timer !== null) { clearInterval(timer); timer = null; }
  try { stop(); } catch (_) { /* already down */ }
  monitorGatt?.destroy();
  clientGatt?.destroy();
  monitorHead?.destroy();
  clientHead?.destroy();
  document.getElementById(STYLE_ID)?.remove();
  if (root) {
    root.classList.remove("domain", "two-up");
    root.innerHTML = "";
  }
  root = null; link = null;
  monitorHead = clientHead = monitorGatt = clientGatt = null;
}
