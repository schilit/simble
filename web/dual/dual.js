// SimBLE Server + Client dual view. Two real devices share one in-page wasm
// Link, and **both are scripts**: a peripheral (android::BluetoothGattServer)
// on the left, a central (android::BluetoothGatt) on the right. The left panel
// renders the server's own database; the right renders what the client
// discovered — the nRF-Connect flow.
//
// The client panel used to display the page's own WebLink calls, because
// central-role scripting did not exist. It does now, so the panel is an
// editor like the other one and the two halves are the same kind of thing.

import init, { WebLink, catalog_script } from "../pkg/simble.js";
import { createDeviceHeader } from "../common/device-header.js";
import { createGattView, promptForBytes } from "../common/gatt-view.js";
import { renderGatt, nameFor, propChips, escapeHtml, decodeValue } from "../common/viewer.js";
import { createAboutBox } from "../common/about-box.js";

const STYLE_ID = "simble-generic-style";

const STYLE = `main { max-width: 78rem; margin: 0 auto; padding: 1rem 1.25rem 2rem; }
  .intro { color: var(--dim); font-size: 0.9rem; margin: 0 0 1rem; max-width: 60rem; }
  .device { border: 1px solid var(--border); border-radius: 10px; background: var(--panel); overflow: hidden; }
  .device > header {
    display: flex; align-items: center; gap: 0.5rem; flex-wrap: wrap;
    padding: 0.6rem 0.9rem; border-bottom: 1px solid var(--border); background: #eaeef2;
  }
  .device > header .role { font-weight: 700; }
  .device > header .kind { color: var(--dim); font-size: 0.8rem; }
  .device > header .addr { margin-left: auto; font-family: ui-monospace, Menlo, monospace;
    color: var(--dim); font-size: 0.78rem; }
  .device .body { padding: 0.8rem 0.9rem; }
  .server .role { color: var(--good); }
  .client .role { color: var(--accent); }
  textarea#script {
    width: 100%; height: 12rem; resize: vertical; background: var(--bg); color: var(--text);
    border: 1px solid var(--border); border-radius: 6px; padding: 0.6rem; tab-size: 4;
    font: 12px/1.45 ui-monospace, SFMono-Regular, Menlo, monospace;
  }
  .row { display: flex; gap: 0.5rem; align-items: center; margin: 0.55rem 0; flex-wrap: wrap; }
  .hint { color: var(--dim); font-size: 0.8rem; }
  .phase { font-size: 0.82rem; color: var(--dim); }
  .phase b { color: var(--accent); }
  h2.sub { font-size: 0.78rem; text-transform: uppercase; letter-spacing: 0.06em;
    color: var(--dim); margin: 0.9rem 0 0.5rem; }
  /* Client (discovered) GATT — nRF-Connect-flavoured tree */
  .svc { border: 1px solid var(--border); border-radius: 8px; margin-bottom: 0.6rem; overflow: hidden; }
  .device header .dot { width: 9px; height: 9px; border-radius: 50%; display: inline-block;
  margin-right: 0.42rem; background-color: var(--dim); vertical-align: middle; }
.device.server header .dot.on { background-color: var(--good); }
.device.client header .dot.on { background-color: var(--accent); }
.device header .icon { border: 1px solid var(--border); background: transparent; border-radius: 6px;
  cursor: pointer; font-size: 0.85rem; line-height: 1; padding: 0.18rem 0.42rem; color: var(--dim);
  margin-left: 0.3rem; }
.device header .icon:hover { color: var(--fg); border-color: var(--fg); }
.device header .icon[aria-pressed="true"] { color: var(--fg); border-color: var(--fg); }
#client-script-text { width: 100%; height: 12rem; resize: vertical; background: var(--bg);
  color: var(--text); border: 1px solid var(--border); border-radius: 6px; padding: 0.6rem;
  tab-size: 4; font: 12px/1.45 ui-monospace, SFMono-Regular, Menlo, monospace; }
#client-log { margin-top: 0.4rem; max-height: 6rem; overflow-y: auto;
  font: 11px/1.5 ui-monospace, SFMono-Regular, Menlo, monospace; color: var(--dim); }
#client-log .bad { color: var(--bad, #b00); }
.svc-head { background: #eaeef2; padding: 0.4rem 0.7rem; font-size: 0.85rem; font-weight: 500; }
  .svc-head .u { font-family: ui-monospace, Menlo, monospace; color: var(--dim); font-size: 0.76rem; margin-left: 0.4rem; }
  .chr { padding: 0.45rem 0.7rem; border-top: 1px solid var(--border); }
  .chr-top { display: flex; align-items: baseline; gap: 0.5rem; flex-wrap: wrap; }
  .chr-name { font-weight: 500; }
  .chr-uuid { font-family: ui-monospace, Menlo, monospace; color: var(--dim); font-size: 0.75rem; }
  .chr-h { color: var(--dim); font-size: 0.72rem; margin-left: auto; font-family: ui-monospace, Menlo, monospace; }
  .prop { display: inline-block; border: 1px solid var(--border); border-radius: 4px; padding: 0 0.3rem;
    font-size: 0.66rem; color: var(--dim); font-family: ui-monospace, Menlo, monospace; }
  .prop.n, .prop.i { color: var(--good); border-color: var(--good); }
  .chr-val { margin-top: 0.2rem; font-size: 0.85rem; }
  .chr-val .decoded { font-weight: 600; }
  .chr-val .raw { font-family: ui-monospace, Menlo, monospace; color: var(--dim);
    font-size: 0.76rem; margin-left: 0.4rem; }
  .chr-actions { margin-top: 0.35rem; display: flex; gap: 0.35rem; flex-wrap: wrap; }
  .chr-actions button {
    background: #eaeef2; color: var(--text); border: 1px solid var(--border); border-radius: 5px;
    padding: 0.12rem 0.5rem; font-size: 0.74rem; cursor: pointer;
  }
  .chr-actions button:hover { border-color: var(--accent); }
  .chr-actions button.on { color: var(--good); border-color: var(--good); }
  .empty { color: var(--dim); font-size: 0.85rem; }
  footer { color: var(--dim); font-size: 0.78rem; padding: 1.25rem 1.25rem 0; max-width: 78rem; margin: 0 auto; }`;

const ABOUT = `<p>This page runs <strong>two real SimBLE devices</strong> in the same tab, sharing one in-process
    <a href="../controllers/">in-browser controller</a> — no netsim. On the left, a <strong class="role" style="color:var(--good)">Server</strong>
    (peripheral) defined by an editable Rhai script. On the right, a <strong style="color:var(--accent)">Client</strong>
    (central) defined by another one — <code>android::BluetoothGatt</code>, Android's client API — that connects,
    discovers its GATT, and reacts in callbacks the way nRF Connect shows a connected device. What looks like
    "a device and a UI" is really a peripheral and a central talking to each other, and both halves are editable.</p>`;

const MARKUP = `  <section class="panel">
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
const SERVER_ADDR = "AA:BB:CC:00:00:01";
const CLIENT_ADDR = "AA:BB:CC:00:00:02";

// The client, as a script. Every call below exists -- this is the same
// android::BluetoothGatt surface the MCP add_central tool and the interop
// tests use, not an illustration.
let link = null;
let serverIndex = 0;
let clientIndex = 1;
let t0 = performance.now();
const prevValues = new Map();
/// The last failure already written to the log, so it is not repeated.
let reportedFailure = null;

let serverHead = null, clientHead = null;
let serverGatt = null, clientGatt = null;

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
  try {
    build(serverHead.textarea.value, clientHead.textarea.value);
    setRunning(true);
  } catch (e) {
    serverHead?.setState(false, String(e).slice(0, 60), "bad");
    setRunning(false);
  }
}

/// Stopping frees the link, so both devices genuinely leave the air rather
/// than merely being hidden.
function stopDevices() {
  try { link?.free(); } catch (_) { /* already gone */ }
  link = null;
  setRunning(false);
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
  if (link) { try { link.free(); } catch (_) { /* gone */ } }
  link = new WebLink();
  serverIndex = link.add_peripheral(SERVER_ADDR, serverScript);
  reportedFailure = null;
  t0 = performance.now();
  prevValues.clear();
  $("client-log").innerHTML = "";
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

/// Advances the link. Separate from `loop`, and much more often, because an
/// ATT request and its response cross on successive ticks: at the rendering
/// rate the client visibly crawls through a two-service database, and the
/// cost of a tick is nothing beside the cost of re-rendering two panels.
function pump() {
  if (!link) return;
  link.tick((performance.now() - t0) / 1000);
}

function loop() {
  if (!link) return;
  try {
    const server = JSON.parse(link.peripheral_status_json(serverIndex));
    renderGatt($("server-gatt"), server, prevValues);
  } catch (e) { console.error("server render:", e); }
  if (clientIndex < 0) return;
  try {
    for (const message of link.scripted_central_emitted(clientIndex)) {
      const { event, payload } = JSON.parse(message);
      logClient(event === "log" ? String(payload) : `${event}: ${JSON.stringify(payload)}`);
    }
    // Sticky, so a failed assertion is reported once and stays readable
    // rather than being redrawn every 100 ms.
    const failure = link.scripted_central_failure(clientIndex);
    if (failure && failure !== reportedFailure) {
      reportedFailure = failure;
      logClient(failure, true);
    }
    const client = JSON.parse(link.central_status_json(clientIndex));
    renderClient(client);
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
  root.prepend(createAboutBox(ABOUT));

  serverScript = catalog_script("smart_lock");
  clientScript = catalog_script("gatt_walker");

  // Both devices carry the same header every other domain uses: name, kind,
  // an address, a pen for the script and one run/stop. Generic was the last
  // page still drawing its own, from before the component existed.
  serverHead = createDeviceHeader({
    name: "Server", kind: "peripheral", accent: "good",
    address: SERVER_ADDR,
    dotMeans: "the server is on the air",
    script: { text: serverScript, editable: true, open: true,
      note: "Edit and apply — the client rediscovers whatever you build.",
      onApply: () => startDevices() },
    // One in-page link hosts both, so a stop takes the pair with it. Saying
    // so beats a button that quietly does more than it claims.
    run: { running: false,
           onRun: () => startDevices(),
           onStop: () => stopDevices() },
  });
  $("server-head").append(serverHead.el);
  $("server-script").append(serverHead.panel);

  clientHead = createDeviceHeader({
    name: "Client", kind: "central", accent: "accent",
    address: CLIENT_ADDR,
    dotMeans: "the client is connected to the server",
    script: { text: clientScript, editable: true,
      note: "A real central script — <code>android::BluetoothGatt</code>.",
      onApply: () => startDevices() },
    run: { running: false, disabled: true,
           reason: "one in-page link — the pair starts and stops together" },
  });
  $("client-head").append(clientHead.el);
  $("client-script").append(clientHead.panel);

  serverGatt = createGattView({ mode: "server" });
  $("server-gatt").append(serverGatt.el);

  // The client's tree drives real operations: each button issues the call
  // through the scripted central, joining the same queue its script uses.
  clientGatt = createGattView({
    mode: "client", empty: "Connecting…",
    onRead: (uuid) => {
      link?.scripted_central_read(clientIndex, uuid);
      logClient(`client.read(${uuid})`);
    },
    onSubscribe: (uuid, enable) => {
      link?.scripted_central_subscribe(clientIndex, uuid, enable);
      logClient(`client.${enable ? "subscribe" : "unsubscribe"}(${uuid})`);
    },
    onWrite: (uuid) => {
      const bytes = promptForBytes();
      if (!bytes) return;
      link?.scripted_central_write(clientIndex, uuid, new Uint8Array(bytes));
      logClient(`client.write(${uuid}, [${bytes.map((b) => "0x" + b.toString(16).padStart(2, "0")).join(", ")}])`);
    },
  });
  $("client-gatt").append(clientGatt.el);

  startDevices();
  timer = setInterval(loop, 100);
  ticker = setInterval(pump, 20);
}

/// Releases everything this domain owns: the shell hosts one domain at a
/// time, so a timer or a live link left behind here is a leak.
export function unmount() {
  if (timer !== null) {
    clearInterval(timer);
    timer = null;
  }
  if (ticker !== null) {
    clearInterval(ticker);
    ticker = null;
  }
  try { link?.free(); } catch (_) { /* already gone */ }
  link = null;
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
