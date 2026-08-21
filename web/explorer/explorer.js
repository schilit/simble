// Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
//
// SimBLE API Explorer: a structured browser of the android::* API. Each method
// is a row you fill and Execute; Execute builds ONE Rhai statement from the
// form and evaluates it in a live WebSession's shared scope (the same engine
// the Playground runs as free text), then shows the generated line, the return
// value, and any events fired. Objects created by one Execute are let-bound
// (svc1, chr1, …) and become selectable inputs to later Executes — so you build
// a real, hostable device click by click.

import init, { WebSession } from "../pkg/simble.js";
import { renderGatt, escapeHtml } from "../common/viewer.js";
import { highlightRhai } from "../common/highlight.js";

const WS_URL =
  "ws://localhost:7681/v1/websocket/bt?name=web-explorer&address=CC:1E:57:00:00:04";

// --- option tables ---------------------------------------------------------
const UUID_OPTIONS = [
  ["Heart Rate Service (180D)", "uuid::HEART_RATE_SERVICE"],
  ["Heart Rate Measurement (2A37)", "uuid::HEART_RATE_MEASUREMENT"],
  ["Body Sensor Location (2A38)", "uuid::BODY_SENSOR_LOCATION"],
  ["Battery Service (180F)", "uuid::BATTERY_SERVICE"],
  ["Battery Level (2A19)", "uuid::BATTERY_LEVEL"],
  ["Client Char. Configuration / CCCD (2902)", "uuid::CLIENT_CHARACTERISTIC_CONFIGURATION"],
  ["Manufacturer Name (2A29)", "uuid::MANUFACTURER_NAME"],
  ["Model Number (2A24)", "uuid::MODEL_NUMBER"],
  ["Environmental Sensing (181A)", 'uuid::of("181A")'],
  ["Temperature (2A6E)", 'uuid::of("2A6E")'],
  ["Humidity (2A6F)", 'uuid::of("2A6F")'],
  ["Custom…", "__custom__"],
];

const PROPERTY_OPTIONS = [
  ["READ", "android::PROPERTY_READ"],
  ["WRITE", "android::PROPERTY_WRITE"],
  ["WRITE_NO_RESPONSE", "android::PROPERTY_WRITE_NO_RESPONSE"],
  ["NOTIFY", "android::PROPERTY_NOTIFY"],
  ["INDICATE", "android::PROPERTY_INDICATE"],
  ["BROADCAST", "android::PROPERTY_BROADCAST"],
];
const PERMISSION_OPTIONS = [
  ["READ", "android::PERMISSION_READ"],
  ["WRITE", "android::PERMISSION_WRITE"],
  ["READ_ENCRYPTED", "android::PERMISSION_READ_ENCRYPTED"],
  ["WRITE_ENCRYPTED", "android::PERMISSION_WRITE_ENCRYPTED"],
];
const SERVICE_TYPE_OPTIONS = [
  ["PRIMARY", "android::SERVICE_TYPE_PRIMARY"],
  ["SECONDARY", "android::SERVICE_TYPE_SECONDARY"],
];
const GATT_STATUS_OPTIONS = [
  ["GATT_SUCCESS", "android::GATT_SUCCESS"],
  ["GATT_FAILURE", "android::GATT_FAILURE"],
  ["GATT_READ_NOT_PERMITTED", "android::GATT_READ_NOT_PERMITTED"],
  ["GATT_WRITE_NOT_PERMITTED", "android::GATT_WRITE_NOT_PERMITTED"],
  ["GATT_REQUEST_NOT_SUPPORTED", "android::GATT_REQUEST_NOT_SUPPORTED"],
];

// A byte-array field: raw text like "0x00, 72" becomes [0x00, 72]; empty is
// () (the engine's "no payload"). Left as-is otherwise — the engine validates.
function bytesExpr(raw) {
  const t = (raw || "").trim();
  if (!t) return "()";
  return `[${t}]`;
}

// --- method registry -------------------------------------------------------
// receiver: null | 'server'|'service'|'characteristic' (an "on <object>" field)
// binds:    null | 'server'|'service'|'characteristic'|'descriptor'
// build(on, a): returns the right-hand-side / call expression (no trailing ;)
const METHODS = [
  // ---- Server ----
  { group: "Server", sig: 'BluetoothGattServer("name")', desc: "Create a GATT server — the device.",
    binds: "server",
    params: [{ key: "name", label: "name", kind: "text", default: "explorer-device" }],
    build: (_on, a) => `android::BluetoothGattServer(${JSON.stringify(a.name)})` },

  { group: "Server", sig: "server.add_service(svc)", desc: "Register a service on the server.",
    receiver: "server",
    params: [{ key: "svc", label: "service", kind: "objref", objType: "service" }],
    build: (on, a) => `${on}.add_service(${a.svc})` },

  { group: "Server", sig: "server.get_service(uuid)", desc: "Look up a registered service by UUID.",
    receiver: "server",
    params: [{ key: "uuid", label: "uuid", kind: "uuid" }],
    build: (on, a) => `${on}.get_service(${a.uuid})` },

  { group: "Server", sig: "server.update_value(uuid, bytes)",
    desc: "Web runtime: write a characteristic's value in the live DB (notifies subscribers).",
    receiver: "server",
    params: [
      { key: "uuid", label: "char uuid", kind: "uuid" },
      { key: "bytes", label: "bytes", kind: "bytes", default: "0x00, 72" },
    ],
    build: (on, a) => `${on}.update_value(${a.uuid}, ${bytesExpr(a.bytes)})` },

  { group: "Server", sig: "server.notify_characteristic_changed(device, chr, confirm, bytes)",
    desc: "Notify a connected central. Requires a device from a 'connected' event.",
    receiver: "server", needsDevice: true,
    params: [
      { key: "device", label: "device", kind: "objref", objType: "device" },
      { key: "chr", label: "characteristic", kind: "objref", objType: "characteristic" },
      { key: "confirm", label: "confirm", kind: "bool" },
      { key: "bytes", label: "bytes", kind: "bytes", default: "0x00, 72" },
    ],
    build: (on, a) => `${on}.notify_characteristic_changed(${a.device}, ${a.chr}, ${a.confirm}, ${bytesExpr(a.bytes)})` },

  { group: "Server", sig: "server.send_response(device, request_id, status, offset, bytes)",
    desc: "Answer a peer read/write request. Requires a device from an event.",
    receiver: "server", needsDevice: true,
    params: [
      { key: "device", label: "device", kind: "objref", objType: "device" },
      { key: "request_id", label: "request_id", kind: "number", default: "0" },
      { key: "status", label: "status", kind: "status" },
      { key: "offset", label: "offset", kind: "number", default: "0" },
      { key: "bytes", label: "bytes", kind: "bytes", default: "" },
    ],
    build: (on, a) => `${on}.send_response(${a.device}, ${a.request_id}, ${a.status}, ${a.offset}, ${bytesExpr(a.bytes)})` },

  // ---- Service ----
  { group: "Service", sig: "BluetoothGattService(uuid, type)", desc: "Create a service.",
    binds: "service",
    params: [
      { key: "uuid", label: "uuid", kind: "uuid" },
      { key: "type", label: "type", kind: "servicetype" },
    ],
    build: (_on, a) => `android::BluetoothGattService(${a.uuid}, ${a.type})` },

  { group: "Service", sig: "svc.add_characteristic(chr)", desc: "Add a characteristic to a service.",
    receiver: "service",
    params: [{ key: "chr", label: "characteristic", kind: "objref", objType: "characteristic" }],
    build: (on, a) => `${on}.add_characteristic(${a.chr})` },

  { group: "Service", sig: "svc.get_characteristic(uuid)", desc: "Look up a characteristic by UUID.",
    receiver: "service",
    params: [{ key: "uuid", label: "uuid", kind: "uuid" }],
    build: (on, a) => `${on}.get_characteristic(${a.uuid})` },

  // ---- Characteristic ----
  { group: "Characteristic", sig: "BluetoothGattCharacteristic(uuid, props, perms)",
    desc: "Create a characteristic.", binds: "characteristic",
    params: [
      { key: "uuid", label: "uuid", kind: "uuid" },
      { key: "props", label: "properties", kind: "flags", options: PROPERTY_OPTIONS, defaults: [0, 3] },
      { key: "perms", label: "permissions", kind: "flags", options: PERMISSION_OPTIONS, defaults: [0] },
    ],
    build: (_on, a) => `android::BluetoothGattCharacteristic(${a.uuid}, ${a.props}, ${a.perms})` },

  { group: "Characteristic", sig: "chr.set_value(bytes)", desc: "Set the characteristic's stored value.",
    receiver: "characteristic",
    params: [{ key: "bytes", label: "bytes", kind: "bytes", default: "0x00, 72" }],
    build: (on, a) => `${on}.set_value(${bytesExpr(a.bytes)})` },

  { group: "Characteristic", sig: "chr.add_descriptor(dsc)", desc: "Attach a descriptor (e.g. a CCCD).",
    receiver: "characteristic",
    params: [{ key: "dsc", label: "descriptor", kind: "objref", objType: "descriptor" }],
    build: (on, a) => `${on}.add_descriptor(${a.dsc})` },

  // ---- Descriptor ----
  { group: "Descriptor", sig: "BluetoothGattDescriptor(uuid, perms)",
    desc: "Create a descriptor (a CCCD makes a characteristic subscribable).", binds: "descriptor",
    params: [
      { key: "uuid", label: "uuid", kind: "uuid", presetIndex: 5 },
      { key: "perms", label: "permissions", kind: "flags", options: PERMISSION_OPTIONS, defaults: [0, 1] },
    ],
    build: (_on, a) => `android::BluetoothGattDescriptor(${a.uuid}, ${a.perms})` },
];

const NAME_PREFIX = { server: "srv", service: "svc", characteristic: "chr", descriptor: "dsc", device: "dev" };

// --- session + registry state ----------------------------------------------
const $ = (id) => document.getElementById(id);
let session = null;
let lastConnectAttempt = 0;
let openedOnce = false;
let startTime = performance.now();
const prevValues = new Map();

const registry = { server: [], service: [], characteristic: [], descriptor: [], device: [] };
const counters = { server: 0, service: 0, characteristic: 0, descriptor: 0, device: 0 };
const sessionLines = []; // successful lines, for "copy as script"

const peekName = (type) => `${NAME_PREFIX[type]}${counters[type] + 1}`;
function allocName(type) {
  counters[type] += 1;
  const name = `${NAME_PREFIX[type]}${counters[type]}`;
  registry[type].push(name);
  return name;
}

// --- form field rendering --------------------------------------------------
function uuidField(m, p) {
  const opts = UUID_OPTIONS.map(([label, expr], i) =>
    `<option value="${i}"${i === (p.presetIndex || 0) ? " selected" : ""}>${escapeHtml(label)}</option>`).join("");
  return `<select data-role="uuid-select">${opts}</select>
    <input type="text" class="custom-uuid" data-role="uuid-custom" placeholder='e.g. 2A6E or a 128-bit UUID' hidden>`;
}
function flagsField(p) {
  return `<div class="flags">${p.options.map(([label], i) =>
    `<label><input type="checkbox" data-role="flag" value="${i}"${(p.defaults || []).includes(i) ? " checked" : ""}> ${label}</label>`).join("")}</div>`;
}
function selectField(role, options) {
  return `<select data-role="${role}">${options.map(([label, expr]) =>
    `<option value="${escapeHtml(expr)}">${escapeHtml(label)}</option>`).join("")}</select>`;
}
function objrefField(p) {
  return `<select data-role="objref" data-objtype="${p.objType}"></select>`;
}

function fieldControl(m, p) {
  switch (p.kind) {
    case "text": return `<input type="text" data-role="text" value="${escapeHtml(p.default || "")}">`;
    case "number": return `<input type="number" data-role="number" value="${escapeHtml(p.default || "0")}">`;
    case "bytes": return `<input type="text" data-role="bytes" value="${escapeHtml(p.default || "")}" placeholder="e.g. 0x00, 72">`;
    case "uuid": return uuidField(m, p);
    case "flags": return flagsField(p);
    case "servicetype": return selectField("expr", SERVICE_TYPE_OPTIONS);
    case "status": return selectField("expr", GATT_STATUS_OPTIONS);
    case "bool": return `<select data-role="expr"><option value="false">false</option><option value="true">true</option></select>`;
    case "objref": return objrefField(p);
    default: return "";
  }
}

function renderMethods() {
  const groups = {};
  for (const m of METHODS) (groups[m.group] ||= []).push(m);
  const html = Object.entries(groups).map(([group, methods]) => `
    <div class="group"><h3>${escapeHtml(group)}</h3>
      ${methods.map((m, gi) => methodHtml(m, `${group}-${gi}`)).join("")}
    </div>`).join("");
  $("methods").innerHTML = html;
  // wire each method
  let idx = 0;
  for (const group of Object.values(groups)) {
    for (const m of group) wireMethod(m, $("methods").querySelectorAll(".method")[idx++]);
  }
}

function methodHtml(m, id) {
  const receiverField = m.receiver
    ? `<div class="field"><label>on</label><select data-role="receiver" data-objtype="${m.receiver}"></select></div>`
    : "";
  const fields = m.params.map((p) =>
    `<div class="field" data-key="${p.key}"><label>${escapeHtml(p.label)}</label><div>${fieldControl(m, p)}</div></div>`).join("");
  return `<details class="method"><summary><span class="sig">${escapeHtml(m.sig)}</span><span class="d">${escapeHtml(m.desc)}</span></summary>
    <div class="method-body">
      ${receiverField}${fields}
      <div class="exec-row">
        <code class="preview"></code>
        <button class="exec primary">Execute</button>
      </div>
      <div class="needs-device"></div>
    </div></details>`;
}

// Resolve one param's current Rhai expression from its DOM.
function resolveParam(fieldEl, p) {
  switch (p.kind) {
    case "text": return fieldEl.querySelector('[data-role=text]').value;
    case "number": return fieldEl.querySelector('[data-role=number]').value.trim() || "0";
    case "bytes": return fieldEl.querySelector('[data-role=bytes]').value;
    case "servicetype": case "status": case "bool":
      return fieldEl.querySelector('[data-role=expr]').value;
    case "objref": return fieldEl.querySelector('[data-role=objref]').value;
    case "flags": {
      const sel = [...fieldEl.querySelectorAll('[data-role=flag]:checked')]
        .map((el) => p.options[+el.value][1]);
      return sel.length ? sel.join(" | ") : "0";
    }
    case "uuid": {
      const which = fieldEl.querySelector('[data-role=uuid-select]').value;
      const opt = UUID_OPTIONS[+which][1];
      if (opt === "__custom__") {
        const raw = fieldEl.querySelector('[data-role=uuid-custom]').value.trim();
        return `uuid::of(${JSON.stringify(raw)})`;
      }
      return opt;
    }
    default: return "()";
  }
}

function collectArgs(bodyEl, m) {
  const a = {};
  for (const p of m.params) {
    const fieldEl = bodyEl.querySelector(`.field[data-key="${p.key}"]`);
    a[p.key] = resolveParam(fieldEl, p);
  }
  return a;
}

function buildLine(m, bodyEl, nameForBind) {
  const on = m.receiver ? bodyEl.querySelector('[data-role=receiver]').value : null;
  const expr = m.build(on, collectArgs(bodyEl, m));
  return m.binds ? `let ${nameForBind} = ${expr};` : `${expr};`;
}

// Whether all objref inputs the method needs have at least one choice.
function methodReady(m, bodyEl) {
  if (m.receiver && registry[m.receiver].length === 0) return false;
  for (const p of m.params) {
    if (p.kind === "objref" && registry[p.objType].length === 0) return false;
  }
  return true;
}

function wireMethod(m, el) {
  const body = el.querySelector(".method-body");
  const preview = el.querySelector(".preview");
  const execBtn = el.querySelector(".exec");
  const needs = el.querySelector(".needs-device");

  // custom-uuid reveal
  for (const sel of body.querySelectorAll('[data-role=uuid-select]')) {
    const custom = sel.parentElement.querySelector('[data-role=uuid-custom]');
    const sync = () => { custom.hidden = UUID_OPTIONS[+sel.value][1] !== "__custom__"; refresh(); };
    sel.addEventListener("change", sync);
  }

  function refresh() {
    const ready = methodReady(m, body);
    execBtn.disabled = !ready;
    if (!ready) {
      needs.textContent = m.needsDevice
        ? "needs a connected central (a device from a 'connected' event)"
        : "create the required object(s) first";
    } else {
      needs.textContent = "";
    }
    preview.innerHTML = highlightRhai(buildLine(m, body, m.binds ? peekName(m.binds) : null));
  }

  body.addEventListener("input", refresh);
  body.addEventListener("change", refresh);
  execBtn.addEventListener("click", () => execute(m, body));
  el._refresh = refresh;
  refresh();
}

// Repopulate every objref/receiver <select> from the registry. When no object
// of that type exists yet, show a visible placeholder (an empty <select> looks
// like a missing control) that names what to create first.
function refreshObjrefs() {
  for (const sel of $("methods").querySelectorAll('[data-role=objref], [data-role=receiver]')) {
    const type = sel.dataset.objtype;
    const prev = sel.value;
    if (registry[type].length) {
      sel.innerHTML = registry[type].map((n) => `<option>${n}</option>`).join("");
      sel.value = registry[type].includes(prev) ? prev : registry[type][registry[type].length - 1];
      sel.disabled = false;
    } else {
      sel.innerHTML = `<option value="" disabled selected>— create a ${type} first —</option>`;
      sel.disabled = true;
    }
  }
  for (const el of $("methods").querySelectorAll(".method")) el._refresh && el._refresh();
}

// --- execute ---------------------------------------------------------------
function logEntry(html) {
  const log = $("log");
  const div = document.createElement("div");
  div.className = "entry";
  div.innerHTML = html;
  log.prepend(div);
}

function execute(m, body) {
  const name = m.binds ? peekName(m.binds) : null;
  const line = buildLine(m, body, name);
  if (!session) { logEntry(`<div class="err">session not ready</div>`); return; }
  const res = JSON.parse(session.eval_line(line));
  if (res.ok) {
    if (m.binds) { allocName(m.binds); refreshObjrefs(); }
    sessionLines.push(line);
    const evt = (res.events && res.events.length)
      ? `<div class="evt">events: ${res.events.map(escapeHtml).join("; ")}</div>` : "";
    logEntry(`<div class="rhai">${highlightRhai(line)}</div><div class="ret">⇒ ${escapeHtml(res.value || "()")}</div>${evt}`);
    renderSession(JSON.parse(session.status_json()));
  } else {
    logEntry(`<div class="rhai">${highlightRhai(line)}</div><div class="err">✗ ${escapeHtml(res.error || "error")}</div>`);
  }
}

// --- session viewer + connection loop --------------------------------------
function setPill(text, cls) {
  const pill = $("conn");
  pill.textContent = text;
  pill.className = "pill" + (cls ? " " + cls : "");
}

function renderSession(status) {
  $("dev-name").textContent = status.name ? `${status.name} (${status.address})` : "no server yet";
  $("dev-conn").textContent = status.connected ? `connected to ${status.peer}` : (status.name ? "advertising" : "—");
  const names = Object.entries(registry).flatMap(([t, arr]) => arr.map((n) => `${n} (${t})`));
  $("registry").textContent = names.length ? names.join(", ") : "—";
  renderGatt($("gatt"), status, prevValues);
}

function loop() {
  if (!session) {
    const now = performance.now();
    if (now - lastConnectAttempt > 3000) {
      lastConnectAttempt = now;
      try { session = new WebSession(WS_URL); } catch (e) { console.error(e); }
    }
    return;
  }
  const state = session.ready_state();
  if (state === 3) {
    if (openedOnce) setPill("connection lost — building offline", "bad");
    else { setPill("offline (netsim not reachable)", "bad"); $("setup").classList.add("visible"); }
    // Keep the session object: building/inspecting still works offline. Just
    // render the current device from status_json.
    try { renderSession(JSON.parse(session.status_json())); } catch (_) { /* ignore */ }
    return;
  }
  if (state === 0) {
    setPill(openedOnce ? "reconnecting…" : "connecting to localhost:7681…", "");
    try { renderSession(JSON.parse(session.status_json())); } catch (_) { /* ignore */ }
    return;
  }
  openedOnce = true;
  $("setup").classList.remove("visible");
  try {
    const status = JSON.parse(session.tick((performance.now() - startTime) / 1000));
    setPill(status.name ? (status.connected ? "on air · central connected" : "on air · advertising") : "connected · no device yet", "ok");
    renderSession(status);
  } catch (e) {
    console.error(e);
  }
}

// --- boot ------------------------------------------------------------------
await init();
renderMethods();

$("copy-script").addEventListener("click", async () => {
  const header = "// Assembled in the SimBLE API Explorer — paste into the Playground.\n";
  const script = header + (sessionLines.length ? sessionLines.join("\n") + "\n" : "// (nothing executed yet)\n");
  try { await navigator.clipboard.writeText(script); $("copy-hint").textContent = "session script copied to clipboard"; }
  catch (_) { $("copy-hint").textContent = "copy failed — select and copy from the log"; }
  setTimeout(() => ($("copy-hint").textContent = ""), 4000);
});

$("reset-session").addEventListener("click", () => {
  for (const k of Object.keys(registry)) { registry[k] = []; counters[k] = 0; }
  sessionLines.length = 0;
  prevValues.clear();
  $("log").innerHTML = "New session. Create a server to begin.";
  try { session && session.free(); } catch (_) { /* gone */ }
  session = null; openedOnce = false; startTime = performance.now();
  renderMethods(); refreshObjrefs();
  renderSession({ name: "", services: [] });
});

renderSession({ name: "", services: [] });
setInterval(loop, 250);
