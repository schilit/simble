// SimBLE Server + Client dual view. Two real devices share one in-page wasm
// Link, and **both are scripts**: a peripheral (android::BluetoothGattServer)
// on the left, a central (android::BluetoothGatt) on the right. The left panel
// renders the server's own database; the right renders what the client
// discovered — the nRF-Connect flow.
//
// The client panel used to display the page's own WebLink calls, because
// central-role scripting did not exist. It does now, so the panel is an
// editor like the other one and the two halves are the same kind of thing.

import init, { WebLink } from "../pkg/simble.js";
import { renderGatt, nameFor, propChips, escapeHtml, decodeValue } from "../common/viewer.js";

const STYLE_ID = "simble-generic-style";

const STYLE = `main { max-width: 78rem; margin: 0 auto; padding: 1rem 1.25rem 2rem; }
  .intro { color: var(--dim); font-size: 0.9rem; margin: 0 0 1rem; max-width: 60rem; }
  .two { display: grid; grid-template-columns: 1fr 1fr; gap: 1.1rem; align-items: start; }
  @media (max-width: 58rem) { .two { grid-template-columns: 1fr; } }
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

const MARKUP = `<div class="domain-status"><span id="conn" class="pill">starting…</span></div>
<p class="intro">This page runs <strong>two real SimBLE devices</strong> in the same tab, sharing one in-process
    <a href="../controllers/">in-browser controller</a> — no netsim. On the left, a <strong class="role" style="color:var(--good)">Server</strong>
    (peripheral) defined by an editable Rhai script. On the right, a <strong style="color:var(--accent)">Client</strong>
    (central) defined by another one — <code>android::BluetoothGatt</code>, Android's client API — that connects,
    discovers its GATT, and reacts in callbacks the way nRF Connect shows a connected device. What looks like
    "a device and a UI" is really a peripheral and a central talking to each other, and both halves are editable.</p>

  <div class="two">
    <section class="device server">
      <header>
        <span class="role"><i class="dot" id="server-dot"></i>Server</span>
        <span class="kind">peripheral · Rhai script</span>
        <button class="icon" id="server-pen" aria-pressed="true" title="Show or hide the script">✎</button>
        <button class="icon" id="run" title="Run or stop both devices">■</button>
        <span class="addr" id="server-addr">—</span>
      </header>
      <div class="body">
        <div id="server-script">
          <textarea id="script" spellcheck="false"></textarea>
          <span id="run-state" class="hint"></span>
        </div>
        <h2 class="sub">Its GATT database (server view)</h2>
        <div id="server-gatt"></div>
      </div>
    </section>

    <section class="device client">
      <header>
        <span class="role"><i class="dot" id="client-dot"></i>Client</span>
        <span class="kind">central · Rhai script</span>
        <button class="icon" id="client-pen" aria-pressed="true" title="Show or hide the script">✎</button>
        <span class="addr" id="client-addr">—</span>
      </header>
      <div class="body">
        <div id="client-script">
          <textarea id="client-script-text" spellcheck="false"></textarea>
          <p class="hint">Android's <code>BluetoothGatt</code> and its callbacks, in Rhai.
             Press ▶ to run both devices with your edits. <code>assert(...)</code> inside a
             callback makes this a test — a failure shows below.</p>
          <div id="client-log"></div>
        </div>
        <div class="phase">Connection: <b id="client-phase">idle</b> · peer <span id="client-peer">—</span></div>
        <h2 class="sub">Discovered services (client view)</h2>
        <div id="client-gatt"><p class="empty">Connecting…</p></div>
      </div>
    </section>
  </div>`;

const $ = (id) => document.getElementById(id);
const SERVER_ADDR = "AA:BB:CC:00:00:01";
const CLIENT_ADDR = "AA:BB:CC:00:00:02";

// The client, as a script. Every call below exists -- this is the same
// android::BluetoothGatt surface the MCP add_central tool and the interop
// tests use, not an illustration.
const DEFAULT_CLIENT_SCRIPT = `// The client: Android's BluetoothGatt, in Rhai.
let client = android::BluetoothGatt("HRM Client");
client.connect("${SERVER_ADDR}");

// Discovery is what makes UUIDs usable, so subscribe from here rather than
// at top level -- before it finishes the peer's handles are unknown.
fn on_services_discovered(client) {
    client.emit("log", "discovered " + client.services().len() + " services");
    client.subscribe(uuid::HEART_RATE_MEASUREMENT);
    client.read(uuid::BATTERY_LEVEL);
}

fn on_characteristic_read(client, uuid, value) {
    // value is a blob; Battery Level is one byte of percent.
    client.emit("log", "read " + uuid.to_string() + " = " + value[0]);
}

// assert() inside a callback is what makes a client script a test.
fn on_characteristic_changed(client, uuid, value) {
    assert(value[1] > 30 && value[1] < 220, "a plausible heart rate");
}
`;

const DEFAULT_SCRIPT = `// The server the client will connect to and discover.
let server = android::BluetoothGattServer("HRM Server");

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

let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
let lvl = android::BluetoothGattCharacteristic(
    uuid::BATTERY_LEVEL, android::PROPERTY_READ, android::PERMISSION_READ);
lvl.set_value([100]);
bas.add_characteristic(lvl);
server.add_service(bas);

fn tick(server, t) {
    let bpm = 76 + (12.0 * sin(t / 4.0)).to_int();
    server.update_value(uuid::HEART_RATE_MEASUREMENT, [0x00, bpm]);
}
`;

let link = null;
let serverIndex = 0;
let clientIndex = 1;
let t0 = performance.now();
const prevValues = new Map();
/// The last failure already written to the log, so it is not repeated.
let reportedFailure = null;

function setPill(text, cls) {
  const pill = $("conn");
  pill.textContent = text;
  pill.className = "pill" + (cls ? " " + cls : "");
}

let running = false;

/// Reflects device state in the header: the dot is the state, the button is
/// the control. They were previously a static bullet and a Run button that
/// could only ever rebuild.
function setRunning(on) {
  running = on;
  $("server-dot")?.classList.toggle("on", on);
  const btn = $("run");
  if (btn) {
    btn.textContent = on ? "■" : "▶";
    btn.title = on ? "Stop both devices (the link stops as a whole)" : "Run both devices";
  }
  if (!on) $("client-dot")?.classList.remove("on");
}

function startDevices() {
  try {
    build(editor.value, clientEditor.value);
    setRunning(true);
    $("run-state").textContent = "running — client reconnecting";
    setTimeout(() => ($("run-state").textContent = ""), 2500);
  } catch (e) {
    $("run-state").textContent = String(e);
    setRunning(false);
  }
}

/// Stopping frees the link, so both devices genuinely leave the air rather
/// than merely being hidden.
function stopDevices() {
  try { link?.free(); } catch (_) { /* already gone */ }
  link = null;
  setRunning(false);
  setPill("stopped", "");
  $("run-state").textContent = "stopped";
  $("client-gatt").innerHTML = '<p class="empty">Stopped.</p>';
  $("client-phase").textContent = "idle";
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
  $("server-addr").textContent = SERVER_ADDR;
  $("client-addr").textContent = CLIENT_ADDR;
  $("client-gatt").innerHTML = '<p class="empty">Connecting…</p>';
}

// nRF-Connect-style discovered-GATT tree from the central's status JSON.
function renderClient(status) {
  $("client-phase").textContent = status.phase || "idle";
  $("client-peer").textContent = status.peer || "—";
  const el = $("client-gatt");
  if (!status.connected) { el.innerHTML = '<p class="empty">Connecting…</p>'; return; }
  if (!status.services || !status.services.length) {
    el.innerHTML = `<p class="empty">Connected — ${escapeHtml(status.phase || "discovering")}…</p>`;
    return;
  }
  el.innerHTML = status.services.map((s) => {
    const sName = nameFor(s.uuid);
    const head = sName
      ? `${escapeHtml(sName)}<span class="u">0x${s.uuid}</span>`
      : `<span class="u">Service 0x${s.uuid}</span>`;
    const chrs = (s.characteristics || []).map((c) => {
      const cName = nameFor(c.uuid);
      const nameHtml = cName
        ? `<span class="chr-name">${escapeHtml(cName)}</span><span class="chr-uuid">0x${c.uuid}</span>`
        : `<span class="chr-name chr-uuid">0x${c.uuid}</span>`;
      const handle = "0x" + c.value_handle.toString(16).padStart(4, "0");
      const p = c.properties;
      const decoded = c.value ? decodeValue(c.uuid, c.value) : null;
      const valHtml = c.value
        ? `${decoded ? `<span class="decoded">${escapeHtml(decoded)}</span>` : ""}<span class="raw">${c.value}</span>`
        : `<span class="raw">— not read —</span>`;
      // The script surface names characteristics by UUID, so the buttons do
      // too — a click joins the same operation queue the script's own calls
      // use, rather than a second path with its own ordering.
      const actions = [];
      if (p & 0x02) actions.push(`<button data-op="read" data-u="${c.uuid}">read ↓</button>`);
      if (p & 0x0c) actions.push(`<button data-op="write" data-u="${c.uuid}">write ↑</button>`);
      if (p & 0x30) actions.push(
        `<button data-op="sub" data-u="${c.uuid}" class="${c.subscribed ? "on" : ""}">${c.subscribed ? "🔔 unsubscribe" : "subscribe 🔔"}</button>`);
      return `<div class="chr">
        <div class="chr-top">${nameHtml} ${propChips(p, c.subscribed)}
          <span class="chr-h">handle ${handle}</span></div>
        <div class="chr-val">${valHtml}</div>
        ${actions.length ? `<div class="chr-actions">${actions.join(" ")}</div>` : ""}
      </div>`;
    }).join("");
    return `<div class="svc"><div class="svc-head">${head}</div>${chrs}</div>`;
  }).join("");
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
    setPill(client.connected ? `client · ${client.phase}` : "client connecting…", client.connected ? "ok" : "");
    $("client-dot")?.classList.toggle("on", !!client.connected);
  } catch (e) { console.error("client render:", e); }
}

// --- lifecycle -------------------------------------------------------------

let timer = null;
let ticker = null;
let container = null;
let editor = null;
let clientEditor = null;

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
  root.innerHTML = MARKUP;

  editor = $("script");
  editor.value = DEFAULT_SCRIPT;

  // Client read / write / subscribe controls (delegated; the tree re-renders).
  $("client-gatt").addEventListener("click", (e) => {
    const btn = e.target.closest("button[data-op]");
    if (!btn || !link) return;
    const uuid = btn.dataset.u;
    if (btn.dataset.op === "read") {
      link.scripted_central_read(clientIndex, uuid);
      logClient(`client.read(${uuid})`);
    } else if (btn.dataset.op === "sub") {
      const on = btn.classList.contains("on");
      link.scripted_central_subscribe(clientIndex, uuid, !on);
      logClient(`client.${on ? "unsubscribe" : "subscribe"}(${uuid})`);
    } else if (btn.dataset.op === "write") {
      const input = prompt("Bytes to write (hex, space-separated, e.g. 00 5A):", "00");
      if (input == null) return;
      const bytes = input.trim().split(/\s+/).map((x) => parseInt(x, 16)).filter((n) => !Number.isNaN(n));
      link.scripted_central_write(clientIndex, uuid, new Uint8Array(bytes));
      logClient(`client.write(${uuid}, [${bytes.map((b) => "0x" + b.toString(16).padStart(2, "0")).join(", ")}])`);
    }
  });
  $("run").addEventListener("click", () => (running ? stopDevices() : startDevices()));

  // A pen shows or hides each side's script, so the panels can be read as
  // devices rather than as editors.
  for (const [pen, panel] of [["server-pen", "server-script"], ["client-pen", "client-script"]]) {
    $(pen).addEventListener("click", () => {
      const box = $(panel);
      box.hidden = !box.hidden;
      $(pen).setAttribute("aria-pressed", String(!box.hidden));
    });
  }

  clientEditor = $("client-script-text");
  clientEditor.value = DEFAULT_CLIENT_SCRIPT;
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
  document.getElementById(STYLE_ID)?.remove();
  if (container) container.innerHTML = "";
  container = null;
  editor = null;
  clientEditor = null;
}
