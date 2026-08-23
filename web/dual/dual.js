// SimBLE Server + Client dual view. A peripheral and a central, and **both are
// scripts**: android::BluetoothGattServer
// on the left, a central (android::BluetoothGatt) on the right. The left panel
// renders the server's own database; the right renders what the client
// discovered — the nRF-Connect flow.
//
// The client panel used to display the page's own WebLink calls, because
// central-role scripting did not exist. It does now, so the panel is an
// editor like the other one and the two halves are the same kind of thing.

import init, { WebLink, WebPeripheral, WebScriptedCentral, catalog_script } from "../pkg/simble.js";
import { currentController } from "../common/controller-bar.js";
import { createDeviceHeader } from "../common/device-header.js";
import { createGattView, promptForBytes } from "../common/gatt-view.js";
import { renderGatt, nameFor, propChips, escapeHtml, decodeValue } from "../common/viewer.js";
import { createAboutBox } from "../common/about-box.js";

/// Which controllers this domain can run on. The shell's controller bar
/// reads this: an option mapped to a string is offered disabled, with that
/// string as the reason, rather than hidden.
///
/// netsim used to be refused here on the grounds that "both devices share one
/// in-page link". That was a fact about one backend, not about the pair: over
/// netsim each half gets its own engine on its own socket -- its own
/// controller, as two separate machines would have -- exactly as Health and
/// Home already do. The server is a `WebPeripheral`, the client a
/// `WebScriptedCentral`, and they meet over rootcanal instead of in this tab.
export const SUPPORTS = { "in-page": true, "websocket": true };


const STYLE_ID = "simble-generic-style";

const STYLE = `main { max-width: 78rem; margin: 0 auto; padding: 1rem 1.25rem 2rem; }
  .intro { color: var(--dim); font-size: var(--fs-body); margin: 0 0 1rem; max-width: 60rem; }
  .device { border: 1px solid var(--border); border-radius: 10px; background: var(--panel); overflow: hidden; }
  .device > header {
    display: flex; align-items: center; gap: 0.5rem; flex-wrap: wrap;
    padding: 0.6rem 0.9rem; border-bottom: 1px solid var(--border); background: #eaeef2;
  }
  .device > header .role { font-weight: 700; }
  .device > header .kind { color: var(--dim); font-size: var(--fs-label); }
  .device > header .addr { margin-left: auto; font-family: ui-monospace, Menlo, monospace;
    color: var(--dim); font-size: var(--fs-meta); }
  .device .body { padding: 0.8rem 0.9rem; }
  .server .role { color: var(--good); }
  .client .role { color: var(--accent); }
  textarea#script {
    width: 100%; height: 12rem; resize: vertical; background: var(--bg); color: var(--text);
    border: 1px solid var(--border); border-radius: 6px; padding: 0.6rem; tab-size: 4;
    font: 12px/1.45 ui-monospace, SFMono-Regular, Menlo, monospace;
  }
  .row { display: flex; gap: 0.5rem; align-items: center; margin: 0.55rem 0; flex-wrap: wrap; }
  .hint { color: var(--dim); font-size: var(--fs-label); }
  .phase { font-size: var(--fs-label); color: var(--dim); }
  .phase b { color: var(--accent); }
  h2.sub { font-size: var(--fs-meta); text-transform: uppercase; letter-spacing: 0.06em;
    color: var(--dim); margin: 0.9rem 0 0.5rem; }
  /* Client (discovered) GATT — nRF-Connect-flavoured tree */
  .svc { border: 1px solid var(--border); border-radius: 8px; margin-bottom: 0.6rem; overflow: hidden; }
  .device header .dot { width: 9px; height: 9px; border-radius: 50%; display: inline-block;
  margin-right: 0.42rem; background-color: var(--dim); vertical-align: middle; }
.device.server header .dot.on { background-color: var(--good); }
.device.client header .dot.on { background-color: var(--accent); }
.device header .icon { border: 1px solid var(--border); background: transparent; border-radius: 6px;
  cursor: pointer; font-size: var(--fs-body); line-height: 1; padding: 0.18rem 0.42rem; color: var(--dim);
  margin-left: 0.3rem; }
.device header .icon:hover { color: var(--fg); border-color: var(--fg); }
.device header .icon[aria-pressed="true"] { color: var(--fg); border-color: var(--fg); }
#client-script-text { width: 100%; height: 12rem; resize: vertical; background: var(--bg);
  color: var(--text); border: 1px solid var(--border); border-radius: 6px; padding: 0.6rem;
  tab-size: 4; font: 12px/1.45 ui-monospace, SFMono-Regular, Menlo, monospace; }
#client-log { margin-top: 0.4rem; max-height: 6rem; overflow-y: auto;
  font: 11px/1.5 ui-monospace, SFMono-Regular, Menlo, monospace; color: var(--dim); }
#client-log .bad { color: var(--bad, #b00); }
.svc-head { background: #eaeef2; padding: 0.4rem 0.7rem; font-size: var(--fs-body); font-weight: 500; }
  .svc-head .u { font-family: ui-monospace, Menlo, monospace; color: var(--dim); font-size: var(--fs-meta); margin-left: 0.4rem; }
  .chr { padding: 0.45rem 0.7rem; border-top: 1px solid var(--border); }
  .chr-top { display: flex; align-items: baseline; gap: 0.5rem; flex-wrap: wrap; }
  .chr-name { font-weight: 500; }
  .chr-uuid { font-family: ui-monospace, Menlo, monospace; color: var(--dim); font-size: var(--fs-meta); }
  .chr-h { color: var(--dim); font-size: var(--fs-meta); margin-left: auto; font-family: ui-monospace, Menlo, monospace; }
  .prop { display: inline-block; border: 1px solid var(--border); border-radius: 4px; padding: 0 0.3rem;
    font-size: var(--fs-meta); color: var(--dim); font-family: ui-monospace, Menlo, monospace; }
  .prop.n, .prop.i { color: var(--good); border-color: var(--good); }
  .chr-val { margin-top: 0.2rem; font-size: var(--fs-body); }
  .chr-val .decoded { font-weight: 600; }
  .chr-val .raw { font-family: ui-monospace, Menlo, monospace; color: var(--dim);
    font-size: var(--fs-meta); margin-left: 0.4rem; }
  .chr-actions { margin-top: 0.35rem; display: flex; gap: 0.35rem; flex-wrap: wrap; }
  .chr-actions button {
    background: #eaeef2; color: var(--text); border: 1px solid var(--border); border-radius: 5px;
    padding: 0.12rem 0.5rem; font-size: var(--fs-meta); cursor: pointer;
  }
  .chr-actions button:hover { border-color: var(--accent); }
  .chr-actions button.on { color: var(--good); border-color: var(--good); }
  .empty { color: var(--dim); font-size: var(--fs-body); }
  footer { color: var(--dim); font-size: var(--fs-meta); padding: 1.25rem 1.25rem 0; max-width: 78rem; margin: 0 auto; }`;

const ABOUT = `<p>This page runs <strong>a peripheral and a central</strong>, each defined
    by its own Rhai script. On the
    <a href="../controllers/">in-browser controller</a> they share one in-process link; on
    <strong>netsim</strong> each gets its own socket, so the pair meets over rootcanal the way two
    separate machines would. On the left, a <strong class="role" style="color:var(--good)">Server</strong>
    (peripheral) defined by an editable Rhai script. On the right, a <strong style="color:var(--accent)">Client</strong>
    (central) using <code>android::BluetoothGatt</code>, Android's client API — it connects, discovers
    the server's GATT, and reacts in callbacks the way nRF Connect shows a connected device. What looks
    like "a device and its UI" is two devices talking, and both halves are editable.</p>`;

const MARKUP = `  <section id="setup" class="panel setup full">
    <h2>netsim is not reachable</h2>
    <p>Could not reach netsim at <code>localhost:7681</code> — is <code>netsimd</code> running with
       its WebSocket frontend enabled? Start it with:</p>
    <pre><code>netsimd --logtostderr --no-shutdown --ws-port 7681</code></pre>
  </section>

  <section class="panel">
    <div id="server-head"></div>
    <div id="server-script"></div>
    <h2 class="sub">Its GATT database (server view)</h2>
    <div id="server-gatt"></div>
  </section>

  <section class="panel">
    <div id="client-head"></div>
    <div id="client-script"></div>
    <h2 class="sub">Discovered services (client view)</h2>
    <div id="client-gatt"></div>
    <div id="client-log" class="output-log"></div>
  </section>`;

const $ = (id) => document.getElementById(id);
// In-page addresses. The catalog's central examples connect to
// EXAMPLE_PEER_ADDRESS, and a test in catalog.rs asserts they all do, so
// gatt_walker's own `connect(...)` already names the server below.
const SERVER_ADDR = "AA:BB:CC:00:00:01";
const CLIENT_ADDR = "AA:BB:CC:00:00:02";

// netsim addresses. Every domain shares one rootcanal, so these have to be
// this page's alone: Health holds :02/:12, Home :05/:15, Media :07/:08/:09,
// Ranging :0A/:0B, the scanner demo :01 and :11/:12/:13.
const SERVER_NETSIM = "CC:1E:57:00:00:0C";
const CLIENT_NETSIM = "CC:1E:57:00:00:1C";
const WS = (node, address) =>
  `ws://localhost:7681/v1/websocket/bt?name=${encodeURIComponent(node)}&address=${address}`;

// netsim does not synthesize a disconnect when a WebSocket drops: the device
// entry lingers, and a socket that re-registers under the same name a moment
// later can be attached to the stale one — the page looks connected while the
// device's tx count never leaves zero. So a dropped pair is never reopened
// immediately; it waits out this backoff first.
const RECONNECT_MS = 3000;

// The client, as a script. Every call below exists -- this is the same
// android::BluetoothGatt surface the MCP add_central tool and the interop
// tests use, not an illustration.
let mode = "in-page";     // "in-page" (one wasm WebLink) | "websocket" (netsim)
let link = null;          // in-page backend: both devices on one link
let serverDev = null;     // netsim backend: WebPeripheral, its own socket
let clientDev = null;     // netsim backend: WebScriptedCentral, its own socket
let serverIndex = 0;
let clientIndex = 1;
let t0 = performance.now();
const prevValues = new Map();
/// The last failure already written to the log, so it is not repeated.
let reportedFailure = null;

// netsim bookkeeping: the last status each engine reported (the pump ticks
// faster than the page renders), whether the sockets have ever been open —
// which separates "netsim is not installed" from "netsim dropped us" — and
// when the last reconnection was attempted.
let serverStatus = null, clientStatus = null;
let openedOnce = false;
let unreachable = false;  // a socket closed without ever opening: netsimd is not there
let restarting = false;   // a deliberate rebuild, waiting out the backoff
let lastAttempt = 0;

let serverHead = null, clientHead = null;
let serverGatt = null, clientGatt = null;
let setupPanel = null;

let running = false;

/// Reflects device state in the header: the dot is the state, the button is
/// the control. They were previously a static bullet and a Run button that
/// could only ever rebuild.
function setRunning(on) {
  running = on;
  serverHead?.setRunning(on);
  if (!on) clientHead?.setState(false, "stopped");
}

function startDevices() {
  // Over netsim, closing a socket and opening one under the same name in the
  // same breath is how ghosts are made: netsim does not synthesize a
  // disconnect, so the new socket can be attached to the entry the old one
  // left behind — the page looks connected while that device's tx count sits
  // at zero. Measured on this page: Apply rebuilding the pair in place left
  // two `web-generic-client` entries in `netsim devices`, one of them dead.
  // So a rebuild that has sockets to close hands the reopening to the loop's
  // backoff rather than racing it. Nothing to close means nothing to race.
  if (mode !== "in-page" && (serverDev || clientDev)) {
    release();
    restarting = true;
    lastAttempt = performance.now();
    setRunning(true);
    serverHead?.setState(false, waiting());
    clientHead?.setState(false, waiting());
    return;
  }
  try {
    build(serverHead.textarea.value, clientHead.textarea.value);
    setRunning(true);
  } catch (e) {
    serverHead?.setState(false, String(e).slice(0, 60), "bad");
    setRunning(false);
  }
}

/// Frees whichever backend is live. Not optional bookkeeping on netsim: a
/// device whose socket is dropped without a disconnect lingers as a ghost at
/// the same address, and the next socket under that name can attach to it.
function release() {
  for (const engine of [link, serverDev, clientDev]) {
    if (engine) { try { engine.free(); } catch (_) { /* already gone */ } }
  }
  link = serverDev = clientDev = null;
  clientIndex = -1;
  serverStatus = clientStatus = null;
}

/// Stopping frees both engines, so the devices genuinely leave the air rather
/// than merely being hidden.
function stopDevices() {
  release();
  openedOnce = false;
  unreachable = false;
  restarting = false;
  setRunning(false);
  setupPanel?.classList.remove("visible");
  serverHead?.setState(false, "stopped");
  clientGatt?.update({ services: [] });
}

/// Appends one line to the client's log — what the script emitted, and any
/// error its callbacks raised.
function logClient(text, bad) {
  const box = $("client-log");
  if (!box) return;
  const line = document.createElement("div");
  if (bad) line.className = "bad";
  line.textContent = text;
  box.append(line);
  while (box.childElementCount > 200) box.firstElementChild.remove();
  box.scrollTop = box.scrollHeight;
}

function build(serverScript, clientScript) {
  release();
  restarting = false;
  reportedFailure = null;
  t0 = performance.now();
  lastAttempt = performance.now();
  prevValues.clear();
  $("client-log").innerHTML = "";
  if (mode === "in-page") {
    link = new WebLink();
    serverIndex = link.add_peripheral(SERVER_ADDR, serverScript);
    // A client script error belongs beside the client's editor, not in the
    // server's status line: the message has to name the panel to fix. The
    // server still runs, so a broken client script leaves a working device to
    // reconnect to once it is fixed.
    try {
      clientIndex = link.add_scripted_central(CLIENT_ADDR, clientScript);
    } catch (e) {
      clientIndex = -1;
      logClient(String(e), true);
    }
  } else {
    // Two engines, two sockets, two controllers. Nothing in this tab carries
    // a packet between them — netsim does, which is the point of running here
    // at all: an Android emulator on the same rootcanal can connect to this
    // server, and the in-page link has no way to offer that.
    serverDev = new WebPeripheral(WS("web-generic-server", SERVER_NETSIM), serverScript);
    try {
      clientDev = new WebScriptedCentral(WS("web-generic-client", CLIENT_NETSIM), clientScript);
      // gatt_walker connects to EXAMPLE_PEER_ADDRESS, the in-page convention;
      // on netsim the server is at an address this page allocated, and
      // topology beats script.
      clientDev.set_target(SERVER_NETSIM);
    } catch (e) {
      clientDev = null;
      logClient(String(e), true);
    }
  }
  clientGatt?.update({ services: [] });
}

// nRF-Connect-style discovered-GATT tree from the central's status JSON.
/// The client's half of the view. The tree, its read/subscribe/write
/// buttons and its value decoding all come from the shared GATT view now --
/// this page had grown its own copy of that renderer, and a second copy is a
/// second thing to fix when discovery changes.
function renderClient(status) {
  clientHead?.setState(!!status.connected,
    status.connected ? `client · ${status.phase || "connected"}` : "connecting…",
    status.connected ? "ok" : "");
  clientGatt?.update(status);
}

/// The server's half of the view. Shared by both backends: what differs is
/// where the status came from, not what it means.
function renderServer(status) {
  renderGatt($("server-gatt"), status, prevValues);
  serverHead?.setName(status.name);
  serverHead?.setState(true,
    status.connected ? "on air · client connected" : "on air · advertising", "ok");
}

/// Whatever the client script said, and its verdict — from whichever engine
/// is hosting it.
function drainClient() {
  const emitted = mode === "in-page"
    ? link.scripted_central_emitted(clientIndex)
    : clientDev.emitted();
  for (const message of emitted) {
    const { event, payload } = JSON.parse(message);
    logClient(event === "log" ? String(payload) : `${event}: ${JSON.stringify(payload)}`);
  }
  // Sticky, so a failed assertion is reported once and stays readable
  // rather than being redrawn every 100 ms.
  const failure = mode === "in-page"
    ? link.scripted_central_failure(clientIndex)
    : clientDev.failure();
  if (failure && failure !== reportedFailure) {
    reportedFailure = failure;
    logClient(failure, true);
  }
}

/// Advances the devices. Separate from `loop`, and much more often, because an
/// ATT request and its response cross on successive ticks: at the rendering
/// rate the client visibly crawls through a two-service database, and the
/// cost of a tick is nothing beside the cost of re-rendering two panels.
///
/// On netsim a tick is also the socket's pump, so the statuses it returns are
/// stashed for `loop` rather than the engines being ticked twice.
function pump() {
  if (!running) return;
  const t = (performance.now() - t0) / 1000;
  if (mode === "in-page") {
    link?.tick(t);
    return;
  }
  try {
    if (serverDev?.ready_state() === 1) serverStatus = serverDev.tick(t);
    if (clientDev?.ready_state() === 1) clientStatus = clientDev.tick(t);
  } catch (e) {
    // A throwing engine is a dead engine. Hand it to the loop's backoff
    // rather than repeating the same failure fifty times a second.
    console.error("netsim tick:", e);
    release();
    lastAttempt = performance.now();
  }
}

function loop() {
  if (!running) return;
  if (mode === "in-page") loopInPage();
  else loopNetsim();
}

function loopInPage() {
  if (!link) return;
  try {
    renderServer(JSON.parse(link.peripheral_status_json(serverIndex)));
  } catch (e) { console.error("server render:", e); }
  if (clientIndex < 0) return;
  try {
    drainClient();
    renderClient(JSON.parse(link.central_status_json(clientIndex)));
  } catch (e) { console.error("client render:", e); }
}

/// What the server's status line says while there is no live pair: which of
/// the three ways to have no socket this is.
function waiting() {
  if (restarting) return "restarting…";
  if (unreachable) return "netsim not reachable — retrying…";
  return openedOnce ? "connection lost — reconnecting…" : "connecting to localhost:7681…";
}

/// The netsim backend's turn of the loop: mind the two sockets, then render
/// whatever the pump last got out of them.
function loopNetsim() {
  const now = performance.now();
  if (!serverDev || !clientDev) {
    serverHead?.setState(false, waiting(), unreachable ? "bad" : "");
    if (now - lastAttempt >= RECONNECT_MS) {
      try { build(serverHead.textarea.value, clientHead.textarea.value); }
      catch (e) { serverHead?.setState(false, String(e).slice(0, 60), "bad"); }
    }
    return;
  }
  // 0 connecting, 1 open, 2 closing, 3 closed. The pair is treated as one
  // scene: if either socket has gone, both are rebuilt, so the client never
  // sits hunting for a server that is no longer on the air.
  const states = [serverDev.ready_state(), clientDev.ready_state()];
  if (states.includes(3) || states.includes(2)) {
    // A socket that closed without ever opening means netsimd is not there:
    // that is a setup problem with an answer, not a device fault, so the
    // standard panel says how to fix it. A socket that opened and then went
    // is netsim restarting, which the backoff rides out.
    if (!openedOnce) {
      unreachable = true;
      setupPanel?.classList.add("visible");
    }
    serverHead?.setState(false, waiting(), "bad");
    clientHead?.setState(false, "disconnected");
    release();
    lastAttempt = now;
    return;
  }
  if (states.includes(0)) {
    serverHead?.setState(false, waiting());
    return;
  }
  openedOnce = true;
  unreachable = false;
  restarting = false;
  setupPanel?.classList.remove("visible");
  try {
    if (serverStatus) renderServer(JSON.parse(serverStatus));
  } catch (e) { console.error("server render:", e); }
  try {
    drainClient();
    if (clientStatus) renderClient(JSON.parse(clientStatus));
  } catch (e) { console.error("client render:", e); }
}

// --- lifecycle -------------------------------------------------------------

let timer = null;
let ticker = null;
let container = null;
// The catalog's pairing: gatt_walker is written for smart_lock, and both are
// the shared definitions MCP's `example` tool and the scene loader read.
let serverScript = "";
let clientScript = "";

/// Builds the domain into `root` and starts it.
export async function mount(root) {
  await init();

  if (!document.getElementById(STYLE_ID)) {
    const el = document.createElement("style");
    el.id = STYLE_ID;
    el.textContent = STYLE;
    document.head.append(el);
  }
  container = root;
  root.classList.add("domain", "two-up");
  root.innerHTML = MARKUP;
  (root.querySelector(".domain") ?? root).prepend(createAboutBox(ABOUT));

  serverScript = catalog_script("smart_lock");
  clientScript = catalog_script("gatt_walker");

  // The controller is the shell's choice, read once per mount: switching it
  // remounts the domain rather than mutating a running one.
  mode = currentController();
  setupPanel = $("setup");
  openedOnce = false;
  unreachable = false;
  restarting = false;

  // Both devices carry the same header every other domain uses: name, kind,
  // an address, a pen for the script and one run/stop. Generic was the last
  // page still drawing its own, from before the component existed.
  serverHead = createDeviceHeader({
    name: "Server", kind: "peripheral", accent: "good",
    address: mode === "in-page" ? SERVER_ADDR : SERVER_NETSIM,
    dotMeans: "the server is on the air",
    script: { text: serverScript, editable: true, open: true,
      note: "Edit and apply — the client rediscovers whatever you build.",
      onApply: () => startDevices() },
    // Both backends start and stop the pair together — one in-page link, or
    // one pair of sockets. Saying so beats a button that quietly does more
    // than it claims.
    run: { running: false,
           onRun: () => startDevices(),
           onStop: () => stopDevices() },
  });
  $("server-head").append(serverHead.el);
  $("server-script").append(serverHead.panel);

  clientHead = createDeviceHeader({
    name: "Client", kind: "central", accent: "accent",
    address: mode === "in-page" ? CLIENT_ADDR : CLIENT_NETSIM,
    dotMeans: "the client is connected to the server",
    script: { text: clientScript, editable: true,
      note: "The central script this page is running — <code>android::BluetoothGatt</code>.",
      onApply: () => startDevices() },
    run: { running: false, disabled: true,
           reason: mode === "in-page"
             ? "one in-page link — the pair starts and stops together"
             : "the server's toggle drives the pair" },
  });
  $("client-head").append(clientHead.el);
  $("client-script").append(clientHead.panel);

  serverGatt = createGattView({ mode: "server" });
  $("server-gatt").append(serverGatt.el);

  // The client's tree drives real operations: each button issues the call
  // through the scripted central, joining the same queue its script uses.
  // Which engine hosts that central is the backend's business, not the
  // button's.
  clientGatt = createGattView({
    mode: "client", empty: "Connecting…",
    // The view hands back the characteristic it drew, not a UUID string --
    // which is also what says whether it is already subscribed, so the bell
    // is a toggle rather than a one-way switch.
    onRead: (c) => {
      if (mode === "in-page") link?.scripted_central_read(clientIndex, c.uuid);
      else clientDev?.read(c.uuid);
      logClient(`client.read(${c.uuid})`);
    },
    onSubscribe: (c) => {
      const enable = !c.subscribed;
      if (mode === "in-page") link?.scripted_central_subscribe(clientIndex, c.uuid, enable);
      else clientDev?.subscribe(c.uuid, enable);
      logClient(`client.${enable ? "subscribe" : "unsubscribe"}(${c.uuid})`);
    },
    onWrite: (c) => {
      const bytes = promptForBytes();
      if (!bytes) return;
      if (mode === "in-page") link?.scripted_central_write(clientIndex, c.uuid, bytes);
      else clientDev?.write(c.uuid, bytes, true);
      const hex = Array.from(bytes, (b) => "0x" + b.toString(16).padStart(2, "0")).join(", ");
      logClient(`client.write(${c.uuid}, [${hex}])`);
    },
  });
  $("client-gatt").append(clientGatt.el);

  startDevices();
  timer = setInterval(loop, 100);
  ticker = setInterval(pump, 20);
}

/// Releases everything this domain owns: the shell hosts one domain at a
/// time, so a timer or a live engine left behind here is a leak — and on
/// netsim a socket dropped without a disconnect leaves a ghost device at the
/// same address for the next mount to collide with.
export function unmount() {
  if (timer !== null) {
    clearInterval(timer);
    timer = null;
  }
  if (ticker !== null) {
    clearInterval(ticker);
    ticker = null;
  }
  running = false;
  release();
  openedOnce = false;
  unreachable = false;
  restarting = false;
  setupPanel = null;
  serverGatt?.destroy();
  clientGatt?.destroy();
  serverHead?.destroy();
  clientHead?.destroy();
  serverGatt = clientGatt = serverHead = clientHead = null;
  document.getElementById(STYLE_ID)?.remove();
  if (container) {
    container.classList.remove("domain", "two-up");
    container.innerHTML = "";
  }
  container = null;
}
