// SimBLE Server + Client dual view. Two real devices share one in-page wasm
// Link: a scripted peripheral (server) and a central (client) that connects to
// it and discovers its GATT. The left panel renders the server's own database;
// the right panel renders what the client discovered — the nRF-Connect flow.

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
#client-script-text { width: 100%; min-height: 8.5rem; }
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
    (peripheral) defined by the editable Rhai script. On the right, a <strong style="color:var(--accent)">Client</strong>
    (central) that connects to it and discovers its GATT, the way nRF Connect shows a connected device. What looks like
    "a device and a UI" is really a peripheral and a central talking to each other.</p>

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
        <span class="kind">central · Rust, not scripted</span>
        <button class="icon" id="client-pen" aria-pressed="false" title="Show or hide what the client is doing">✎</button>
        <span class="addr" id="client-addr">—</span>
      </header>
      <div class="body">
        <div id="client-script" hidden>
          <textarea id="client-script-text" spellcheck="false" readonly></textarea>
          <p class="hint"><strong>Not a script.</strong> The client is Rust (<code>CentralDevice</code>);
             SimBLE has no central-role scripting. These are the real <code>WebLink</code> binding
             calls this page issues, appended as they happen.</p>
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

// NOT a script, and deliberately not written as one. The client is Rust --
// CentralDevice -- and SimBLE has no central-role scripting, so any script
// shown here would be an invented API that no one can run. What it shows
// instead are the real binding calls this page makes, appended as they
// happen, which is the linkage between a button and its effect.
const CLIENT_TRACE_HEADER = `// The client is not scripted. It is CentralDevice (Rust), driven by this
// page through the WebLink bindings below -- these are the real function
// names, and each line is appended when the call is actually issued.
//
// On mount:
link.add_central(CLIENT_ADDR, SERVER_ADDR);   // connect + discover
//
// From the discovered tree:
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
    build(editor.value);
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

/// Appends one line to the client's live transcript. The point of the panel
/// is the linkage between a button and the call it makes, so a line only
/// appears because an operation was actually issued.
function traceClient(line) {
  const box = $("client-script-text");
  if (!box) return;
  box.value += line + "\n";
  box.scrollTop = box.scrollHeight;
}

function build(script) {
  if (link) { try { link.free(); } catch (_) { /* gone */ } }
  link = new WebLink();
  serverIndex = link.add_peripheral(SERVER_ADDR, script);
  clientIndex = link.add_central(CLIENT_ADDR, SERVER_ADDR);
  t0 = performance.now();
  prevValues.clear();
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
      const actions = [];
      if (p & 0x02) actions.push(`<button data-op="read" data-h="${c.value_handle}">read ↓</button>`);
      if (p & 0x0c) actions.push(`<button data-op="write" data-h="${c.value_handle}">write ↑</button>`);
      if (p & 0x30) actions.push(
        `<button data-op="sub" data-h="${c.value_handle}" class="${c.subscribed ? "on" : ""}">${c.subscribed ? "🔔 subscribed" : "subscribe 🔔"}</button>`);
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

function loop() {
  if (!link) return;
  if (!link) return;
  link.tick((performance.now() - t0) / 1000);
  try {
    const server = JSON.parse(link.peripheral_status_json(serverIndex));
    renderGatt($("server-gatt"), server, prevValues);
  } catch (e) { console.error("server render:", e); }
  try {
    const client = JSON.parse(link.central_status_json(clientIndex));
    renderClient(client);
    setPill(client.connected ? `client · ${client.phase}` : "client connecting…", client.connected ? "ok" : "");
    $("client-dot")?.classList.toggle("on", !!client.connected);
  } catch (e) { console.error("client render:", e); }
}

// --- lifecycle -------------------------------------------------------------

let timer = null;
let container = null;
let editor = null;

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
    const handle = parseInt(btn.dataset.h, 10);
    if (btn.dataset.op === "read") {
      link.central_read(clientIndex, handle);
      traceClient(`link.central_read(clientIndex, 0x${handle.toString(16).padStart(4, "0")});`);
    } else if (btn.dataset.op === "sub") {
      link.central_subscribe(clientIndex, handle);
      traceClient(`link.central_subscribe(clientIndex, 0x${handle.toString(16).padStart(4, "0")});`);
    } else if (btn.dataset.op === "write") {
      const input = prompt("Bytes to write (hex, space-separated, e.g. 00 5A):", "00");
      if (input == null) return;
      const bytes = input.trim().split(/\s+/).map((x) => parseInt(x, 16)).filter((n) => !Number.isNaN(n));
      link.central_write(clientIndex, handle, new Uint8Array(bytes));
      traceClient(`link.central_write(clientIndex, 0x${handle.toString(16).padStart(4, "0")}, [${
        bytes.map((b) => "0x" + b.toString(16).padStart(2, "0")).join(", ")}]);`);
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

  $("client-script-text").value = CLIENT_TRACE_HEADER;
  startDevices();
  timer = setInterval(loop, 100);
}

/// Releases everything this domain owns: the shell hosts one domain at a
/// time, so a timer or a live link left behind here is a leak.
export function unmount() {
  if (timer !== null) {
    clearInterval(timer);
    timer = null;
  }
  try { link?.free(); } catch (_) { /* already gone */ }
  link = null;
  document.getElementById(STYLE_ID)?.remove();
  if (container) container.innerHTML = "";
  container = null;
  editor = null;
}
