// Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
//
// SimBLE API Explorer: a reference for the android::* scripting surface that
// you drive instead of reading. Each member is a row you open, fill and
// Execute; Execute builds ONE Rhai statement from the form and evaluates it in
// a live WebSession's shared scope (the same engine the Playground runs as
// free text), then shows the generated line, the return value, and any events
// fired. Objects created by one Execute are let-bound (svc1, chr1, …) and
// become selectable inputs to later Executes — so you build a real, hostable
// device click by click.
//
// The page is shaped like an API reference for a reason. A bare list of forms
// tells you a call exists and nothing about it: what the third argument means,
// what comes back, why it is refusing to run. So every member here carries its
// full signature, prose, a parameter table (name · type · meaning) and its
// return value, and the form controls live *inside* that table next to the
// argument they set. Constants are documented with their values and what each
// bit means, not reduced to dropdown labels.
//
// Three kinds of member, and the difference is load-bearing:
//
//   exec  the default — the line runs in this session, right now.
//   ref   real API, but nothing on this page hosts it. `eval_line` re-collects
//         exactly one thing from the session scope — `ScriptGattServer`s — and
//         pumps those. Anything else evaluates, queues packets into an outbox,
//         and has no one drain it: `android::BluetoothGatt`, the HID host and
//         the Assistant (all centrals), and `BluetoothLeBroadcast` (not a
//         central at all — it is simply not in the list `eval_line` collects).
//         A green "⇒ ()" would be a lie, so these get Copy instead of Execute.
//   doc   a callback you define, custom syntax, or a constant family. There is
//         no call to build, so there is no form — faking one would invent an
//         API.
//
// These labels are load-bearing and they are not a to-do list. Deleting one
// does not make the call work; it makes the page lie. docs/gaps.md §4 exists
// to say so.
//
// Everything below was read off the Rust rather than remembered: the peripheral
// surface from src/scripting/bindings.rs, the web-only additions from
// src/transport/wasm_ws.rs (register_web_extensions), the central from
// src/scripting/client.rs, the HID host from src/scripting/hid.rs, both
// Auracast proxies and the BASS registrars from src/scripting/broadcast.rs,
// `catalog::*` from src/scripting/catalog.rs, `assert_over`/`advance` from
// src/scripting/monitor.rs, and the constants from src/scripting/constants.rs
// and the android layer they alias. Each member names its source file, so the
// next person can check a claim without grepping.
//
// And it stays that way mechanically: tests/explorer_surface_test.rs walks
// those same registration sites and fails if a registered name has no entry
// here. This page went stale once because nothing checked it; that is now CI's
// job rather than a reader's.

import init, { WebSession } from "../pkg/simble.js";
import { renderGatt, escapeHtml } from "../common/viewer.js";
import { highlightRhai } from "../common/highlight.js";

const WS_URL =
  "ws://localhost:7681/v1/websocket/bt?name=web-explorer&address=CC:1E:57:00:00:04";

// --- option tables ---------------------------------------------------------
// Named UUIDs worth reaching for from a dropdown. The full uuid:: dictionary is
// documented in the Constants section; this is the shortlist you actually build
// devices out of, plus an escape hatch.
const UUID_OPTIONS = [
  ["Heart Rate Service (180D)", "uuid::HEART_RATE_SERVICE"],
  ["Heart Rate Measurement (2A37)", "uuid::HEART_RATE_MEASUREMENT"],
  ["Body Sensor Location (2A38)", "uuid::BODY_SENSOR_LOCATION"],
  ["Battery Service (180F)", "uuid::BATTERY_SERVICE"],
  ["Battery Level (2A19)", "uuid::BATTERY_LEVEL"],
  ["Client Char. Configuration / CCCD (2902)", "uuid::CLIENT_CHARACTERISTIC_CONFIGURATION"],
  ["Characteristic User Description (2901)", "uuid::CHARACTERISTIC_USER_DESCRIPTION"],
  ["Manufacturer Name (2A29)", "uuid::MANUFACTURER_NAME"],
  ["Model Number (2A24)", "uuid::MODEL_NUMBER"],
  ["Firmware Revision (2A26)", "uuid::FIRMWARE_REVISION"],
  ["Ranging Service (185B)", "uuid::RANGING_SERVICE"],
  ["PACS Service (1850)", "uuid::PACS_SERVICE"],
  ["Environmental Sensing (181A)", 'uuid::of("181A")'],
  ["Temperature (2A6E)", 'uuid::of("2A6E")'],
  ["Humidity (2A6F)", 'uuid::of("2A6F")'],
  ["Custom…", "__custom__"],
];
// Referenced by name rather than by index: the list grows, and a stale
// presetIndex silently picks the wrong UUID.
const uuidIndex = (expr) => UUID_OPTIONS.findIndex(([, e]) => e === expr);

// [label, expression, value, what the bit means]. The value and the meaning are
// carried here rather than in a separate doc table so a checkbox and its
// documentation cannot drift apart.
const PROPERTY_OPTIONS = [
  ["BROADCAST", "android::PROPERTY_BROADCAST", "0x01",
   "May be broadcast in advertising data (needs a Server Characteristic Configuration descriptor)."],
  ["READ", "android::PROPERTY_READ", "0x02", "A central may read the value."],
  ["WRITE_NO_RESPONSE", "android::PROPERTY_WRITE_NO_RESPONSE", "0x04",
   "Accepts Write Command — unacknowledged, no response on the wire."],
  ["WRITE", "android::PROPERTY_WRITE", "0x08",
   "Accepts Write Request — acknowledged with a Write Response."],
  ["NOTIFY", "android::PROPERTY_NOTIFY", "0x10",
   "May be notified. Needs a CCCD (2902) before a central can turn it on."],
  ["INDICATE", "android::PROPERTY_INDICATE", "0x20",
   "May be indicated — like notify, but the central confirms each one."],
  ["SIGNED_WRITE", "android::PROPERTY_SIGNED_WRITE", "0x40",
   "Accepts an authenticated signed write."],
  ["EXTENDED_PROPS", "android::PROPERTY_EXTENDED_PROPS", "0x80",
   "More properties live in the Characteristic Extended Properties descriptor."],
];
const PERMISSION_OPTIONS = [
  ["READ", "android::PERMISSION_READ", "0x01", "Readable with no security requirement."],
  ["READ_ENCRYPTED", "android::PERMISSION_READ_ENCRYPTED", "0x02",
   "Readable only on an encrypted link."],
  ["READ_ENCRYPTED_MITM", "android::PERMISSION_READ_ENCRYPTED_MITM", "0x04",
   "Readable only on an encrypted, authenticated (MITM-protected) link."],
  ["WRITE", "android::PERMISSION_WRITE", "0x10", "Writable with no security requirement."],
  ["WRITE_ENCRYPTED", "android::PERMISSION_WRITE_ENCRYPTED", "0x20",
   "Writable only on an encrypted link."],
  ["WRITE_ENCRYPTED_MITM", "android::PERMISSION_WRITE_ENCRYPTED_MITM", "0x40",
   "Writable only on an encrypted, authenticated link."],
  ["WRITE_SIGNED", "android::PERMISSION_WRITE_SIGNED", "0x80",
   "Accepts a signed write on an unencrypted link."],
  ["WRITE_SIGNED_MITM", "android::PERMISSION_WRITE_SIGNED_MITM", "0x100",
   "Accepts a signed write from an authenticated peer."],
];
const SERVICE_TYPE_OPTIONS = [
  ["SERVICE_TYPE_PRIMARY (0)", "android::SERVICE_TYPE_PRIMARY"],
  ["SERVICE_TYPE_SECONDARY (1)", "android::SERVICE_TYPE_SECONDARY"],
];
const GATT_STATUS_OPTIONS = [
  ["GATT_SUCCESS (0)", "android::GATT_SUCCESS"],
  ["GATT_READ_NOT_PERMITTED (2)", "android::GATT_READ_NOT_PERMITTED"],
  ["GATT_WRITE_NOT_PERMITTED (3)", "android::GATT_WRITE_NOT_PERMITTED"],
  ["GATT_INSUFFICIENT_AUTHENTICATION (5)", "android::GATT_INSUFFICIENT_AUTHENTICATION"],
  ["GATT_REQUEST_NOT_SUPPORTED (6)", "android::GATT_REQUEST_NOT_SUPPORTED"],
  ["GATT_INVALID_OFFSET (7)", "android::GATT_INVALID_OFFSET"],
  ["GATT_INSUFFICIENT_AUTHORIZATION (8)", "android::GATT_INSUFFICIENT_AUTHORIZATION"],
  ["GATT_INVALID_ATTRIBUTE_LENGTH (13)", "android::GATT_INVALID_ATTRIBUTE_LENGTH"],
  ["GATT_INSUFFICIENT_ENCRYPTION (15)", "android::GATT_INSUFFICIENT_ENCRYPTION"],
  ["GATT_CONNECTION_CONGESTED (143)", "android::GATT_CONNECTION_CONGESTED"],
  ["GATT_FAILURE (257)", "android::GATT_FAILURE"],
];

// A byte-array field: raw text like "0x00, 72" becomes [0x00, 72]; empty is
// () (the engine's "no payload"). Left as-is otherwise — the engine validates.
function bytesExpr(raw) {
  const t = (raw || "").trim();
  if (!t) return "()";
  return `[${t}]`;
}

const BYTES_DOC =
  "Bytes, written the way Rhai writes them: <code>0x00, 72</code> becomes <code>[0x00, 72]</code>. " +
  "Leave it empty to pass <code>()</code>, which the bindings read as “no payload”.";
const UUID_DOC =
  "A <code>Uuid</code>. Pick a named constant, or choose <em>Custom…</em> to get " +
  "<code>uuid::of(\"…\")</code>, which takes a 16-bit assigned number (<code>\"2A6E\"</code>) " +
  "or a full 128-bit string.";

// Android's callbacks carry a `BluetoothStatusCodes` reason. There is no
// mapping from an HCI status or an ATT error to one, so ours carry the
// controller's or the peer's own status byte instead of an invented constant —
// which is why this sentence is shared rather than restated per callback.
const REASON_DOC =
  "The controller's or the peer's own status byte, <code>0</code> on success. Android would pass a " +
  "<code>BluetoothStatusCodes</code> value here; there is no mapping from an HCI status or an ATT " +
  "error to one, so this is the real status rather than an invented constant.";
const SINK_ARG_DOC = "The Delegator's address, as a string.";
const SOURCE_ID_DOC = "The Source_ID BASS assigned to this source.";

// --- types -----------------------------------------------------------------
// Sections, in reading order: the peripheral you can build, the value types it
// is built from, the central, then the parts that are documentation only.
const TYPES = [
  { id: "session", short: "Session", name: "Session", role: "engine",
    blurb: "Free functions every script sees, registered on the engine itself rather than on an object. " +
      "<code>assert</code> is what turns a script into a test." },
  { id: "catalog", short: "Catalog", name: "catalog::*", role: "peripheral",
    blurb: "The shipped devices, by name. <code>catalog::device(\"hrm\")</code> compiles and runs a " +
      "catalog entry <em>in this very engine</em>, so what comes back is an ordinary " +
      "<code>BluetoothGattServer</code> — the load <strong>is</strong> the peripheral being added. " +
      "It is the fastest thing on this page: one Execute and you are hosting a real device, with its " +
      "<code>fn tick</code> carried along so <code>advance</code> and <code>assert_over</code> can run " +
      "its physics." },
  { id: "server", short: "Server", name: "android::BluetoothGattServer", role: "peripheral",
    blurb: "The device. Android's <code>BluetoothGattServer</code>, wrapping a <code>VirtualDevice</code> " +
      "with a real GATT database. Its address is allocated per engine session " +
      "(<code>F0:DE:C0:00:00:NN</code>) so identical scripts produce identical devices. " +
      "The moment one exists in the session scope, this page hosts it: it advertises, accepts " +
      "connections and notifies." },
  { id: "advertising", short: "Advertising", name: "Advertising", role: "peripheral",
    blurb: "What the device puts on the air before anyone connects. All of these stage data that is " +
      "folded into the advertisement at bring-up, so the Explorer re-issues advertising when they change." },
  { id: "profiles", short: "Profiles", name: "Profile registrars", role: "peripheral",
    blurb: "Whole profiles, implemented in Rust and installed into the device's database in one call. " +
      "The protocol — the state machine, its tests — stays in Rust; the script just composes a device " +
      "out of them. Services registered this way live in the GATT database rather than the script's " +
      "service list, so they need <code>advertise_service_uuid</code> to be discoverable." },
  { id: "service", short: "Service", name: "android::BluetoothGattService", role: "peripheral",
    blurb: "A group of characteristics with a UUID. Built free-standing, then handed to a server — " +
      "<code>add_service</code> takes it by value, so changes made after that do not reach the " +
      "registered copy." },
  { id: "characteristic", short: "Characteristic", name: "android::BluetoothGattCharacteristic",
    role: "peripheral",
    blurb: "One attribute: a UUID, a value, what a peer may do with it (properties) and what security " +
      "that requires (permissions)." },
  { id: "descriptor", short: "Descriptor", name: "android::BluetoothGattDescriptor", role: "peripheral",
    blurb: "Metadata attached to a characteristic. The one that matters is the CCCD (2902): a " +
      "notify-capable characteristic without one has nothing for a central to write, so it can never " +
      "be subscribed." },
  { id: "device", short: "Device", name: "BluetoothDevice", role: "value",
    blurb: "A peer, as seen from the server side. Scripts never construct one — it arrives on an event, " +
      "and it is the handle the server-side notify and response calls address." },
  { id: "uuid", short: "Uuid", name: "Uuid", role: "value",
    blurb: "16-bit or 128-bit, one type. Comparable and printable; the <code>uuid::</code> module holds " +
      "the named constants and the two escape hatches." },
  { id: "client", short: "Central", name: "android::BluetoothGatt", role: "central",
    blurb: "The other half of GATT: the thing that connects, discovers, reads, writes and subscribes. " +
      "Android's <code>BluetoothGatt</code>, and the <code>on_*</code> functions are its " +
      "<code>BluetoothGattCallback</code>. <strong>This page hosts servers only</strong>, so a central " +
      "built here would queue packets nobody sends — every member below is documented and copyable " +
      "rather than executable. Run them in the Playground, a scene, or <code>run_test</code>." },
  { id: "hid", short: "HID host", name: "android::BluetoothHidHost", role: "central",
    blurb: "A central that knows what a HID peer is — the HOGP half of Android's " +
      "<code>BluetoothHidHost</code> proxy. It composes <code>BluetoothGatt</code> for the connection " +
      "and Rust's <code>HidHost</code> for the protocol, so discovery is automatic: once services are " +
      "found it reads every Report Map and subscribes to every notifying Report on its own. " +
      "<strong>No report is decoded in Rhai</strong> — the script sees the result. Being a central, " +
      "every member below is reference-only here for the same reason <code>BluetoothGatt</code> is." },
  { id: "broadcast", short: "Broadcast", name: "android::BluetoothLeBroadcast", role: "broadcast",
    blurb: "The Auracast source: the whole advertising → periodic advertising → BIG ladder, driven by " +
      "Rust's <code>BigBroadcaster</code>. No HCI type crosses into Rhai — you hand it a settings map " +
      "and it hands you a <code>BluetoothLeBroadcastMetadata</code>-shaped map back. " +
      "<strong>Reference-only here</strong>: this session re-collects only the " +
      "<code>BluetoothGattServer</code>s in scope, so a source built on this page would queue its " +
      "setup packets into an outbox nothing flushes. The Broadcast page hosts one for real." },
  { id: "assistant", short: "Assistant", name: "android::BluetoothLeBroadcastAssistant", role: "central",
    blurb: "The phone half of Auracast: pure GATT, writing BASS Add/Modify/Remove Source to a Scan " +
      "Delegator's control point and reading the Broadcast Receive State back. It is a central " +
      "underneath — it borrows the whole central binding — so it is reference-only here, and it has no " +
      "<code>connect</code>: like Android's framework-owned proxy, the first call that names a sink " +
      "opens the link." },
  { id: "callbacks", short: "Callbacks", name: "Callbacks", role: "reference",
    blurb: "Functions the script defines and the runtime calls. Not closures assigned to a callback " +
      "object, as Android would: this Rhai build is non-<code>sync</code>, so dispatch goes by name and " +
      "arity instead. A handler is recognised only at the exact arity shown. Handlers are pure — they " +
      "cannot see the calling scope — so <code>this</code>, a per-device map, is the only memory they have." },
  { id: "events", short: "Events", name: "Event maps", role: "reference",
    blurb: "The object map an event arrives as. Absent fields are omitted rather than set to " +
      "<code>()</code>, so <code>\"uuid\" in event</code> reads naturally." },
  { id: "constants", short: "Constants", name: "Constants", role: "reference",
    blurb: "Every constant the <code>android</code> and <code>uuid</code> modules publish, with its " +
      "value and what it means. They are static-module constants, which Rhai keeps genuinely immutable, " +
      "and each one aliases a Rust const rather than restating a number." },
];

// --- member registry -------------------------------------------------------
// kind:      'ctor' | 'prop' | 'method' | 'callback' | 'syntax' | 'consts'
// mode:      undefined (executable) | 'ref' (copy only) | 'doc' (no form)
// receiver:  null | an object type — renders the "on" selector
// binds:     null | the object type the call returns, let-bound on success
// params[].type/doc: what the parameter table shows
// build(on, a): the call expression, no trailing ';'
const METHODS = [
  // ---- Session ----
  { group: "session", kind: "method", sig: "assert(condition, message)", ret: "()",
    desc: "Fail the run if the condition is false.",
    prose: "The host function that makes a script a test: on a false condition it throws a Rhai runtime " +
      "error carrying the message, which fails the line here, fails <code>run_test</code>, and — inside " +
      "a callback — is kept forever as the run's verdict.",
    params: [
      // A default that cannot fail on a fresh page: the interesting form
      // (srv1.name == "…") needs an object that may not exist yet, and a
      // "variable not found" as the first thing anyone sees teaches nothing.
      { key: "cond", label: "condition", kind: "code", type: "bool", default: "1 + 1 == 2",
        doc: "Any Rhai expression that evaluates to a bool. Objects bound in this session are in " +
          "scope, so <code>srv1.name == \"explorer-device\"</code> works once you have a server." },
      { key: "msg", label: "message", kind: "text", type: "string", default: "the server has a name",
        doc: "What was expected, phrased positively — it is quoted verbatim in “assertion failed: …”." },
    ],
    src: "scripting/bindings.rs",
    build: (_on, a) => `assert(${a.cond}, ${JSON.stringify(a.msg)})` },

  { group: "session", kind: "method", sig: "take_events()", ret: "array of maps",
    desc: "Drain every queued event, oldest first, for every server in the session.",
    prose: "The session-wide drain; <code>server.take_events()</code> is the per-server variant.",
    note: "Expect <code>[]</code> here. This page drains the queue itself after every Execute so it can " +
      "print the events in the log, so by the time you run this the queue is normally empty. It is not " +
      "empty in the one case that matters — see <code>take_events()[0].device</code> under BluetoothDevice.",
    params: [], src: "scripting/bindings.rs",
    build: () => "take_events()" },

  { group: "session", kind: "method",
    sig: "assert_over(server, uuid, op, threshold, seconds, byte)", ret: "int",
    desc: "The temporal assertion: fail unless the comparison held on *every* sample of a window.",
    prose: "<code>assert</code> says <em>this happened</em>; <code>assert_over</code> says <em>this " +
      "stayed true</em>, and those are different claims — a heart rate that reads 72 once has proved " +
      "nothing about the spike at t=1.4 s. The window is sampled every 0.1 s; between samples the " +
      "server's own <code>fn tick</code> is run, so the device's physics move under the assertion. " +
      "On the first violation it throws, naming the value, the time and which sample it was. " +
      "Same semantics as the MCP <code>assert_over</code> tool, deliberately unchanged, so " +
      "“monitor HR &lt; 200 for 3 s” means one thing on both surfaces.",
    note: "Three shorter arities are registered and resolve to the same function: " +
      "<code>(server, uuid, op, threshold)</code> uses the 2.0 s default window and byte 1; " +
      "<code>(…, seconds)</code> defaults the byte. And because Rhai has one integer type and no " +
      "coercion at the call site, <code>seconds</code> is registered for both <code>f64</code> and " +
      "<code>i64</code> — <code>assert_over(srv1, u, \"&lt;\", 200, 3)</code> resolves, which is the " +
      "most natural spelling. This form passes all six, so it always resolves.",
    params: [
      { key: "srv", label: "server", kind: "objref", objType: "server", type: "BluetoothGattServer",
        doc: "The device to sample. Read host-side through <code>server.value(uuid)</code> — there is " +
          "no central and no radio involved." },
      { key: "uuid", label: "uuid", kind: "uuid", type: "Uuid",
        presetExpr: "uuid::HEART_RATE_MEASUREMENT",
        doc: "The characteristic to sample. Errors if the server has no such characteristic." },
      { key: "op", label: "op", kind: "code", type: "string", default: '"<"',
        doc: "One of <code>&lt;</code> <code>&gt;</code> <code>&lt;=</code> <code>&gt;=</code> " +
          "<code>==</code> <code>!=</code>, quoted. Anything else is an error, not a silent pass." },
      { key: "threshold", label: "threshold", kind: "code", type: "int", default: "200",
        doc: "The right-hand side of the comparison." },
      { key: "seconds", label: "seconds", kind: "code", type: "float | int", default: "2.0", opt: true,
        doc: "Window length. Defaults to 2.0. A window is <code>ceil(seconds / 0.1)</code> samples and " +
          "always at least one, so <code>0.0</code> still checks the value it starts with." },
      { key: "byte", label: "byte", kind: "code", type: "int", default: "1", opt: true,
        doc: "Index into the characteristic's value. Defaults to 1 — index 0 is the flags octet in " +
          "nearly every SIG measurement format." },
    ],
    retDesc: "the extreme sample — the one that came closest to violating the rule",
    src: "scripting/monitor.rs",
    build: (_on, a) =>
      `assert_over(${a.srv}, ${a.uuid}, ${a.op}, ${a.threshold}, ${a.seconds}, ${a.byte})` },

  { group: "session", kind: "method", sig: "advance(server, t)", ret: "()",
    desc: "Run one step of the server's own `fn tick(server, t)` at time `t`.",
    prose: "The other half of “the device's physics run under the assertion”. A server loaded with " +
      "<code>catalog::device</code> carries its entry's script along, and this is what forwards a tick " +
      "into it — so a scene that composed itself out of catalog devices keeps them moving once the " +
      "test is over. Also spelled <code>server.advance(t)</code>: Rhai accepts method-call syntax for " +
      "any registered function.",
    note: "A server with no carried script — anything you built by hand on this page — is a no-op " +
      "here, and deliberately <em>not</em> an error: a static device is a legitimate thing to " +
      "monitor, it simply never changes. Load one with <code>catalog::device</code> to see it move.",
    params: [
      { key: "srv", label: "server", kind: "objref", objType: "server", type: "BluetoothGattServer",
        doc: "The device to tick." },
      { key: "t", label: "t", kind: "code", type: "float | int", default: "1.0",
        doc: "The time to pass to <code>fn tick</code>, in seconds. Registered for both " +
          "<code>f64</code> and <code>i64</code>, for the reason <code>assert_over</code> is." },
    ],
    src: "scripting/monitor.rs",
    build: (_on, a) => `advance(${a.srv}, ${a.t})` },

  { group: "session", kind: "method", sig: "f32_le(value)", ret: "blob",
    desc: "Lay an IEEE-754 float out little-endian, the four bytes a characteristic carries.",
    prose: "Several profiles carry floats — ranging distance, weight scales, some sensor readings — " +
      "and a script has no other way to build one. Little-endian, as everything on the wire is.",
    params: [{ key: "v", label: "value", kind: "code", type: "float", default: "1.25",
      doc: "The value, as an <code>f64</code>; it is narrowed to <code>f32</code> before encoding." }],
    src: "transport/wasm_ws.rs (web runtime)",
    build: (_on, a) => `f32_le(${a.v})` },

  // ---- catalog::* ----------------------------------------------------------
  // The one section where a single Execute produces a whole working device.
  { group: "catalog", kind: "method", sig: 'catalog::device(name)', ret: "BluetoothGattServer",
    binds: "server",
    desc: "Load a shipped catalog device by name — and host it.",
    prose: "The entry is compiled and run <strong>in this engine</strong>, not a sub-engine, which is " +
      "what makes it useful: every binding the entry needs (<code>update_value</code>, " +
      "<code>add_pacs</code>, <code>f32_le</code>) is already here, its events land in this session's " +
      "queue, and the value that comes back is an ordinary <code>BluetoothGattServer</code>. So this " +
      "page hosts it the moment the line succeeds — it advertises, accepts connections and notifies, " +
      "exactly like one you built by hand. The entry's <code>fn tick</code> is carried with it, so " +
      "<code>advance</code> and <code>assert_over</code> run the real device.",
    note: "An entry that loads another entry is legitimate composition; one that loads itself is a " +
      "stack overflow that Rhai's call-depth limit cannot see, because the recursion runs through " +
      "native frames. So nesting is capped at 4 and says so.",
    params: [{ key: "name", label: "name", kind: "text", type: "string", default: "hrm",
      doc: "A catalog name — run <code>catalog::names()</code> below for the list. An unknown name is " +
        "an error naming what you asked for, never a silent empty device." }],
    src: "scripting/catalog.rs",
    build: (_on, a) => `catalog::device(${JSON.stringify(a.name)})` },

  { group: "catalog", kind: "method", sig: "catalog::names()", ret: "array of strings",
    desc: "Every name `catalog::device` would accept, in catalog order.",
    prose: "Peripherals then clients — the same order the listings and the docs quote. It exists so an " +
      "error message is never the only way to find out what is available.",
    params: [], src: "scripting/catalog.rs",
    build: () => "catalog::names()" },

  { group: "catalog", kind: "method", sig: "catalog::source(name)", ret: "string",
    desc: "The entry's Rhai source, unrun.",
    prose: "For a test that wants to assert <em>about</em> a catalog entry — that it still sets a CCCD, " +
      "that it still defines a <code>tick</code> — rather than duplicating the file. Nothing is " +
      "evaluated, so this is also the way to read what <code>catalog::device</code> is about to do.",
    params: [{ key: "name", label: "name", kind: "text", type: "string", default: "hrm",
      doc: "A catalog name. Unknown names error the same way <code>device</code>'s do." }],
    src: "scripting/catalog.rs",
    build: (_on, a) => `catalog::source(${JSON.stringify(a.name)})` },

  // ---- BluetoothGattServer ----
  { group: "server", kind: "ctor", sig: 'android::BluetoothGattServer(name)',
    ret: "BluetoothGattServer", binds: "server",
    desc: "Create a GATT server — the device itself.",
    prose: "The constructor is the type name as a module function, not <code>::new</code>: " +
      "<code>new</code> is a reserved word in Rhai, and type-name-as-constructor is Rhai's own idiom " +
      "for the case. Creating one here is enough to put it on the air.",
    params: [{ key: "name", label: "name", kind: "text", type: "string", default: "explorer-device",
      doc: "The device name, as it appears in advertising and in every scanner. Also the key that " +
        "routes events to this server when a session holds several." }],
    src: "scripting/bindings.rs",
    build: (_on, a) => `android::BluetoothGattServer(${JSON.stringify(a.name)})` },

  { group: "server", kind: "prop", sig: "server.name", ret: "string",
    desc: "The device name given to the constructor.",
    receiver: "server", params: [], src: "scripting/bindings.rs",
    build: (on) => `${on}.name` },

  { group: "server", kind: "method", sig: "server.add_service(service)", ret: "bool",
    desc: "Register a service on the server, building its attributes into the GATT database.",
    prose: "This is where a free-standing builder object becomes real attributes: declarations are " +
      "written and handles assigned, to the characteristics and to their descriptors. It adds nothing " +
      "of its own — a notify-capable characteristic gets a CCCD only if you added one. Always returns " +
      "<code>true</code>.",
    receiver: "server",
    params: [{ key: "svc", label: "service", kind: "objref", objType: "service",
      type: "BluetoothGattService",
      doc: "A service bound earlier in this session. Passed by value — add its characteristics first." }],
    retDesc: "always true", src: "scripting/bindings.rs",
    build: (on, a) => `${on}.add_service(${a.svc})` },

  { group: "server", kind: "method", sig: "server.get_service(uuid)", ret: "BluetoothGattService | ()",
    desc: "Look up a registered service by UUID.",
    prose: "Returns the registered copy — the one carrying assigned handles — or <code>()</code> if the " +
      "server has no such service.",
    receiver: "server",
    params: [{ key: "uuid", label: "uuid", kind: "uuid", type: "Uuid", doc: UUID_DOC }],
    src: "scripting/bindings.rs",
    build: (on, a) => `${on}.get_service(${a.uuid})` },

  { group: "server", kind: "method", sig: "server.update_value(uuid, value)", ret: "()",
    desc: "Write a characteristic's value straight into the live database — and notify subscribers.",
    prose: "A web-runtime addition, and the one you want for “make the device show a new reading”. " +
      "<code>notify_characteristic_changed</code> does not persist its value, and the web glue treats " +
      "the database as the single source of truth: the page UI reads it, and a value change there " +
      "becomes a real ATT notification for any subscribed central on the next tick.",
    receiver: "server",
    params: [
      { key: "uuid", label: "uuid", kind: "uuid", type: "Uuid",
        presetExpr: "uuid::HEART_RATE_MEASUREMENT",
        doc: "The characteristic to write. Errors if no characteristic with this UUID is registered." },
      { key: "bytes", label: "value", kind: "bytes", type: "blob | array | ()", default: "0x00, 72",
        doc: BYTES_DOC },
    ],
    src: "transport/wasm_ws.rs (web runtime)",
    build: (on, a) => `${on}.update_value(${a.uuid}, ${bytesExpr(a.bytes)})` },

  { group: "server", kind: "method", sig: "server.value(uuid)", ret: "blob",
    desc: "Read a characteristic's current bytes back out of the live database.",
    prose: "The read half of <code>update_value</code>, and the only state a <code>fn tick</code> can " +
      "carry between calls. A host-side read, so it ignores client permissions — a script can read the " +
      "write-only control point a peer just wrote. Errors if there is no such characteristic.",
    receiver: "server",
    params: [{ key: "uuid", label: "uuid", kind: "uuid", type: "Uuid",
      presetExpr: "uuid::HEART_RATE_MEASUREMENT", doc: UUID_DOC }],
    src: "transport/wasm_ws.rs (web runtime)",
    build: (on, a) => `${on}.value(${a.uuid})` },

  { group: "server", kind: "method",
    sig: "server.notify_characteristic_changed(device, characteristic, confirm)", ret: "int",
    desc: "Notify or indicate a connected central with the characteristic's current value.",
    prose: "Returns a <code>GATT_*</code> status: <code>GATT_FAILURE</code> if the characteristic has no " +
      "assigned handle (it was never added to a registered service) or the device is not connected, " +
      "<code>GATT_SUCCESS</code> otherwise. Mirrors Android API 33's <code>int</code> return, not the " +
      "older <code>boolean</code>.",
    receiver: "server", needsDevice: true,
    params: [
      { key: "device", label: "device", kind: "objref", objType: "device", type: "BluetoothDevice",
        doc: "The peer to notify. Only arrives on an event — see BluetoothDevice." },
      { key: "chr", label: "characteristic", kind: "objref", objType: "characteristic",
        type: "BluetoothGattCharacteristic",
        doc: "The characteristic to send. Its stored value is what goes out, so <code>set_value</code> " +
          "it first — or use the four-argument form below." },
      { key: "confirm", label: "confirm", kind: "bool", type: "bool",
        doc: "<code>false</code> sends a notification (fire and forget); <code>true</code> sends an " +
          "indication, which the central confirms." },
    ],
    retDesc: "GATT_SUCCESS or GATT_FAILURE", src: "scripting/bindings.rs",
    build: (on, a) => `${on}.notify_characteristic_changed(${a.device}, ${a.chr}, ${a.confirm})` },

  { group: "server", kind: "method",
    sig: "server.notify_characteristic_changed(device, characteristic, confirm, value)", ret: "int",
    desc: "Set the value and notify in one call.",
    prose: "The four-argument overload, sugar for <code>set_value</code> followed by the three-argument " +
      "form. Note that it sets the value on the copy it is handed, not in the device's database — " +
      "<code>update_value</code> is what changes what a later read returns.",
    receiver: "server", needsDevice: true,
    params: [
      { key: "device", label: "device", kind: "objref", objType: "device", type: "BluetoothDevice",
        doc: "The peer to notify." },
      { key: "chr", label: "characteristic", kind: "objref", objType: "characteristic",
        type: "BluetoothGattCharacteristic", doc: "The characteristic to send." },
      { key: "confirm", label: "confirm", kind: "bool", type: "bool",
        doc: "<code>true</code> for an indication, <code>false</code> for a notification." },
      { key: "bytes", label: "value", kind: "bytes", type: "blob | array | ()", default: "0x00, 72",
        doc: BYTES_DOC },
    ],
    retDesc: "GATT_SUCCESS or GATT_FAILURE", src: "scripting/bindings.rs",
    build: (on, a) =>
      `${on}.notify_characteristic_changed(${a.device}, ${a.chr}, ${a.confirm}, ${bytesExpr(a.bytes)})` },

  { group: "server", kind: "method",
    sig: "server.send_response(device, request_id, status, offset, value)", ret: "()",
    desc: "Answer a peer's read or write request.",
    prose: "Simble dispatches ATT synchronously — the real response was already produced and sent by " +
      "the time your handler ran — so this cannot rewrite the packet on the wire. On " +
      "<code>GATT_SUCCESS</code> it still applies <code>value</code> to the attribute, so later reads " +
      "see what you answered.",
    receiver: "server", needsDevice: true,
    params: [
      { key: "device", label: "device", kind: "objref", objType: "device", type: "BluetoothDevice",
        doc: "The peer that made the request — <code>event.device</code>." },
      { key: "request_id", label: "request_id", kind: "number", type: "int", default: "0",
        doc: "The id from the event being answered — <code>event.request_id</code>." },
      { key: "status", label: "status", kind: "status", type: "int",
        doc: "A <code>GATT_*</code> constant. Anything other than <code>GATT_SUCCESS</code> reports an " +
          "error and leaves the attribute alone." },
      { key: "offset", label: "offset", kind: "number", type: "int", default: "0",
        doc: "Byte offset the request began at — <code>event.offset</code>. Echoed back; long reads " +
          "are not split here." },
      { key: "bytes", label: "value", kind: "bytes", type: "blob | array | ()", default: "", opt: true,
        doc: "The payload to answer a read with, or <code>()</code> for none — which is what a write " +
          "acknowledgement wants. " + BYTES_DOC },
    ],
    src: "scripting/bindings.rs",
    build: (on, a) =>
      `${on}.send_response(${a.device}, ${a.request_id}, ${a.status}, ${a.offset}, ${bytesExpr(a.bytes)})` },

  { group: "server", kind: "method", sig: "server.take_events()", ret: "array of maps",
    desc: "Drain this server's queued events, leaving other servers' events alone.",
    prose: "One event queue serves every server in a session, so this filters by server name. See " +
      "Event maps for the shape of what comes back — and the note on the session-wide " +
      "<code>take_events()</code> for why this normally returns <code>[]</code> on this page.",
    receiver: "server", params: [], src: "scripting/bindings.rs",
    build: (on) => `${on}.take_events()` },

  { group: "server", kind: "method", sig: "server.emit(kind, payload)", ret: "()",
    desc: "Send a message back to the host — a page, a test — that is not GATT state.",
    prose: "The return path of the event channel: a decoded frame, a log line, a state transition. " +
      "Payloads cross as JSON, so the payload must be serialisable. The host reads them with " +
      "<code>take_emitted</code>; this page does not display them.",
    receiver: "server",
    params: [
      { key: "kind", label: "kind", kind: "text", type: "string", default: "bpm",
        doc: "The message name the host matches on." },
      { key: "payload", label: "payload", kind: "code", type: "any (serialisable)", default: "72",
        doc: "Any Rhai value that converts to JSON — an int, a string, a map, an array." },
    ],
    src: "transport/wasm_ws.rs (web runtime)",
    build: (on, a) => `${on}.emit(${JSON.stringify(a.kind)}, ${a.payload})` },

  { group: "server", kind: "method", sig: "server.close()", ret: "()",
    desc: "Drop the server's callback, so it stops raising events.",
    prose: "Mirrors <code>BluetoothGattServer.close()</code>. The device and its database survive; only " +
      "event delivery stops.",
    receiver: "server", params: [], src: "scripting/bindings.rs",
    build: (on) => `${on}.close()` },

  // ---- Advertising ----
  { group: "advertising", kind: "method", sig: "server.advertise_service_uuid(uuid16)", ret: "()",
    desc: "Add a 16-bit service UUID to the advertisement.",
    prose: "Services added with <code>add_service</code> are advertised for you. This is for the ones " +
      "the profile registrars install straight into the database, which the script's service list never " +
      "sees.",
    receiver: "server",
    params: [{ key: "uuid16", label: "uuid16", kind: "number", type: "int", default: "0x185B",
      doc: "A 16-bit assigned number as an integer — <code>0x180D</code>, not a <code>Uuid</code>. " +
        "Errors if it does not fit in 16 bits." }],
    src: "transport/wasm_ws.rs (web runtime)",
    build: (on, a) => `${on}.advertise_service_uuid(${a.uuid16})` },

  { group: "advertising", kind: "method", sig: "server.advertise_service_data(uuid16, data)", ret: "()",
    desc: "Stage 16-bit Service Data for the advertisement.",
    prose: "The beacon idiom — Fast Pair, Eddystone, a Quick Share nudge — where the payload rides in " +
      "the advertisement and nobody has to connect. Folded into the on-air payload at bring-up.",
    receiver: "server",
    params: [
      { key: "uuid16", label: "uuid16", kind: "number", type: "int", default: "0xFE2C",
        doc: "The 16-bit UUID the data belongs to, as an integer." },
      { key: "data", label: "data", kind: "bytes", type: "blob | array | ()", default: "0x00, 0x01",
        doc: BYTES_DOC },
    ],
    src: "scripting/bindings.rs",
    build: (on, a) => `${on}.advertise_service_data(${a.uuid16}, ${bytesExpr(a.data)})` },

  { group: "advertising", kind: "method",
    sig: "server.advertise_manufacturer_data(company_id, data)", ret: "()",
    desc: "Stage manufacturer-specific data for the advertisement.",
    receiver: "server",
    params: [
      { key: "company_id", label: "company_id", kind: "number", type: "int", default: "0x004C",
        doc: "A 16-bit Bluetooth SIG company identifier (0x004C is Apple, 0x00E0 Google). Errors if it " +
          "does not fit in 16 bits." },
      { key: "data", label: "data", kind: "bytes", type: "blob | array | ()", default: "0x02, 0x15",
        doc: BYTES_DOC },
    ],
    src: "scripting/bindings.rs",
    build: (on, a) => `${on}.advertise_manufacturer_data(${a.company_id}, ${bytesExpr(a.data)})` },

  { group: "advertising", kind: "method", sig: "server.advertise_connectable(connectable)", ret: "()",
    desc: "Make the device a pure beacon, or connectable again.",
    prose: "<code>false</code> advertises non-connectable (ADV_NONCONN_IND), so a scanner shows a " +
      "broadcast-only beacon and never offers to connect — what real iBeacon and Eddystone advertisers " +
      "do.",
    receiver: "server",
    params: [{ key: "connectable", label: "connectable", kind: "bool", type: "bool",
      doc: "<code>true</code> for a normal connectable peripheral, <code>false</code> for a beacon." }],
    src: "scripting/bindings.rs",
    build: (on, a) => `${on}.advertise_connectable(${a.connectable})` },

  // ---- Profile registrars ----
  { group: "profiles", kind: "method", sig: "server.add_ras()", ret: "()",
    desc: "Install the Ranging Service (185B) — the GATT half of Channel Sounding.",
    prose: "A responder publishing distance estimates a peer reads or subscribes to. The ranging " +
      "procedure itself is a controller feature; this is what a phone actually talks to. Pair it with " +
      "<code>advertise_service_uuid(0x185B)</code>.",
    receiver: "server", params: [], src: "transport/wasm_ws.rs (web runtime)",
    build: (on) => `${on}.add_ras()` },

  { group: "profiles", kind: "method",
    sig: "server.add_pacs(sink_location, source_location)", ret: "()",
    desc: "Install Published Audio Capabilities (1850) — what an LE Audio device can play and capture.",
    receiver: "server",
    params: [
      { key: "sink", label: "sink_location", kind: "code", type: "int (bitmask)",
        default: "audio::location::STEREO",
        doc: "Audio Location bitmask for the sink side. Use the <code>audio::location::*</code> " +
          "constants; <code>0</code> means no sink." },
      { key: "source", label: "source_location", kind: "code", type: "int (bitmask)", default: "0",
        doc: "Audio Location bitmask for the source (capture) side; <code>0</code> for a playback-only " +
          "device." },
    ],
    src: "transport/wasm_ws.rs (web runtime)",
    build: (on, a) => `${on}.add_pacs(${a.sink}, ${a.source})` },

  { group: "profiles", kind: "method",
    sig: "server.add_ascs(sink_ase_ids, source_ase_ids)", ret: "()",
    desc: "Install Audio Stream Control (184E), with the endpoints it exposes.",
    prose: "This one is not inert: it installs the ASE control-point handler, so a peer's Config Codec " +
      "and Enable writes actually drive the endpoint state machine.",
    receiver: "server",
    params: [
      { key: "sink", label: "sink_ase_ids", kind: "bytes", type: "blob | array | ()", default: "1",
        doc: "One byte per sink ASE, each its ASE id. <code>1</code> gives one sink endpoint. " + BYTES_DOC },
      { key: "source", label: "source_ase_ids", kind: "bytes", type: "blob | array | ()", default: "",
        doc: "Same, for source endpoints. Empty for a playback-only device." },
    ],
    src: "transport/wasm_ws.rs (web runtime)",
    build: (on, a) => `${on}.add_ascs(${bytesExpr(a.sink)}, ${bytesExpr(a.source)})` },

  { group: "profiles", kind: "method",
    sig: "server.add_vcs(initial_volume, initial_mute)", ret: "()",
    desc: "Install Volume Control (1844) — Android's BluetoothVolumeControl.",
    prose: "Not inert: it installs the Volume Control Point handler, so a peer's write drives a real " +
      "state machine. The operand's change counter is checked and a stale one is rejected with " +
      "ATT error <code>0x80</code> Invalid Change Counter — which is the part a script cannot " +
      "hand-build, because Rhai has no way to fail an ATT write. Only a write that actually moves " +
      "the state advances the counter.",
    receiver: "server",
    params: [
      { key: "vol", label: "initial_volume", kind: "code", type: "int (0-255)", default: "128",
        doc: "The volume the device powers on at." },
      { key: "mute", label: "initial_mute", kind: "code", type: "int", default: "0",
        doc: "<code>0</code> not muted, <code>1</code> muted." },
    ],
    src: "transport/wasm_ws.rs (web runtime)",
    build: (on, a) => `${on}.add_vcs(${a.vol}, ${a.mute})` },

  { group: "profiles", kind: "method",
    sig: "server.add_vocs(audio_location, description)", ret: "()",
    desc: "Install one Volume Offset Control instance (1845) — one audio output's trim.",
    prose: "A device includes one VOCS per output. Its control point has its own change counter, " +
      "independent of the Volume Control Service's.",
    receiver: "server",
    params: [
      { key: "loc", label: "audio_location", kind: "code", type: "int (bitmask)", default: "0x01",
        doc: "Audio Location bitmask for this output — <code>0x01</code> front left, " +
          "<code>0x02</code> front right." },
      { key: "desc", label: "description", kind: "text", type: "string", default: "Left",
        doc: "Human-readable name for the output." },
    ],
    src: "transport/wasm_ws.rs (web runtime)",
    build: (on, a) => `${on}.add_vocs(${a.loc}, "${a.desc}")` },

  { group: "profiles", kind: "method",
    sig: "server.add_aics(gain_minimum, gain_maximum, description)", ret: "()",
    desc: "Install one Audio Input Control instance (1843) — one audio input's gain.",
    prose: "The input is typed <code>Bluetooth</code> and <code>Active</code>, the case an LE Audio " +
      "sink actually has. Its control point validates gain range, mute and gain-mode transitions.",
    receiver: "server",
    params: [
      { key: "min", label: "gain_minimum", kind: "code", type: "int (0-255)", default: "0",
        doc: "Lowest gain setting the input accepts." },
      { key: "max", label: "gain_maximum", kind: "code", type: "int (0-255)", default: "255",
        doc: "Highest gain setting the input accepts." },
      { key: "desc", label: "description", kind: "text", type: "string", default: "Line In",
        doc: "Human-readable name for the input." },
    ],
    src: "transport/wasm_ws.rs (web runtime)",
    build: (on, a) => `${on}.add_aics(${a.min}, ${a.max}, "${a.desc}")` },

  { group: "profiles", kind: "method",
    sig: "server.add_csis(sirk, set_size, rank)", ret: "()",
    desc: "Install Coordinated Set Identification (1846) — Android's BluetoothCsipSetCoordinator.",
    prose: "Makes this device one member of a set (a pair of earbuds, a pair of hearing aids). " +
      "Pair it with <code>advertise_set_identity</code>, which is what actually makes the member " +
      "recognisable — the SIRK crypto is the half a script cannot build, since Rhai has no AES.",
    receiver: "server",
    params: [
      { key: "sirk", label: "sirk", kind: "bytes", type: "blob | array", default: "0x83, 0x8E, 0x68, 0x05, 0x53, 0xF1, 0x41, 0x5A, 0xA2, 0x65, 0xBB, 0xAF, 0xC6, 0xEA, 0x03, 0xB8",
        doc: "The 16-byte Set Identity Resolving Key. Every member of one set carries the same one. " + BYTES_DOC },
      { key: "size", label: "set_size", kind: "code", type: "int", default: "2",
        doc: "How many devices are in the set." },
      { key: "rank", label: "rank", kind: "code", type: "int", default: "1",
        doc: "This member's 1-based rank within the set; a coordinator locks members in rank order." },
    ],
    src: "transport/wasm_ws.rs (web runtime)",
    build: (on, a) => `${on}.add_csis(${bytesExpr(a.sirk)}, ${a.size}, ${a.rank})` },

  { group: "profiles", kind: "method",
    sig: "server.advertise_set_identity(sirk, prand)", ret: "()",
    desc: "Advertise the Resolvable Set Identifier (AD type 2E) for a coordinated set.",
    prose: "Stages <code>sih(sirk, prand) || prand</code> into the advertisement — hash first, " +
      "then the random half it was taken over (CSIS Section 4.9). A coordinator " +
      "recomputes the hash with each SIRK it holds; a match means \"member of that set\". Without " +
      "this a set member is discoverable but not identifiable as a member.",
    receiver: "server",
    params: [
      { key: "sirk", label: "sirk", kind: "bytes", type: "blob | array", default: "0x83, 0x8E, 0x68, 0x05, 0x53, 0xF1, 0x41, 0x5A, 0xA2, 0x65, 0xBB, 0xAF, 0xC6, 0xEA, 0x03, 0xB8",
        doc: "The same 16-byte SIRK passed to <code>add_csis</code>. " + BYTES_DOC },
      { key: "prand", label: "prand", kind: "bytes", type: "blob | array", default: "0x69, 0xF5, 0x63",
        doc: "Three random bytes. A real member draws fresh ones each time it starts advertising, " +
          "so the identifier is not a stable tracker. " + BYTES_DOC },
    ],
    src: "transport/wasm_ws.rs (web runtime)",
    build: (on, a) => `${on}.advertise_set_identity(${bytesExpr(a.sirk)}, ${bytesExpr(a.prand)})` },

  { group: "profiles", kind: "method",
    sig: "server.add_has(features, presets)", ret: "()",
    desc: "Install Hearing Access (1854) — Android's BluetoothHapClient.",
    prose: "Presets are named listening programs. The client pages the list with Read Presets Request " +
      "and switches with Set Active Preset, over a control point that <em>indicates</em> its " +
      "responses — a handshake a <code>tick</code> cannot hold, because it has to keep a read " +
      "cursor across an indication and resume on the confirmation.",
    receiver: "server",
    params: [
      { key: "feat", label: "features", kind: "code", type: "int (bitfield)", default: "0b10_0010",
        doc: "HAS Section 3.1: bits 0-1 the hearing-aid type (00 binaural, 01 monaural, 10 banded), " +
          "bit 2 preset synchronization, bit 3 independent presets, bit 4 dynamic presets, " +
          "bit 5 writable presets." },
      { key: "presets", label: "presets", kind: "code", type: "array of maps",
        default: '[#{ index: 1, name: "Universal" }, #{ index: 2, name: "Restaurant" }]',
        doc: "One map per preset: <code>index</code> and <code>name</code> are required, " +
          "<code>writable</code> and <code>available</code> default to true. Names are 1-40 bytes." },
    ],
    src: "transport/wasm_ws.rs (web runtime)",
    build: (on, a) => `${on}.add_has(${a.feat}, ${a.presets})` },

  { group: "profiles", kind: "method",
    sig: "server.add_gmcs(player_name, ccid)", ret: "()",
    desc: "Install the Generic Media Control Service (1849) — a phone's device-wide player.",
    prose: "Android has <em>no</em> Bluetooth profile proxy for this: an app controls media through " +
      "<code>MediaSession</code>/<code>MediaController</code> and the stack bridges to MCS " +
      "internally, so this binding is named for the Bluetooth service rather than borrowing an " +
      "Android class that does not exist. Use <code>add_mcs</code> for a per-app player instance.",
    receiver: "server",
    params: [
      { key: "name", label: "player_name", kind: "text", type: "string", default: "SimBLE Player",
        doc: "What a headset shows as the player's name." },
      { key: "ccid", label: "ccid", kind: "code", type: "int (0-255)", default: "1",
        doc: "Content Control ID — how a Call/Media Control profile says which player a stream " +
          "belongs to." },
    ],
    src: "transport/wasm_ws.rs (web runtime)",
    build: (on, a) => `${on}.add_gmcs("${a.name}", ${a.ccid})` },

  { group: "profiles", kind: "method",
    sig: "server.add_mcs(player_name, ccid)", ret: "()",
    desc: "Install one Media Control Service instance (1848) — a single app's player.",
    prose: "A real phone registers one MCS per media app, each with its own Content Control ID, " +
      "alongside the one Generic Media Control Service.",
    receiver: "server",
    params: [
      { key: "name", label: "player_name", kind: "text", type: "string", default: "Podcasts",
        doc: "What a headset shows as this player's name." },
      { key: "ccid", label: "ccid", kind: "code", type: "int (0-255)", default: "2",
        doc: "Content Control ID, distinct from every other player's." },
    ],
    src: "transport/wasm_ws.rs (web runtime)",
    build: (on, a) => `${on}.add_mcs("${a.name}", ${a.ccid})` },

  { group: "profiles", kind: "method", sig: "server.add_bass(num_receive_states)", ret: "()",
    desc: "Install the Broadcast Audio Scan Service (184F) — this device becomes an Auracast Scan Delegator.",
    prose: "The earbud side of Auracast. Android has no proxy for <em>being</em> a Delegator — a phone " +
      "is always the Assistant — so this keeps the <code>add_pacs</code>/<code>add_ascs</code> " +
      "registrar spelling rather than inventing a Java class name for it. Once installed, an " +
      "Assistant's Add Source writes land on a real control-point handler and show up in " +
      "<code>receive_states()</code>.",
    receiver: "server",
    params: [{ key: "n", label: "num_receive_states", kind: "code", type: "int", default: "1",
      doc: "How many Broadcast Receive State slots to publish. BASS Section 3.2 requires at least " +
        "one, and zero is rejected rather than quietly making an unusable service." }],
    src: "scripting/broadcast.rs",
    build: (on, a) => `${on}.add_bass(${a.n})` },

  { group: "profiles", kind: "method", sig: "server.receive_states()", ret: "array of maps",
    desc: "The Delegator's view of its own BASS slots.",
    prose: "So a script can see which sources an Assistant added and what each is synchronised to. " +
      "Each map is a Broadcast Receive State: <code>source_id</code>, <code>source_device</code>, " +
      "<code>source_address_type</code>, <code>source_advertising_sid</code>, " +
      "<code>broadcast_id</code>, <code>pa_sync_state</code>, <code>big_encryption</code>, and " +
      "<code>subgroups</code> — each carrying its own <code>bis_sync</code> mask and " +
      "<code>metadata</code> blob. Empty on a server with no BASS.",
    receiver: "server", params: [],
    retDesc: "one map per occupied slot; [] if add_bass was never called",
    src: "scripting/broadcast.rs",
    build: (on) => `${on}.receive_states()` },

  { group: "profiles", kind: "method",
    sig: "server.report_sync_outcome(source_id, pa_sync_state, bis_sync)", ret: "()",
    desc: "Tell the Delegator what a synchronisation attempt actually achieved.",
    prose: "No Android equivalent, deliberately: on a real earbud the <em>controller</em> carries out " +
      "the synchronisation an Add Source requested and reports back what it managed. Nothing in a " +
      "scripted Delegator owns a radio, so the script stands in for one — this is the composition " +
      "step <code>BroadcastAudioScanService</code>'s own docs say it does not take. Without it a " +
      "Delegator reports <code>NotSynchronizedToPa</code> forever, which is the honest default.",
    note: "Expect <code>BASS rejected Source_ID 1: 0x81</code> here on a fresh page, and that is the " +
      "binding working: you cannot report an outcome for a source that was never added, and the " +
      "Delegator says so with the spec's own <em>Invalid Source_ID</em> error rather than inventing a " +
      "slot. Run it against a Source_ID that <code>receive_states()</code> lists — which needs an " +
      "Assistant, and this page hosts no central.",
    receiver: "server",
    params: [
      { key: "id", label: "source_id", kind: "code", type: "int (0-255)", default: "1",
        doc: "The Source_ID BASS assigned when the source was added — read it out of " +
          "<code>receive_states()</code>." },
      { key: "pa", label: "pa_sync_state", kind: "code", type: "string", default: '"SynchronizedToPa"',
        doc: "A PA_Sync_State name, quoted: <code>\"NotSynchronizedToPa\"</code>, " +
          "<code>\"SyncInfoRequest\"</code>, <code>\"SynchronizedToPa\"</code>, " +
          "<code>\"FailedToSynchronizeToPa\"</code> or <code>\"NoPast\"</code>." },
      { key: "bis", label: "bis_sync", kind: "code", type: "int (bitmask)", default: "0x00000001",
        doc: "Which BISes are synchronised, one bit per BIS index. <code>0</code> for none; " +
          "<code>0xFFFFFFFF</code> means “failed to sync to BIG”." },
    ],
    src: "scripting/broadcast.rs",
    build: (on, a) => `${on}.report_sync_outcome(${a.id}, ${a.pa}, ${a.bis})` },

  { group: "profiles", kind: "method", sig: "server.send_audio(sdu)", ret: "bool",
    desc: "Stream one ISO SDU to the connected peer.",
    prose: "The unicast media plane. Returns <code>false</code> — rather than erroring — when there is " +
      "no audio handle or no packet could be built, which is exactly when a real controller would drop " +
      "the SDU. Expect <code>false</code> on this page until an ASCS endpoint is enabled by a peer.",
    receiver: "server",
    params: [{ key: "sdu", label: "sdu", kind: "bytes", type: "blob | array", default: "0x01, 0x02",
      doc: BYTES_DOC }],
    retDesc: "true if the SDU was queued, false if no data path is open",
    src: "transport/wasm_ws.rs (web runtime)",
    build: (on, a) => `${on}.send_audio(${bytesExpr(a.sdu)})` },

  { group: "profiles", kind: "method", sig: "server.take_audio()", ret: "array of blobs",
    desc: "Drain the ISO SDUs this device has received, oldest first.",
    prose: "The sink half of <code>send_audio</code>. Draining is destructive, so a script polls it " +
      "from <code>fn tick</code> and keeps whatever it needs itself.",
    receiver: "server", params: [],
    retDesc: "[] when nothing has arrived", src: "transport/wasm_ws.rs (web runtime)",
    build: (on) => `${on}.take_audio()` },

  // ---- BluetoothGattService ----
  { group: "service", kind: "ctor", sig: "android::BluetoothGattService(uuid, service_type)",
    ret: "BluetoothGattService", binds: "service",
    desc: "Create a service.",
    prose: "Free-standing until <code>server.add_service</code> takes it — add its characteristics first.",
    params: [
      { key: "uuid", label: "uuid", kind: "uuid", type: "Uuid", doc: UUID_DOC },
      { key: "type", label: "service_type", kind: "servicetype", type: "int",
        doc: "<code>SERVICE_TYPE_PRIMARY</code> for a service a peer discovers directly; " +
          "<code>SERVICE_TYPE_SECONDARY</code> for one only meant to be included by another." },
    ],
    src: "scripting/bindings.rs",
    build: (_on, a) => `android::BluetoothGattService(${a.uuid}, ${a.type})` },

  { group: "service", kind: "prop", sig: "service.uuid", ret: "Uuid",
    desc: "The service's UUID.", receiver: "service", params: [], src: "scripting/bindings.rs",
    build: (on) => `${on}.uuid` },
  { group: "service", kind: "prop", sig: "service.service_type", ret: "int",
    desc: "0 for primary, 1 for secondary — the SERVICE_TYPE_* value it was built with.",
    receiver: "service", params: [], src: "scripting/bindings.rs",
    build: (on) => `${on}.service_type` },
  { group: "service", kind: "prop", sig: "service.characteristics", ret: "array",
    desc: "The characteristics added so far, as an array of BluetoothGattCharacteristic.",
    prose: "A copy: mutating an element does not reach the service.",
    receiver: "service", params: [], src: "scripting/bindings.rs",
    build: (on) => `${on}.characteristics` },

  { group: "service", kind: "method", sig: "service.add_characteristic(characteristic)", ret: "bool",
    desc: "Add a characteristic to the service.",
    prose: "Appends by value, so build the characteristic completely — value, descriptors — first. " +
      "Always returns <code>true</code>.",
    receiver: "service",
    params: [{ key: "chr", label: "characteristic", kind: "objref", objType: "characteristic",
      type: "BluetoothGattCharacteristic", doc: "A characteristic bound earlier in this session." }],
    retDesc: "always true", src: "scripting/bindings.rs",
    build: (on, a) => `${on}.add_characteristic(${a.chr})` },

  { group: "service", kind: "method", sig: "service.get_characteristic(uuid)",
    ret: "BluetoothGattCharacteristic | ()",
    desc: "Find a characteristic on this service by UUID.",
    prose: "Returns <code>()</code> if the service has none with that UUID.",
    receiver: "service",
    params: [{ key: "uuid", label: "uuid", kind: "uuid", type: "Uuid",
      presetExpr: "uuid::HEART_RATE_MEASUREMENT", doc: UUID_DOC }],
    src: "scripting/bindings.rs",
    build: (on, a) => `${on}.get_characteristic(${a.uuid})` },

  // ---- BluetoothGattCharacteristic ----
  { group: "characteristic", kind: "ctor",
    sig: "android::BluetoothGattCharacteristic(uuid, properties, permissions)",
    ret: "BluetoothGattCharacteristic", binds: "characteristic",
    desc: "Create a characteristic.",
    prose: "Properties say what a peer may do; permissions say what security that requires. They are " +
      "independent — READ without PERMISSION_READ is a characteristic a central is told it can read and " +
      "then may not.",
    params: [
      { key: "uuid", label: "uuid", kind: "uuid", type: "Uuid",
        presetExpr: "uuid::HEART_RATE_MEASUREMENT", doc: UUID_DOC },
      { key: "props", label: "properties", kind: "flags", options: PROPERTY_OPTIONS,
        defaults: [1, 4], type: "int (bitmask)",
        doc: "OR of <code>PROPERTY_*</code>. Tick nothing for <code>0</code>. NOTIFY also needs a CCCD " +
          "descriptor before a central can subscribe." },
      { key: "perms", label: "permissions", kind: "flags", options: PERMISSION_OPTIONS,
        defaults: [0], type: "int (bitmask)",
        doc: "OR of <code>PERMISSION_*</code>. Tick nothing for <code>0</code> — no access permitted." },
    ],
    src: "scripting/bindings.rs",
    build: (_on, a) => `android::BluetoothGattCharacteristic(${a.uuid}, ${a.props}, ${a.perms})` },

  { group: "characteristic", kind: "prop", sig: "characteristic.uuid", ret: "Uuid",
    desc: "The characteristic's UUID.", receiver: "characteristic", params: [],
    src: "scripting/bindings.rs", build: (on) => `${on}.uuid` },
  { group: "characteristic", kind: "prop", sig: "characteristic.properties", ret: "int",
    desc: "The PROPERTY_* bitmask it was built with.",
    receiver: "characteristic", params: [], src: "scripting/bindings.rs",
    build: (on) => `${on}.properties` },
  { group: "characteristic", kind: "prop", sig: "characteristic.permissions", ret: "int",
    desc: "The PERMISSION_* bitmask it was built with.",
    receiver: "characteristic", params: [], src: "scripting/bindings.rs",
    build: (on) => `${on}.permissions` },
  { group: "characteristic", kind: "prop", sig: "characteristic.value", ret: "blob",
    desc: "The stored value on this builder object.",
    prose: "The builder's own bytes, not the live database — once the service is registered, " +
      "<code>server.value(uuid)</code> is what reflects writes from a peer.",
    receiver: "characteristic", params: [], src: "scripting/bindings.rs",
    build: (on) => `${on}.value` },

  { group: "characteristic", kind: "method", sig: "characteristic.set_value(value)", ret: "bool",
    desc: "Set the stored value. Always returns true.",
    prose: "Sets the initial value a registered characteristic is published with. After registration, " +
      "use <code>server.update_value</code> — that one reaches the database and notifies subscribers.",
    receiver: "characteristic",
    params: [{ key: "bytes", label: "value", kind: "bytes", type: "blob | array | ()",
      default: "0x00, 72", doc: BYTES_DOC }],
    retDesc: "always true", src: "scripting/bindings.rs",
    build: (on, a) => `${on}.set_value(${bytesExpr(a.bytes)})` },

  { group: "characteristic", kind: "method", sig: "characteristic.get_value()", ret: "blob",
    desc: "The stored value — the method spelling of the `value` property.",
    receiver: "characteristic", params: [], src: "scripting/bindings.rs",
    build: (on) => `${on}.get_value()` },

  { group: "characteristic", kind: "method", sig: "characteristic.add_descriptor(descriptor)",
    ret: "bool",
    desc: "Attach a descriptor — in practice, a CCCD.",
    prose: "Always returns <code>true</code>, and appends by value, so set the descriptor up first. " +
      "This is not optional plumbing: <code>add_service</code> writes exactly the attributes you gave " +
      "it, so a NOTIFY characteristic with no CCCD publishes nothing for a central to write and can " +
      "never be subscribed. (The Rust profile registrars — <code>add_ras</code>, <code>add_pacs</code>, " +
      "<code>add_ascs</code> — add theirs for you; a hand-built characteristic does not.)",
    receiver: "characteristic",
    params: [{ key: "dsc", label: "descriptor", kind: "objref", objType: "descriptor",
      type: "BluetoothGattDescriptor", doc: "A descriptor bound earlier in this session." }],
    retDesc: "always true", src: "scripting/bindings.rs",
    build: (on, a) => `${on}.add_descriptor(${a.dsc})` },

  // ---- BluetoothGattDescriptor ----
  { group: "descriptor", kind: "ctor", sig: "android::BluetoothGattDescriptor(uuid, permissions)",
    ret: "BluetoothGattDescriptor", binds: "descriptor",
    desc: "Create a descriptor. A CCCD is what makes a characteristic subscribable.",
    params: [
      { key: "uuid", label: "uuid", kind: "uuid", type: "Uuid",
        presetExpr: "uuid::CLIENT_CHARACTERISTIC_CONFIGURATION",
        doc: "Usually <code>CLIENT_CHARACTERISTIC_CONFIGURATION</code> (2902), the two bytes a central " +
          "writes to turn notifications on. " + UUID_DOC },
      { key: "perms", label: "permissions", kind: "flags", options: PERMISSION_OPTIONS,
        defaults: [0, 3], type: "int (bitmask)",
        doc: "OR of <code>PERMISSION_*</code>. A CCCD needs READ and WRITE, or a central cannot " +
          "subscribe." },
    ],
    src: "scripting/bindings.rs",
    build: (_on, a) => `android::BluetoothGattDescriptor(${a.uuid}, ${a.perms})` },

  { group: "descriptor", kind: "prop", sig: "descriptor.uuid", ret: "Uuid",
    desc: "The descriptor's UUID.", receiver: "descriptor", params: [],
    src: "scripting/bindings.rs", build: (on) => `${on}.uuid` },
  { group: "descriptor", kind: "prop", sig: "descriptor.value", ret: "blob",
    desc: "The descriptor's stored bytes.", receiver: "descriptor", params: [],
    src: "scripting/bindings.rs", build: (on) => `${on}.value` },
  { group: "descriptor", kind: "method", sig: "descriptor.set_value(value)", ret: "bool",
    desc: "Set the descriptor's bytes. Always returns true.",
    receiver: "descriptor",
    params: [{ key: "bytes", label: "value", kind: "bytes", type: "blob | array | ()",
      default: "0x00, 0x00", doc: BYTES_DOC }],
    retDesc: "always true", src: "scripting/bindings.rs",
    build: (on, a) => `${on}.set_value(${bytesExpr(a.bytes)})` },
  { group: "descriptor", kind: "method", sig: "descriptor.get_value()", ret: "blob",
    desc: "The descriptor's bytes — the method spelling of the `value` property.",
    receiver: "descriptor", params: [], src: "scripting/bindings.rs",
    build: (on) => `${on}.get_value()` },

  // ---- BluetoothDevice ----
  // There is no constructor: a device only ever arrives on an event. This entry
  // is the one line that gets one out of the queue, which is what makes
  // notify_characteristic_changed and send_response reachable at all here.
  { group: "device", kind: "method", sig: "take_events()[0].device", ret: "BluetoothDevice",
    binds: "device",
    desc: "Bind the peer from the oldest queued event — the only way to get a BluetoothDevice.",
    prose: "Scripts never construct a device; it arrives as <code>event.device</code> on an event that " +
      "has a peer (connected, disconnected, a read or write request, notification_sent, mtu_changed). " +
      "This line reaches into the queue and binds it, which is what makes the two device-taking server " +
      "methods usable from this page.",
    note: "Timing matters: this page drains the event queue after every Execute so it can print events " +
      "in the log. Run this as the <em>first</em> Execute after a central connects, or the event is " +
      "already gone and you get an index-out-of-bounds error. Connect a central from " +
      "<a href=\"../scanner/\">the scanner</a> in another tab.",
    params: [], src: "scripting/bindings.rs · transport/wasm_ws.rs",
    build: () => "take_events()[0].device" },

  { group: "device", kind: "prop", sig: "device.address", ret: "string",
    desc: "The peer's Bluetooth address, as `AA:BB:CC:00:00:01`.",
    receiver: "device", params: [], src: "scripting/bindings.rs",
    build: (on) => `${on}.address` },
  { group: "device", kind: "method", sig: "device.to_string()", ret: "string",
    desc: "The device rendered for a log line.",
    receiver: "device", params: [], src: "scripting/bindings.rs",
    build: (on) => `${on}.to_string()` },

  // ---- Uuid ----
  { group: "uuid", kind: "method", sig: "uuid::of(text)", ret: "Uuid",
    desc: "Parse a UUID from its string form.",
    prose: "The escape hatch for anything with no named constant: a 16-bit assigned number as hex " +
      "(<code>\"2A6E\"</code>) or a full 128-bit UUID (<code>\"f0ff0002-1234-5678-90ab-cdef01234567\"</code>). " +
      "Raises a script error on anything it cannot parse.",
    params: [{ key: "text", label: "text", kind: "text", type: "string", default: "2A6E",
      doc: "The UUID as a string. Hyphens required for the 128-bit form, absent for the 16-bit one." }],
    src: "scripting/constants.rs",
    build: (_on, a) => `uuid::of(${JSON.stringify(a.text)})` },

  { group: "uuid", kind: "method", sig: "uuid::from_u16(n)", ret: "Uuid",
    desc: "Lift a bare 16-bit assigned number into a Uuid.",
    prose: "The same thing <code>uuid::of</code> does for hex text, from an integer you already have. " +
      "Errors if the number does not fit in 16 bits.",
    params: [{ key: "n", label: "n", kind: "number", type: "int", default: "0x2A6E",
      doc: "A 16-bit assigned number, decimal or hex." }],
    src: "scripting/constants.rs",
    build: (_on, a) => `uuid::from_u16(${a.n})` },

  { group: "uuid", kind: "method", sig: "uuid.to_string()", ret: "string",
    desc: "Render a UUID — short form for 16-bit, full form for 128-bit.",
    params: [{ key: "uuid", label: "uuid", kind: "uuid", type: "Uuid", doc: UUID_DOC }],
    src: "scripting/bindings.rs",
    build: (_on, a) => `${a.uuid}.to_string()` },

  { group: "uuid", kind: "method", sig: "a == b", ret: "bool",
    desc: "Compare two UUIDs. `!=` is registered too.",
    prose: "A 16-bit UUID and its 128-bit expansion are distinct values here — comparison is on the " +
      "representation, not the Bluetooth Base UUID mapping.",
    params: [
      { key: "a", label: "a", kind: "uuid", type: "Uuid", doc: "Left-hand UUID." },
      { key: "b", label: "b", kind: "uuid", type: "Uuid", presetExpr: 'uuid::of("2A6E")',
        doc: "Right-hand UUID." },
    ],
    src: "scripting/bindings.rs",
    build: (_on, a) => `${a.a} == ${a.b}` },

  { group: "uuid", kind: "method", sig: "a != b", ret: "bool",
    desc: "The negation, registered separately.",
    prose: "Rhai does not derive <code>!=</code> from <code>==</code> for a custom type, so it is its " +
      "own registration. Listed as its own member for that reason — it is a distinct entry in the " +
      "function registry, not sugar.",
    params: [
      { key: "a", label: "a", kind: "uuid", type: "Uuid", doc: "Left-hand UUID." },
      { key: "b", label: "b", kind: "uuid", type: "Uuid", presetExpr: 'uuid::of("2A6E")',
        doc: "Right-hand UUID." },
    ],
    src: "scripting/bindings.rs",
    build: (_on, a) => `${a.a} != ${a.b}` },

  { group: "uuid", kind: "method", sig: "uuid.to_debug()", ret: "string",
    desc: "The Rust `{:?}` form, for when `to_string` hides what you need to see.",
    prose: "<code>to_string</code> gives the canonical spelling; <code>to_debug</code> gives the " +
      "<code>Debug</code> rendering, which shows whether a value is held as a 16-bit assigned number " +
      "or as a full 128-bit UUID. That distinction is exactly what <code>==</code> compares on, so " +
      "this is the member to reach for when a comparison surprises you.",
    params: [{ key: "u", label: "uuid", kind: "uuid", type: "Uuid",
      presetExpr: "uuid::HEART_RATE_MEASUREMENT", doc: UUID_DOC }],
    src: "scripting/bindings.rs",
    build: (_on, a) => `${a.u}.to_debug()` },

  // ---- android::BluetoothGatt (central) ------------------------------------
  // Every member here is mode 'ref'. See the file header: this session pumps
  // servers, so a client's queued packets would never leave the page.
  { group: "client", kind: "ctor", mode: "ref", sig: "android::BluetoothGatt(name)",
    ret: "BluetoothGatt", binds: "client", bindsName: "client",
    desc: "Create a GATT client — the central.",
    prose: "A central has no address of its own to allocate: the controller it runs on supplies one. " +
      "The script must keep it in a top-level variable — that is how the runner finds it.",
    params: [{ key: "name", label: "name", kind: "text", type: "string", default: "Probe",
      doc: "What a page or a scene labels this client with." }],
    src: "scripting/client.rs",
    build: (_on, a) => `android::BluetoothGatt(${JSON.stringify(a.name)})` },

  { group: "client", kind: "ctor", mode: "doc", sig: "android::BluetoothLeScanner()",
    ret: "BluetoothLeScanner",
    desc: "A scanner, so a script can find a peer by what it advertises instead of by an address.",
    prose: "The primitive that lets a script meet a peer it was never told about. A phone advertises " +
      "from a resolvable private address that rotates and that Android will not disclose even to its " +
      "own app &mdash; there is no address to write into a script, only a service to look for. Keep it " +
      "in a top-level variable beside the <code>BluetoothGatt</code>; the host queues the scan bring-up " +
      "once <code>start_scan</code> has been called, and reports arrive at " +
      "<code>on_scan_result</code>.",
    params: [],
    src: "scripting/client.rs" },

  { group: "client", kind: "ctor", mode: "doc", sig: "android::ScanFilter(uuid)", ret: "ScanFilter",
    desc: "Report only advertisements carrying this service UUID.",
    prose: "Android's <code>ScanFilter.Builder().setServiceUuid(&hellip;).build()</code>, collapsed to a " +
      "constructor because a script would call the builder once and build immediately. " +
      "<code>start_scan()</code> with no filter reports everything, as on Android.",
    params: [{ key: "uuid", label: "uuid", type: "Uuid",
      doc: "The service an advertisement must carry, e.g. <code>uuid::of(\"f0bb0001-1234-5678-90ab-cdef01234567\")</code>." }],
    src: "scripting/client.rs" },

  { group: "client", kind: "method", mode: "doc", sig: "scanner.start_scan(filter)", ret: "()",
    desc: "Start scanning. The filter is optional, as on Android.",
    prose: "Active scanning, so a peer's scan-response name is collected too &mdash; a passive scan never " +
      "solicits it. The bring-up is queued once, on the next pass, and only for a script that actually " +
      "asked to scan.",
    receiver: "scanner", receiverName: "scanner",
    params: [{ key: "filter", label: "filter", type: "ScanFilter",
      doc: "What to report. Omit the argument to report every advertisement." }],
    src: "scripting/client.rs" },

  { group: "client", kind: "method", mode: "doc", sig: "scanner.stop_scan()", ret: "()",
    desc: "Stop reporting advertisements.",
    prose: "Call it from <code>on_scan_result</code> once the peer is found, or the handler keeps firing " +
      "for every repeat advertisement.",
    receiver: "scanner", receiverName: "scanner", params: [],
    src: "scripting/client.rs" },

  { group: "client", kind: "prop", mode: "doc", sig: "scanner.scanning", ret: "bool",
    desc: "Whether the scan is still running.",
    receiver: "scanner", receiverName: "scanner", params: [],
    src: "scripting/client.rs" },

  { group: "client", kind: "method", mode: "ref", sig: "client.connect(address)", ret: "()",
    desc: "Bring the controller up and connect to a peer by address.",
    prose: "Queues the whole bring-up — reset, scan, connect — as HCI packets for the host to send. " +
      "Raises a script error if the string is not an address. A host that allocates addresses itself " +
      "(MCP, a page that spawns devices) overrides the target afterwards: topology beats script.",
    receiver: "client", receiverName: "client",
    params: [{ key: "address", label: "address", kind: "text", type: "string",
      default: "AA:BB:CC:00:00:01",
      doc: "The peer's address, <code>AA:BB:CC:00:00:01</code>. Every catalog example uses this one." }],
    src: "scripting/client.rs",
    build: (on, a) => `${on}.connect(${JSON.stringify(a.address)})` },

  { group: "client", kind: "method", mode: "ref", sig: "client.disconnect()", ret: "()",
    desc: "Tear the link down.",
    receiver: "client", receiverName: "client", params: [], src: "scripting/client.rs",
    build: (on) => `${on}.disconnect()` },

  { group: "client", kind: "method", mode: "ref", sig: "client.read(uuid)", ret: "()",
    desc: "Queue a read of a characteristic by UUID.",
    prose: "Queued, not immediate — the answer arrives at <code>fn on_characteristic_read(client, uuid, " +
      "value)</code>, and the bytes are also kept for <code>client.value(uuid)</code>. UUIDs only " +
      "resolve to handles after discovery, so read from <code>on_services_discovered</code> rather than " +
      "at top level.",
    receiver: "client", receiverName: "client",
    params: [{ key: "uuid", label: "uuid", kind: "uuid", type: "Uuid", doc: UUID_DOC }],
    src: "scripting/client.rs",
    build: (on, a) => `${on}.read(${a.uuid})` },

  { group: "client", kind: "method", mode: "ref", sig: "client.write(uuid, value)", ret: "()",
    desc: "Queue an acknowledged write (ATT Write Request).",
    prose: "The peer answers with a Write Response, which arrives at " +
      "<code>fn on_characteristic_write(client, uuid, status)</code> carrying the ATT status.",
    receiver: "client", receiverName: "client",
    params: [
      { key: "uuid", label: "uuid", kind: "uuid", type: "Uuid", doc: UUID_DOC },
      { key: "value", label: "value", kind: "bytes", type: "blob | array | ()", default: "0x01",
        doc: BYTES_DOC },
    ],
    src: "scripting/client.rs",
    build: (on, a) => `${on}.write(${a.uuid}, ${bytesExpr(a.value)})` },

  { group: "client", kind: "method", mode: "ref", sig: "client.write_command(uuid, value)", ret: "()",
    desc: "Queue an unacknowledged write (ATT Write Command).",
    prose: "Vol 3, Part F, 3.4.5.3: the peer never answers one, so " +
      "<code>on_characteristic_write</code> fires as soon as it goes out. Needs " +
      "<code>PROPERTY_WRITE_NO_RESPONSE</code> on the peer's characteristic.",
    receiver: "client", receiverName: "client",
    params: [
      { key: "uuid", label: "uuid", kind: "uuid", type: "Uuid", doc: UUID_DOC },
      { key: "value", label: "value", kind: "bytes", type: "blob | array | ()", default: "0x01",
        doc: BYTES_DOC },
    ],
    src: "scripting/client.rs",
    build: (on, a) => `${on}.write_command(${a.uuid}, ${bytesExpr(a.value)})` },

  { group: "client", kind: "method", mode: "ref", sig: "client.subscribe(uuid)", ret: "()",
    desc: "Turn notifications on by writing the characteristic's CCCD.",
    prose: "Confirmed by <code>fn on_subscribed(client, uuid)</code>; the values then arrive at " +
      "<code>fn on_characteristic_changed(client, uuid, value)</code>. Fails if the peer's " +
      "characteristic has no CCCD — which is why a server-side characteristic needs one.",
    receiver: "client", receiverName: "client",
    params: [{ key: "uuid", label: "uuid", kind: "uuid",
      presetExpr: "uuid::HEART_RATE_MEASUREMENT", type: "Uuid", doc: UUID_DOC }],
    src: "scripting/client.rs",
    build: (on, a) => `${on}.subscribe(${a.uuid})` },

  { group: "client", kind: "method", mode: "ref", sig: "client.unsubscribe(uuid)", ret: "()",
    desc: "Turn notifications off by clearing the CCCD.",
    receiver: "client", receiverName: "client",
    params: [{ key: "uuid", label: "uuid", kind: "uuid",
      presetExpr: "uuid::HEART_RATE_MEASUREMENT", type: "Uuid", doc: UUID_DOC }],
    src: "scripting/client.rs",
    build: (on, a) => `${on}.unsubscribe(${a.uuid})` },

  { group: "client", kind: "method", mode: "ref", sig: "client.value(uuid)", ret: "blob",
    desc: "The last bytes seen for a characteristic, read or notified.",
    prose: "Empty when nothing has arrived on it — a script asserting on a value it never received sees " +
      "an empty blob, not a stale one.",
    receiver: "client", receiverName: "client",
    params: [{ key: "uuid", label: "uuid", kind: "uuid", type: "Uuid", doc: UUID_DOC }],
    src: "scripting/client.rs",
    build: (on, a) => `${on}.value(${a.uuid})` },

  { group: "client", kind: "method", mode: "ref", sig: "client.is_subscribed(uuid)", ret: "bool",
    desc: "Whether notifications are currently on for a characteristic.",
    receiver: "client", receiverName: "client",
    params: [{ key: "uuid", label: "uuid", kind: "uuid", type: "Uuid", doc: UUID_DOC }],
    src: "scripting/client.rs",
    build: (on, a) => `${on}.is_subscribed(${a.uuid})` },

  { group: "client", kind: "method", mode: "ref", sig: "client.has_characteristic(uuid)", ret: "bool",
    desc: "Whether discovery found a characteristic with this UUID.",
    prose: "The polite way to check a peer is what you think it is before subscribing — " +
      "<code>assert(client.has_characteristic(…), \"the peer is a heart-rate monitor\")</code>. Always " +
      "false before discovery finishes.",
    receiver: "client", receiverName: "client",
    params: [{ key: "uuid", label: "uuid", kind: "uuid",
      presetExpr: "uuid::HEART_RATE_MEASUREMENT", type: "Uuid", doc: UUID_DOC }],
    src: "scripting/client.rs",
    build: (on, a) => `${on}.has_characteristic(${a.uuid})` },

  { group: "client", kind: "method", mode: "ref", sig: "client.services()", ret: "array of Uuid",
    desc: "The service UUIDs discovery found.",
    prose: "Empty until discovery finishes. Pair it with <code>characteristics(service)</code> to walk " +
      "an unknown peer.",
    receiver: "client", receiverName: "client", params: [], src: "scripting/client.rs",
    build: (on) => `${on}.services()` },

  { group: "client", kind: "method", mode: "ref", sig: "client.characteristics(service)",
    ret: "array of Uuid",
    desc: "The characteristic UUIDs discovered under one service.",
    receiver: "client", receiverName: "client",
    params: [{ key: "service", label: "service", kind: "uuid", type: "Uuid",
      doc: "A service UUID, normally one that came back from <code>services()</code>." }],
    src: "scripting/client.rs",
    build: (on, a) => `${on}.characteristics(${a.service})` },

  { group: "client", kind: "method", mode: "ref", sig: "client.emit(kind, payload)", ret: "()",
    desc: "Send a message back to the host — the client's mirror of `server.emit`.",
    prose: "How a client tells a page or a test something that is not GATT state: a decoded reading, a " +
      "milestone. Payloads cross as JSON, so they must be serialisable.",
    receiver: "client", receiverName: "client",
    params: [
      { key: "kind", label: "kind", kind: "text", type: "string", default: "bpm",
        doc: "The message name the host matches on." },
      { key: "payload", label: "payload", kind: "code", type: "any (serialisable)", default: "value[1]",
        doc: "Any Rhai value that converts to JSON." },
    ],
    src: "scripting/client.rs",
    build: (on, a) => `${on}.emit(${JSON.stringify(a.kind)}, ${a.payload})` },

  { group: "client", kind: "prop", mode: "ref", sig: "client.name", ret: "string",
    desc: "The name given to the constructor.",
    receiver: "client", receiverName: "client", params: [], src: "scripting/client.rs",
    build: (on) => `${on}.name` },
  { group: "client", kind: "prop", mode: "ref", sig: "client.peer", ret: "string",
    desc: "The address the client is targeting, as a string.",
    receiver: "client", receiverName: "client", params: [], src: "scripting/client.rs",
    build: (on) => `${on}.peer` },
  { group: "client", kind: "prop", mode: "ref", sig: "client.state", ret: "string",
    desc: "Where the client is in bring-up, as a label.",
    prose: "One of <code>idle</code>, <code>initializing</code>, <code>scanning for the peer</code>, " +
      "<code>connecting</code>, <code>exchanging MTU</code>, <code>discovering services</code>, " +
      "<code>discovering characteristics</code>, <code>ready</code>, <code>disconnected</code>. " +
      "A label for humans — branch on <code>connected</code> or <code>discovered</code> instead.",
    receiver: "client", receiverName: "client", params: [], src: "scripting/client.rs · device/central.rs",
    build: (on) => `${on}.state` },
  { group: "client", kind: "prop", mode: "ref", sig: "client.connected", ret: "bool",
    desc: "Whether the link is up (a non-zero connection handle).",
    receiver: "client", receiverName: "client", params: [], src: "scripting/client.rs",
    build: (on) => `${on}.connected` },
  { group: "client", kind: "prop", mode: "ref", sig: "client.discovered", ret: "bool",
    desc: "True once discovery has finished — the moment naming a characteristic by UUID starts working.",
    receiver: "client", receiverName: "client", params: [], src: "scripting/client.rs",
    build: (on) => `${on}.discovered` },
  { group: "client", kind: "prop", mode: "ref", sig: "client.idle", ret: "bool",
    desc: "True when every queued operation has been sent and answered.",
    prose: "What a test waits on before deciding a run is over.",
    receiver: "client", receiverName: "client", params: [], src: "scripting/client.rs",
    build: (on) => `${on}.idle` },
  { group: "client", kind: "prop", mode: "ref", sig: "client.mtu", ret: "int",
    desc: "The negotiated ATT MTU.",
    prose: "23 until the exchange completes; the payload limit is three bytes less than this.",
    receiver: "client", receiverName: "client", params: [], src: "scripting/client.rs",
    build: (on) => `${on}.mtu` },

  // ---- android::BluetoothHidHost -------------------------------------------
  // Reference-only for the same structural reason the central is: a HID host
  // IS a central, so the packets it queues would never leave this page.
  { group: "hid", kind: "ctor", mode: "ref", sig: "android::BluetoothHidHost(name)",
    ret: "BluetoothHidHost", binds: "host", bindsName: "host",
    desc: "Create a HID host — a central that decodes HOGP input.",
    prose: "Named for the Android proxy it mirrors. This binds the <strong>HOGP (LE) half</strong>; " +
      "the Classic half is out of reach because a scene cannot host a BR/EDR device at all yet.",
    params: [{ key: "name", label: "name", kind: "text", type: "string", default: "Computer",
      doc: "What a page or a scene labels this host with." }],
    src: "scripting/hid.rs",
    build: (_on, a) => `android::BluetoothHidHost(${JSON.stringify(a.name)})` },

  { group: "hid", kind: "prop", mode: "ref", sig: "host.name", ret: "string",
    desc: "The name given to the constructor.",
    receiver: "host", receiverName: "host", params: [], src: "scripting/hid.rs",
    build: (on) => `${on}.name` },

  { group: "hid", kind: "prop", mode: "ref", sig: "host.peer", ret: "string",
    desc: "The address this host is connected to, or aimed at.",
    receiver: "host", receiverName: "host", params: [], src: "scripting/hid.rs",
    build: (on) => `${on}.peer` },

  { group: "hid", kind: "prop", mode: "ref", sig: "host.connected", ret: "bool",
    desc: "Whether the link is up.",
    prose: "True once a connection handle exists — the same test <code>client.connected</code> makes.",
    receiver: "host", receiverName: "host", params: [], src: "scripting/hid.rs",
    build: (on) => `${on}.connected` },

  { group: "hid", kind: "prop", mode: "ref", sig: "host.discovered", ret: "bool",
    desc: "Whether GATT discovery has finished.",
    prose: "The GATT half only. <code>ready</code> is the HID-level question — discovery finishing is " +
      "when this host <em>starts</em> reading Report Maps.",
    receiver: "host", receiverName: "host", params: [], src: "scripting/hid.rs",
    build: (on) => `${on}.discovered` },

  { group: "hid", kind: "prop", mode: "ref", sig: "host.gatt", ret: "BluetoothGatt",
    desc: "The GATT client underneath, so a script can do plain GATT on the same connection.",
    prose: "A HOGP device usually has a Battery Service too. Rather than opening a second link, reach " +
      "through this and use the whole central binding — <code>read</code>, <code>subscribe</code>, " +
      "everything — on the connection the HID host already holds.",
    receiver: "host", receiverName: "host", params: [], src: "scripting/hid.rs",
    build: (on) => `${on}.gatt` },

  { group: "hid", kind: "prop", mode: "ref", sig: "host.kind", ret: "string",
    desc: "What the Report Map said the peer is.",
    prose: "<code>\"keyboard\"</code>, <code>\"mouse\"</code>, <code>\"game pad\"</code>, " +
      "<code>\"unsupported\"</code> — or <code>\"unknown\"</code> before the Report Map has been read. " +
      "The same string <code>on_identified</code> is handed.",
    receiver: "host", receiverName: "host", params: [], src: "scripting/hid.rs",
    build: (on) => `${on}.kind` },

  { group: "hid", kind: "prop", mode: "ref", sig: "host.ready", ret: "bool",
    desc: "True once the Report Map has been read and the Report characteristics subscribed.",
    prose: "The HID-level readiness, as distinct from <code>discovered</code>. This is the point after " +
      "which input reports actually arrive.",
    receiver: "host", receiverName: "host", params: [], src: "scripting/hid.rs",
    build: (on) => `${on}.ready` },

  { group: "hid", kind: "prop", mode: "ref", sig: "host.report_map", ret: "blob",
    desc: "The Report Map bytes, exactly as read.",
    prose: "Empty until read. A USB HID report descriptor — the thing that says what the reports mean. " +
      "It is here to be asserted on and dumped, not re-parsed: the decoding is done in Rust.",
    receiver: "host", receiverName: "host", params: [], src: "scripting/hid.rs",
    build: (on) => `${on}.report_map` },

  { group: "hid", kind: "prop", mode: "ref", sig: "host.report", ret: "blob",
    desc: "The most recent input report, exactly as it arrived.",
    prose: "The wire beside its meaning: the decoded callbacks tell you a key went down, this tells " +
      "you which bytes said so. Empty before the first one.",
    receiver: "host", receiverName: "host", params: [], src: "scripting/hid.rs",
    build: (on) => `${on}.report` },

  { group: "hid", kind: "prop", mode: "ref", sig: "host.report_handle", ret: "int",
    desc: "The value handle that report arrived on.",
    prose: "A device with several Report characteristics — a combo keyboard/mouse — sends on more than " +
      "one handle, and this is how a script tells them apart. <code>0</code> before the first report.",
    receiver: "host", receiverName: "host", params: [], src: "scripting/hid.rs",
    build: (on) => `${on}.report_handle` },

  { group: "hid", kind: "prop", mode: "ref", sig: "host.report_len_hint", ret: "int",
    desc: "The largest input report this host expects.",
    prose: "For a script sizing a view of the raw bytes. A hint, as the name says — derived from the " +
      "Report Map, not enforced on what arrives.",
    receiver: "host", receiverName: "host", params: [], src: "scripting/hid.rs",
    build: (on) => `${on}.report_len_hint` },

  { group: "hid", kind: "method", mode: "ref", sig: "host.connect(address)", ret: "()",
    desc: "Connect to a HID peer by address. Discovery, Report Map reads and subscription follow automatically.",
    prose: "The one place this deliberately does more than the GATT binding. Once services are " +
      "discovered the host reads every Report Map and subscribes to every notifying Report on its own, " +
      "because that is what a HID host <em>is</em> — a script doing it by hand would be " +
      "re-implementing <code>HidHost::plan</code>. It matches Android, where an app never issues those " +
      "reads either.",
    receiver: "host", receiverName: "host",
    params: [{ key: "address", label: "address", kind: "text", type: "string",
      default: "AA:BB:CC:00:00:01", doc: "The peer's address. A string that is not an address errors." }],
    src: "scripting/hid.rs",
    build: (on, a) => `${on}.connect(${JSON.stringify(a.address)})` },

  { group: "hid", kind: "method", mode: "ref", sig: "host.disconnect()", ret: "()",
    desc: "Drop the link.",
    receiver: "host", receiverName: "host", params: [], src: "scripting/hid.rs",
    build: (on) => `${on}.disconnect()` },

  { group: "hid", kind: "method", mode: "ref", sig: "host.services()", ret: "array of Uuid",
    desc: "The peer's discovered service UUIDs.",
    prose: "The same list <code>client.services()</code> returns — this host is a central underneath.",
    receiver: "host", receiverName: "host", params: [], src: "scripting/hid.rs",
    build: (on) => `${on}.services()` },

  { group: "hid", kind: "method", mode: "ref", sig: "host.emit(kind, payload)", ret: "()",
    desc: "Send a host-side event to the page or the test.",
    prose: "The same <code>emit</code> every other script object offers. The payload crosses as JSON, " +
      "so it must be serialisable.",
    receiver: "host", receiverName: "host",
    params: [
      { key: "kind", label: "kind", kind: "text", type: "string", default: "typed",
        doc: "The event name the host sees." },
      { key: "payload", label: "payload", kind: "code", type: "any (JSON-serialisable)",
        default: '#{ key: "a" }', doc: "Any Rhai value that serialises to JSON." },
    ],
    src: "scripting/hid.rs",
    build: (on, a) => `${on}.emit(${JSON.stringify(a.kind)}, ${a.payload})` },

  // ---- android::BluetoothLeBroadcast (Auracast source) ---------------------
  // Reference-only for a different reason than the centrals: this session
  // re-collects only BluetoothGattServers from its scope, so a source built
  // here would queue its setup packets into an outbox nothing flushes.
  { group: "broadcast", kind: "ctor", mode: "ref", sig: "android::BluetoothLeBroadcast(name)",
    ret: "BluetoothLeBroadcast", binds: "broadcast", bindsName: "broadcast",
    desc: "Create an Auracast broadcast source.",
    prose: "Nothing goes on the air yet — the name is the default <code>broadcast_name</code>, and " +
      "<code>start_broadcast</code> is what builds the BIG.",
    params: [{ key: "name", label: "name", kind: "text", type: "string", default: "SimBLE Auracast",
      doc: "The default Broadcast Name, overridable per-broadcast in the settings map." }],
    src: "scripting/broadcast.rs",
    build: (_on, a) => `android::BluetoothLeBroadcast(${JSON.stringify(a.name)})` },

  { group: "broadcast", kind: "prop", mode: "ref", sig: "broadcast.name", ret: "string",
    desc: "The name given to the constructor.",
    receiver: "broadcast", receiverName: "broadcast", params: [], src: "scripting/broadcast.rs",
    build: (on) => `${on}.name` },

  { group: "broadcast", kind: "prop", mode: "ref", sig: "broadcast.state", ret: "string",
    desc: "Where the source is on the advertising → PA → BIG ladder.",
    prose: "One of <code>\"idle\"</code> (nothing started), <code>\"starting\"</code> (anywhere in " +
      "bring-up), <code>\"streaming\"</code>, <code>\"stopped\"</code> or <code>\"failed\"</code>. " +
      "Deliberately coarse: the fine-grained <code>BroadcastState</code> is a Rust type and no HCI " +
      "detail crosses into Rhai.",
    receiver: "broadcast", receiverName: "broadcast", params: [], src: "scripting/broadcast.rs",
    build: (on) => `${on}.state` },

  { group: "broadcast", kind: "method", mode: "ref", sig: "broadcast.start_broadcast(settings)",
    ret: "()",
    desc: "Build the BIG and start advertising it.",
    prose: "Android's <code>startBroadcast(BluetoothLeBroadcastSettings)</code>. The Java type is a " +
      "builder; a map literal is the honest translation. Errors if this source is already " +
      "broadcasting — stop it first.",
    note: "Exactly one subgroup: <code>BroadcastConfig::base()</code> publishes one, so a second " +
      "would describe a BASE that never reaches the air, and is rejected rather than dropped. " +
      "Channel allocations are assigned by BIS index, so the <code>bis</code> list is " +
      "<em>validated</em> against what the BASE will carry rather than honoured — naming " +
      "<code>[\"FRONT_RIGHT\", \"FRONT_LEFT\"]</code> is an error, not a swap.",
    receiver: "broadcast", receiverName: "broadcast",
    params: [{ key: "settings", label: "settings", kind: "code", type: "map",
      default: '#{ broadcast_id: 0xC0FFEE, broadcast_code: (), subgroups: [ #{ codec: "lc3_48_2", bis: ["FRONT_LEFT", "FRONT_RIGHT"] } ] }',
      doc: "Keys, all optional: <code>broadcast_name</code> (defaults to the source's name), " +
        "<code>broadcast_id</code> (24-bit; a wider value errors rather than truncating into a " +
        "different broadcast), <code>advertising_sid</code>, <code>broadcast_code</code> " +
        "(<code>()</code> for unencrypted, otherwise up to 16 octets, zero-extended as the spec " +
        "requires), and <code>subgroups</code> — one map with <code>codec</code>, <code>bis</code> " +
        "and <code>metadata</code>." }],
    src: "scripting/broadcast.rs",
    build: (on, a) => `${on}.start_broadcast(${a.settings})` },

  { group: "broadcast", kind: "method", mode: "ref", sig: "broadcast.stop_broadcast(broadcast_id)",
    ret: "()",
    desc: "Terminate the BIG.",
    prose: "The Broadcast_ID is checked against this source's own, so stopping the wrong broadcast is " +
      "an error rather than a silent no-op.",
    receiver: "broadcast", receiverName: "broadcast",
    params: [{ key: "id", label: "broadcast_id", kind: "code", type: "int (24-bit)",
      default: "0xC0FFEE", doc: "Must equal the Broadcast_ID this source is running." }],
    src: "scripting/broadcast.rs",
    build: (on, a) => `${on}.stop_broadcast(${a.id})` },

  { group: "broadcast", kind: "method", mode: "ref",
    sig: "broadcast.update_broadcast(broadcast_id, settings)", ret: "()",
    desc: "Rewrite the subgroup metadata of a running broadcast.",
    prose: "Android's <code>updateBroadcast(int, BluetoothLeBroadcastSettings)</code>. <strong>Only " +
      "the metadata can change</strong> while the BIG runs — everything else in the BASE has to agree " +
      "with the LE Create BIG the controller already acted on. Errors if the broadcast is not " +
      "streaming yet, because there is no periodic train to rewrite.",
    receiver: "broadcast", receiverName: "broadcast",
    params: [
      { key: "id", label: "broadcast_id", kind: "code", type: "int (24-bit)", default: "0xC0FFEE",
        doc: "Must equal the Broadcast_ID this source is running." },
      { key: "settings", label: "settings", kind: "code", type: "map",
        default: '#{ subgroups: [ #{ codec: "lc3_48_2", metadata: [0x03, 0x02, 0x04, 0x00] } ] }',
        doc: "The same shape <code>start_broadcast</code> takes; only the subgroup " +
          "<code>metadata</code> reaches the air." },
    ],
    src: "scripting/broadcast.rs",
    build: (on, a) => `${on}.update_broadcast(${a.id}, ${a.settings})` },

  { group: "broadcast", kind: "method", mode: "ref", sig: "broadcast.is_playing(broadcast_id)",
    ret: "bool",
    desc: "Whether that Broadcast_ID is streaming right now.",
    prose: "False for a broadcast that is still in bring-up, and false for any id that is not this " +
      "source's — it answers about one broadcast, not about the source.",
    receiver: "broadcast", receiverName: "broadcast",
    params: [{ key: "id", label: "broadcast_id", kind: "code", type: "int (24-bit)",
      default: "0xC0FFEE", doc: "The Broadcast_ID to ask about." }],
    src: "scripting/broadcast.rs",
    build: (on, a) => `${on}.is_playing(${a.id})` },

  { group: "broadcast", kind: "method", mode: "ref", sig: "broadcast.get_all_broadcast_metadata()",
    ret: "array of maps",
    desc: "The `BluetoothLeBroadcastMetadata` a receiver — or an Assistant — needs to find and join this broadcast.",
    prose: "One entry, because one source runs one broadcast. The map carries " +
      "<code>source_device</code>, <code>source_address_type</code>, " +
      "<code>source_advertising_sid</code>, <code>broadcast_id</code>, <code>broadcast_name</code>, " +
      "<code>pa_sync_interval</code> and <code>encrypted</code> — exactly the fields " +
      "<code>assistant.add_source</code> reads back out. That round trip is the point: it is how a " +
      "script joins the two halves without either of them naming an HCI type.",
    receiver: "broadcast", receiverName: "broadcast", params: [],
    retDesc: "[] before start_broadcast", src: "scripting/broadcast.rs",
    build: (on) => `${on}.get_all_broadcast_metadata()` },

  { group: "broadcast", kind: "method", mode: "ref", sig: "broadcast.send_audio(bis_index, sdu)",
    ret: "bool",
    desc: "Push one SDU into a BIS.",
    prose: "The media plane. Android has no equivalent — an app hands audio to the LE Audio framework, " +
      "never to the proxy — so this is named for what it does rather than after a Java method that " +
      "would be an invention. Returns <code>false</code> while the data paths are not open, which is " +
      "when a real controller would drop the SDU.",
    receiver: "broadcast", receiverName: "broadcast",
    params: [
      { key: "bis", label: "bis_index", kind: "code", type: "int (1-based)", default: "1",
        doc: "Which BIS of the BIG to send on. Indices start at 1, as they do in the BASE." },
      { key: "sdu", label: "sdu", kind: "bytes", type: "blob | array", default: "0x01, 0x02",
        doc: BYTES_DOC },
    ],
    retDesc: "true if the SDU was queued, false if the data paths are not open",
    src: "scripting/broadcast.rs",
    build: (on, a) => `${on}.send_audio(${a.bis}, ${bytesExpr(a.sdu)})` },

  // ---- android::BluetoothLeBroadcastAssistant -------------------------------
  { group: "assistant", kind: "ctor", mode: "ref",
    sig: "android::BluetoothLeBroadcastAssistant(name)", ret: "BluetoothLeBroadcastAssistant",
    binds: "assistant", bindsName: "assistant",
    desc: "Create a Broadcast Assistant — the phone that tells an earbud which broadcast to join.",
    prose: "Pure GATT underneath: it writes BASS control-point operations to a Scan Delegator and reads " +
      "the Broadcast Receive State back. Nothing below GATT is involved.",
    params: [{ key: "name", label: "name", kind: "text", type: "string", default: "Phone",
      doc: "What a page or a scene labels this Assistant with." }],
    src: "scripting/broadcast.rs",
    build: (_on, a) => `android::BluetoothLeBroadcastAssistant(${JSON.stringify(a.name)})` },

  { group: "assistant", kind: "prop", mode: "ref", sig: "assistant.name", ret: "string",
    desc: "The name given to the constructor.",
    receiver: "assistant", receiverName: "assistant", params: [], src: "scripting/broadcast.rs",
    build: (on) => `${on}.name` },

  { group: "assistant", kind: "prop", mode: "ref", sig: "assistant.sink", ret: "string",
    desc: "The Delegator this Assistant is talking to, as an address.",
    prose: "Empty until the first operation that names a sink. Android's framework knows every " +
      "connected sink; simble's central holds one link at a time, so this Assistant learns its one " +
      "sink from <code>add_source</code>, <code>modify_source</code> or <code>remove_source</code> — " +
      "and a second sink needs a second Assistant.",
    receiver: "assistant", receiverName: "assistant", params: [], src: "scripting/broadcast.rs",
    build: (on) => `${on}.sink` },

  { group: "assistant", kind: "prop", mode: "ref", sig: "assistant.connected", ret: "bool",
    desc: "Whether the link to the sink is up.",
    receiver: "assistant", receiverName: "assistant", params: [], src: "scripting/broadcast.rs",
    build: (on) => `${on}.connected` },

  { group: "assistant", kind: "method", mode: "ref",
    sig: "assistant.add_source(sink, metadata, is_group_op)", ret: "()",
    desc: "Write BASS Add Source: tell the earbud to join a broadcast.",
    prose: "There is no <code>connect</code> to call first — Android's <code>addSource</code> reaches a " +
      "sink whether or not a link is up, because the framework owns the connection, and this does the " +
      "same rather than inventing a method Android does not have. The Source_ID is BASS's to assign, " +
      "so <code>on_source_added</code> fires when the Receive State carrying it arrives, not when the " +
      "write is answered.",
    note: "<code>is_group_op</code> is accepted and <strong>not honoured</strong>: applying an " +
      "operation to a sink's whole coordinated set needs a CSIP set coordinator, and nothing wires one " +
      "to this. The operation reaches the one connected sink either way.",
    receiver: "assistant", receiverName: "assistant",
    params: [
      { key: "sink", label: "sink", kind: "text", type: "string", default: "AA:BB:CC:00:00:01",
        doc: "The Delegator's address. The first call that names one opens the link." },
      { key: "metadata", label: "metadata", kind: "code", type: "map",
        default: "broadcast.get_all_broadcast_metadata()[0]",
        doc: "A <code>BluetoothLeBroadcastMetadata</code>-shaped map — the one " +
          "<code>get_all_broadcast_metadata</code> returns. <code>source_device</code> is required; " +
          "<code>source_address_type</code>, <code>source_advertising_sid</code>, " +
          "<code>broadcast_id</code>, <code>pa_sync</code>, <code>pa_sync_interval</code> and " +
          "<code>subgroups</code> fill in the rest." },
      { key: "group", label: "is_group_op", kind: "bool", type: "bool",
        doc: "Android's whole-set flag. Accepted, not honoured — see the note." },
    ],
    src: "scripting/broadcast.rs",
    build: (on, a) => `${on}.add_source(${JSON.stringify(a.sink)}, ${a.metadata}, ${a.group})` },

  { group: "assistant", kind: "method", mode: "ref",
    sig: "assistant.modify_source(sink, source_id, metadata)", ret: "()",
    desc: "Write BASS Modify Source: change the PA sync or subgroup selection of a source already added.",
    prose: "The Source_ID is the one BASS assigned, which a script reads off " +
      "<code>on_receive_state_changed</code> or <code>get_all_sources</code>.",
    receiver: "assistant", receiverName: "assistant",
    params: [
      { key: "sink", label: "sink", kind: "text", type: "string", default: "AA:BB:CC:00:00:01",
        doc: "The Delegator's address." },
      { key: "id", label: "source_id", kind: "code", type: "int (0-255)", default: "1",
        doc: "The Source_ID BASS assigned." },
      { key: "metadata", label: "metadata", kind: "code", type: "map",
        default: '#{ pa_sync: "SynchronizeToPaPastNotAvailable", pa_sync_interval: 0xFFFF }',
        doc: "<code>pa_sync</code> is one of <code>\"DoNotSynchronizeToPa\"</code>, " +
          "<code>\"SynchronizeToPaPastAvailable\"</code> or " +
          "<code>\"SynchronizeToPaPastNotAvailable\"</code> (the default); " +
          "<code>pa_sync_interval</code> and <code>subgroups</code> follow " +
          "<code>add_source</code>'s shapes." },
    ],
    src: "scripting/broadcast.rs",
    build: (on, a) => `${on}.modify_source(${JSON.stringify(a.sink)}, ${a.id}, ${a.metadata})` },

  { group: "assistant", kind: "method", mode: "ref",
    sig: "assistant.remove_source(sink, source_id)", ret: "()",
    desc: "Write BASS Remove Source: drop a source from the earbud's slot.",
    prose: "The Delegator answers by emptying the slot, which arrives as a zero-length Receive State — " +
      "so the confirmation is <code>on_source_removed</code>, not a state map.",
    receiver: "assistant", receiverName: "assistant",
    params: [
      { key: "sink", label: "sink", kind: "text", type: "string", default: "AA:BB:CC:00:00:01",
        doc: "The Delegator's address." },
      { key: "id", label: "source_id", kind: "code", type: "int (0-255)", default: "1",
        doc: "The Source_ID to remove." },
    ],
    src: "scripting/broadcast.rs",
    build: (on, a) => `${on}.remove_source(${JSON.stringify(a.sink)}, ${a.id})` },

  { group: "assistant", kind: "method", mode: "ref",
    sig: "assistant.start_searching_for_sources()", ret: "()",
    desc: "Write BASS Remote Scan Started — tell the sink a scan is underway on its behalf.",
    prose: "Half of Android's <code>startSearchingForSources(List&lt;ScanFilter&gt;)</code>: the BASS " +
      "half, which is the half that is reachable. Errors if this Assistant has not learned its sink " +
      "yet — an operation that names no sink cannot open the link.",
    note: "<strong>The scanning half is not implemented</strong>, so no <code>on_source_found</code> " +
      "is ever delivered. <code>LeCentral</code> has no scan surface of its own, and scan-report " +
      "parsing decodes only <em>legacy</em> advertising while an Auracast source advertises with an " +
      "extended set. A script gets its metadata from " +
      "<code>broadcast.get_all_broadcast_metadata()</code> instead.",
    receiver: "assistant", receiverName: "assistant", params: [], src: "scripting/broadcast.rs",
    build: (on) => `${on}.start_searching_for_sources()` },

  { group: "assistant", kind: "method", mode: "ref",
    sig: "assistant.stop_searching_for_sources()", ret: "()",
    desc: "Write BASS Remote Scan Stopped.",
    prose: "The other half of the pair, and subject to the same “knows its sink already” rule.",
    receiver: "assistant", receiverName: "assistant", params: [], src: "scripting/broadcast.rs",
    build: (on) => `${on}.stop_searching_for_sources()` },

  { group: "assistant", kind: "method", mode: "ref", sig: "assistant.get_all_sources(sink)",
    ret: "array of maps",
    desc: "Every Broadcast Receive State this Assistant has seen from that sink.",
    prose: "The Assistant's own cache, updated from each Receive State notification — the same map " +
      "shape <code>server.receive_states()</code> returns. The <code>sink</code> argument is accepted " +
      "for Android's signature and ignored: this Assistant holds one link.",
    receiver: "assistant", receiverName: "assistant",
    params: [{ key: "sink", label: "sink", kind: "text", type: "string", default: "AA:BB:CC:00:00:01",
      doc: "Android's sink argument. Ignored — one Assistant, one sink." }],
    src: "scripting/broadcast.rs",
    build: (on, a) => `${on}.get_all_sources(${JSON.stringify(a.sink)})` },

  { group: "assistant", kind: "method", mode: "ref", sig: "assistant.emit(kind, payload)", ret: "()",
    desc: "Send a host-side event to the page or the test.",
    prose: "The same <code>emit</code> the central and the server already have; the payload crosses as JSON.",
    receiver: "assistant", receiverName: "assistant",
    params: [
      { key: "kind", label: "kind", kind: "text", type: "string", default: "joined",
        doc: "The event name the host sees." },
      { key: "payload", label: "payload", kind: "code", type: "any (JSON-serialisable)",
        default: "#{ source_id: 1 }", doc: "Any Rhai value that serialises to JSON." },
    ],
    src: "scripting/broadcast.rs",
    build: (on, a) => `${on}.emit(${JSON.stringify(a.kind)}, ${a.payload})` },

  // ---- Callbacks -----------------------------------------------------------
  // Documentation only, and honestly so: these are functions the script
  // defines, not calls the page can build. Arity is not cosmetic — the runtime
  // detects a handler by name AND parameter count, so a signature written with
  // one argument too few is simply never called.
  { group: "callbacks", kind: "callback", mode: "doc", sig: "fn tick(server, t)", ret: "ignored",
    desc: "Peripheral. Called once per host tick — where a device's behaviour over time lives.",
    prose: "Only called if defined with exactly two parameters. A device has no variables that survive " +
      "between calls, so carry state in <code>this</code> or in the GATT database via " +
      "<code>server.value</code> / <code>server.update_value</code>. Errors are recorded, not fatal.",
    params: [
      { key: "server", label: "server", type: "BluetoothGattServer",
        doc: "The server the script created." },
      { key: "t", label: "t", type: "float",
        doc: "Seconds since the script started running." },
    ],
    src: "transport/wasm_ws.rs" },

  { group: "callbacks", kind: "callback", mode: "doc", sig: "fn on_event(server, event)",
    ret: "ignored",
    desc: "Peripheral. Called for every queued event — connections, reads, writes, MTU changes.",
    prose: "The alternative to draining <code>take_events()</code> yourself. Events are dispatched " +
      "before <code>tick</code>, so a write that arrived since the last tick is handled before the " +
      "periodic tick sees the world.",
    params: [
      { key: "server", label: "server", type: "BluetoothGattServer", doc: "The server that raised it." },
      { key: "event", label: "event", type: "map",
        doc: "The event map — see Event maps for its fields and the kinds a peripheral raises." },
    ],
    src: "transport/wasm_ws.rs · scripting/bindings.rs" },

  { group: "callbacks", kind: "callback", mode: "doc", sig: "fn on_scan_result(client, result)",
    ret: "ignored",
    desc: "Central. An advertisement matched the scan filter.",
    prose: "Fires for every matching advertisement, including repeats of the same device, so guard the " +
      "first one &mdash; connect twice and the second attempt fights the first. The usual body is " +
      "<code>client.connect(result.address)</code>, which is how a script aims itself at a peer whose " +
      "address it could not have known.",
    params: [
      { key: "client", label: "client", type: "BluetoothGatt", doc: "The client, ready to be aimed." },
      { key: "result", label: "result", type: "map",
        doc: "<code>address</code>, <code>address_type</code>, <code>rssi</code>, " +
          "<code>connectable</code>, <code>name</code> (<code>()</code> when the advertisement carried " +
          "none) and <code>service_uuids</code>." },
    ],
    src: "scripting/client.rs" },

  { group: "callbacks", kind: "callback", mode: "doc",
    sig: "fn on_connection_state_change(client, connected)", ret: "ignored",
    desc: "Central. The link came up or went away.",
    params: [
      { key: "client", label: "client", type: "BluetoothGatt", doc: "The client." },
      { key: "connected", label: "connected", type: "bool",
        doc: "<code>true</code> on connect, <code>false</code> on disconnect. The HCI status byte is " +
          "not passed here — read it from <code>event.status</code> in <code>on_event</code>." },
    ],
    src: "scripting/client.rs" },

  { group: "callbacks", kind: "callback", mode: "doc", sig: "fn on_services_discovered(client)",
    ret: "ignored",
    desc: "Central. Discovery finished — the earliest point at which UUIDs resolve to handles.",
    prose: "One parameter, not two. This is where a client script does its real work: check the peer is " +
      "what you expect, then read, write or subscribe. Operations queued here go out in the same pass, " +
      "not a tick later.",
    params: [{ key: "client", label: "client", type: "BluetoothGatt",
      doc: "The client. <code>services()</code> and <code>characteristics(uuid)</code> are populated by now." }],
    src: "scripting/client.rs" },

  { group: "callbacks", kind: "callback", mode: "doc",
    sig: "fn on_characteristic_read(client, uuid, value)", ret: "ignored",
    desc: "Central. A queued read came back.",
    prose: "A non-zero ATT status also raises <code>on_error</code> with " +
      "<code>read {uuid}: ATT error 0xNN</code>; the status itself is not a parameter here.",
    params: [
      { key: "client", label: "client", type: "BluetoothGatt", doc: "The client." },
      { key: "uuid", label: "uuid", type: "Uuid", doc: "The characteristic that was read." },
      { key: "value", label: "value", type: "blob",
        doc: "The bytes returned. Index it like an array: <code>value[1]</code>." },
    ],
    src: "scripting/client.rs" },

  { group: "callbacks", kind: "callback", mode: "doc",
    sig: "fn on_characteristic_write(client, uuid, status)", ret: "ignored",
    desc: "Central. A write was answered — or, for a write command, has gone out.",
    params: [
      { key: "client", label: "client", type: "BluetoothGatt", doc: "The client." },
      { key: "uuid", label: "uuid", type: "Uuid", doc: "The characteristic written." },
      { key: "status", label: "status", type: "int",
        doc: "The ATT status: <code>0</code> means the peer accepted it." },
    ],
    src: "scripting/client.rs" },

  { group: "callbacks", kind: "callback", mode: "doc",
    sig: "fn on_characteristic_changed(client, uuid, value)", ret: "ignored",
    desc: "Central. A notification or indication arrived on a subscribed characteristic.",
    params: [
      { key: "client", label: "client", type: "BluetoothGatt", doc: "The client." },
      { key: "uuid", label: "uuid", type: "Uuid", doc: "The characteristic that changed." },
      { key: "value", label: "value", type: "blob", doc: "The bytes the peer sent." },
    ],
    src: "scripting/client.rs" },

  { group: "callbacks", kind: "callback", mode: "doc", sig: "fn on_subscribed(client, uuid)",
    ret: "ignored",
    desc: "Central. A subscribe took effect. Not called for unsubscribe.",
    prose: "Raised only when the CCCD write enabled notifications; a failed subscribe goes to " +
      "<code>on_error</code> instead.",
    params: [
      { key: "client", label: "client", type: "BluetoothGatt", doc: "The client." },
      { key: "uuid", label: "uuid", type: "Uuid", doc: "The characteristic now being notified." },
    ],
    src: "scripting/client.rs" },

  { group: "callbacks", kind: "callback", mode: "doc", sig: "fn on_mtu_changed(client, mtu)",
    ret: "ignored",
    desc: "Central. The ATT MTU exchange completed.",
    params: [
      { key: "client", label: "client", type: "BluetoothGatt", doc: "The client." },
      { key: "mtu", label: "mtu", type: "int", doc: "The negotiated MTU in bytes." },
    ],
    src: "scripting/client.rs" },

  { group: "callbacks", kind: "callback", mode: "doc", sig: "fn on_error(client, message)",
    ret: "ignored",
    desc: "Central. An operation failed, or came back with a non-zero ATT status.",
    prose: "Raised for a failed read, a failed subscribe, and for an operation that could not even " +
      "start. That last case is treated as a script bug: it is recorded as the run's failure too, so a " +
      "test fails rather than waiting for a callback that will never come.",
    params: [
      { key: "client", label: "client", type: "BluetoothGatt", doc: "The client." },
      { key: "message", label: "message", type: "string",
        doc: "Already formatted, e.g. <code>read 2A37: ATT error 0x02</code> or " +
          "<code>subscribe 2A37: no CCCD</code>." },
    ],
    src: "scripting/client.rs" },

  { group: "callbacks", kind: "callback", mode: "doc", sig: "fn on_event(client, event)",
    ret: "ignored",
    desc: "Central. Every event, as one stream — for a script that would rather match than define six handlers.",
    prose: "Called <em>in addition to</em> the specific handlers, before them, for every event. Same " +
      "two-parameter shape as the peripheral's <code>on_event</code>, so the runtime tells them apart " +
      "by which object the script built, not by the signature.",
    params: [
      { key: "client", label: "client", type: "BluetoothGatt", doc: "The client." },
      { key: "event", label: "event", type: "map",
        doc: "The event map — see Event maps for the central's kinds and fields." },
    ],
    src: "scripting/client.rs" },

  // HID host. `on_report` mirrors Android's ACTION_REPORT and stops at the
  // bytes; everything below it is decoded, has no counterpart on Android's
  // proxy, and is named for what it is rather than after a Java callback that
  // does not exist. They are the reason the binding is worth having.
  { group: "callbacks", kind: "callback", mode: "doc", sig: "fn on_report(host, report)",
    ret: "ignored",
    desc: "HID. A raw input report arrived — the wire, before its meaning.",
    prose: "Android's own surface: <code>ACTION_REPORT</code> carries the report bytes and nothing " +
      "more. Raised <em>before</em> the decoded callbacks for the same notification, so a script sees " +
      "the bytes ahead of what they turned into.",
    params: [
      { key: "host", label: "host", type: "BluetoothHidHost", doc: "The host." },
      { key: "report", label: "report", type: "blob",
        doc: "The report exactly as it arrived. <code>host.report_handle</code> says which " +
          "characteristic it came from." },
    ],
    src: "scripting/hid.rs" },

  { group: "callbacks", kind: "callback", mode: "doc", sig: "fn on_input(host, event)",
    ret: "ignored",
    desc: "HID. Every decoded event, as one stream — for a script that would rather match than define seven handlers.",
    prose: "Raised for each decoded event <em>in addition to</em> its specific handler, and before it. " +
      "<code>event.type</code> carries the same snake_case tag <code>HidEvent</code>'s serde " +
      "representation uses, so a script and the JSON a page reads agree on the vocabulary: " +
      "<code>identified</code>, <code>key_down</code>, <code>key_up</code>, <code>roll_over</code>, " +
      "<code>pointer</code>, <code>button_down</code>, <code>button_up</code>. The remaining fields " +
      "are whichever the specific callback would have been passed.",
    params: [
      { key: "host", label: "host", type: "BluetoothHidHost", doc: "The host." },
      { key: "event", label: "event", type: "map", doc: "The decoded event; see the tags above." },
    ],
    src: "scripting/hid.rs" },

  { group: "callbacks", kind: "callback", mode: "doc",
    sig: "fn on_identified(host, kind, report_map)", ret: "ignored",
    desc: "HID. The Report Map has been read and the peer classified.",
    prose: "The first thing a HID host learns, and the point after which reports mean something. " +
      "Decoding happens in Rust — the script is handed the verdict.",
    params: [
      { key: "host", label: "host", type: "BluetoothHidHost", doc: "The host." },
      { key: "kind", label: "kind", type: "string",
        doc: "<code>\"keyboard\"</code>, <code>\"mouse\"</code>, <code>\"game pad\"</code> or " +
          "<code>\"unsupported\"</code>. Same string as <code>host.kind</code>." },
      { key: "report_map", label: "report_map", type: "blob",
        doc: "The Report Map bytes that produced that verdict." },
    ],
    src: "scripting/hid.rs" },

  { group: "callbacks", kind: "callback", mode: "doc", sig: "fn on_key_down(host, key)",
    ret: "ignored",
    desc: "HID. A key went down — a press edge, differenced out of consecutive reports.",
    prose: "HID reports carry the set of keys currently held, not events; the edges are computed in " +
      "Rust by differencing consecutive reports.",
    params: [
      { key: "host", label: "host", type: "BluetoothHidHost", doc: "The host." },
      { key: "key", label: "key", type: "map",
        doc: "<code>#{usage, modifiers, character, label}</code>. <code>character</code> and " +
          "<code>label</code> are <code>()</code> when the key produces neither — test with " +
          "<code>key.character != ()</code> rather than against a sentinel." },
    ],
    src: "scripting/hid.rs" },

  { group: "callbacks", kind: "callback", mode: "doc", sig: "fn on_key_up(host, usage)",
    ret: "ignored",
    desc: "HID. A key was released.",
    prose: "The release edge. Only the usage is carried — the character a key produced was already " +
      "reported on the way down.",
    params: [
      { key: "host", label: "host", type: "BluetoothHidHost", doc: "The host." },
      { key: "usage", label: "usage", type: "int", doc: "The HID usage code that went up." },
    ],
    src: "scripting/hid.rs" },

  { group: "callbacks", kind: "callback", mode: "doc", sig: "fn on_rollover(host)", ret: "ignored",
    desc: "HID. The keyboard reported more keys than it can distinguish.",
    prose: "Phantom-key rollover: the report says <code>ErrorRollOver</code> in every slot, so the " +
      "held-key set is unknowable for that report. Arity 1 — there is nothing to carry.",
    params: [{ key: "host", label: "host", type: "BluetoothHidHost", doc: "The host." }],
    src: "scripting/hid.rs" },

  { group: "callbacks", kind: "callback", mode: "doc", sig: "fn on_pointer(host, dx, dy, wheel)",
    ret: "ignored",
    desc: "HID. Relative pointer movement.",
    prose: "Four parameters, not a map — pointer motion is three numbers and arrives at report rate. " +
      "A handler defined at any other arity is not recognised.",
    params: [
      { key: "host", label: "host", type: "BluetoothHidHost", doc: "The host." },
      { key: "dx", label: "dx", type: "int", doc: "Horizontal movement since the last report, signed." },
      { key: "dy", label: "dy", type: "int", doc: "Vertical movement, signed." },
      { key: "wheel", label: "wheel", type: "int", doc: "Wheel detents, signed. <code>0</code> if the device has no wheel." },
    ],
    src: "scripting/hid.rs" },

  { group: "callbacks", kind: "callback", mode: "doc", sig: "fn on_button_down(host, button)",
    ret: "ignored",
    desc: "HID. A pointer button went down.",
    params: [
      { key: "host", label: "host", type: "BluetoothHidHost", doc: "The host." },
      { key: "button", label: "button", type: "int", doc: "Button index, 1-based — 1 is primary." },
    ],
    src: "scripting/hid.rs" },

  { group: "callbacks", kind: "callback", mode: "doc", sig: "fn on_button_up(host, button)",
    ret: "ignored",
    desc: "HID. A pointer button was released.",
    params: [
      { key: "host", label: "host", type: "BluetoothHidHost", doc: "The host." },
      { key: "button", label: "button", type: "int", doc: "Button index, 1-based." },
    ],
    src: "scripting/hid.rs" },

  // Broadcast source. Android's BluetoothLeBroadcast.Callback, minus the ones
  // that need a framework we do not have.
  { group: "callbacks", kind: "callback", mode: "doc",
    sig: "fn on_broadcast_started(broadcast, reason, broadcast_id)", ret: "ignored",
    desc: "Source. The BIG was created — the broadcast exists on the air.",
    prose: "Raised on the edge into bring-up's second half (data paths opening, or streaming), and " +
      "only once per broadcast: a source that goes on to fail after this point reports " +
      "<code>on_broadcast_stopped</code>, not <code>on_broadcast_start_failed</code>.",
    params: [
      { key: "b", label: "broadcast", type: "BluetoothLeBroadcast", doc: "The source." },
      { key: "reason", label: "reason", type: "int", doc: REASON_DOC },
      { key: "id", label: "broadcast_id", type: "int", doc: "The 24-bit Broadcast_ID that started." },
    ],
    src: "scripting/broadcast.rs" },

  { group: "callbacks", kind: "callback", mode: "doc",
    sig: "fn on_broadcast_start_failed(broadcast, reason)", ret: "ignored",
    desc: "Source. Bring-up failed before the BIG ever existed.",
    prose: "The one broadcast callback with no Broadcast_ID: nothing was created, so there is nothing " +
      "to name. Arity 2 for that reason.",
    params: [
      { key: "b", label: "broadcast", type: "BluetoothLeBroadcast", doc: "The source." },
      { key: "reason", label: "reason", type: "int", doc: REASON_DOC },
    ],
    src: "scripting/broadcast.rs" },

  { group: "callbacks", kind: "callback", mode: "doc",
    sig: "fn on_broadcast_stopped(broadcast, reason, broadcast_id)", ret: "ignored",
    desc: "Source. The BIG was terminated, cleanly or otherwise.",
    prose: "Follows <code>on_playback_stopped</code> when the broadcast was streaming. A non-zero " +
      "<code>reason</code> means it ended by failure rather than by <code>stop_broadcast</code>.",
    params: [
      { key: "b", label: "broadcast", type: "BluetoothLeBroadcast", doc: "The source." },
      { key: "reason", label: "reason", type: "int", doc: REASON_DOC },
      { key: "id", label: "broadcast_id", type: "int", doc: "The Broadcast_ID that stopped." },
    ],
    src: "scripting/broadcast.rs" },

  { group: "callbacks", kind: "callback", mode: "doc",
    sig: "fn on_broadcast_updated(broadcast, reason, broadcast_id)", ret: "ignored",
    desc: "Source. An `update_broadcast` reached the periodic train.",
    prose: "Followed immediately by <code>on_broadcast_metadata_changed</code> carrying the new BASE.",
    params: [
      { key: "b", label: "broadcast", type: "BluetoothLeBroadcast", doc: "The source." },
      { key: "reason", label: "reason", type: "int", doc: REASON_DOC },
      { key: "id", label: "broadcast_id", type: "int", doc: "The Broadcast_ID that was updated." },
    ],
    src: "scripting/broadcast.rs" },

  { group: "callbacks", kind: "callback", mode: "doc",
    sig: "fn on_broadcast_update_failed(broadcast, reason, broadcast_id)", ret: "ignored",
    desc: "Source. The controller refused the periodic-advertising-data rewrite.",
    params: [
      { key: "b", label: "broadcast", type: "BluetoothLeBroadcast", doc: "The source." },
      { key: "reason", label: "reason", type: "int", doc: REASON_DOC },
      { key: "id", label: "broadcast_id", type: "int", doc: "The Broadcast_ID that was being updated." },
    ],
    src: "scripting/broadcast.rs" },

  { group: "callbacks", kind: "callback", mode: "doc",
    sig: "fn on_broadcast_metadata_changed(broadcast, broadcast_id, metadata)", ret: "ignored",
    desc: "Source. The published metadata changed — here is the new `BluetoothLeBroadcastMetadata`.",
    prose: "Note the argument order: this one carries the id <em>second</em> and a map third, where " +
      "the rest carry a reason second. It is the only broadcast callback whose third argument is not " +
      "an integer, and dispatch is by name and arity, so getting the order wrong is silent.",
    params: [
      { key: "b", label: "broadcast", type: "BluetoothLeBroadcast", doc: "The source." },
      { key: "id", label: "broadcast_id", type: "int", doc: "The Broadcast_ID whose metadata changed." },
      { key: "metadata", label: "metadata", type: "map",
        doc: "The same map <code>get_all_broadcast_metadata()</code> returns." },
    ],
    src: "scripting/broadcast.rs" },

  { group: "callbacks", kind: "callback", mode: "doc",
    sig: "fn on_playback_started(broadcast, reason, broadcast_id)", ret: "ignored",
    desc: "Source. The BIG reached Streaming — audio can now flow.",
    prose: "Distinct from <code>on_broadcast_started</code>: that one fires when the BIG is created, " +
      "this when it is actually streaming. <code>send_audio</code> returns <code>false</code> until " +
      "this point.",
    params: [
      { key: "b", label: "broadcast", type: "BluetoothLeBroadcast", doc: "The source." },
      { key: "reason", label: "reason", type: "int", doc: REASON_DOC },
      { key: "id", label: "broadcast_id", type: "int", doc: "The Broadcast_ID now streaming." },
    ],
    src: "scripting/broadcast.rs" },

  { group: "callbacks", kind: "callback", mode: "doc",
    sig: "fn on_playback_stopped(broadcast, reason, broadcast_id)", ret: "ignored",
    desc: "Source. Streaming ended.",
    prose: "Raised only if the broadcast was streaming, and always before " +
      "<code>on_broadcast_stopped</code>.",
    params: [
      { key: "b", label: "broadcast", type: "BluetoothLeBroadcast", doc: "The source." },
      { key: "reason", label: "reason", type: "int", doc: REASON_DOC },
      { key: "id", label: "broadcast_id", type: "int", doc: "The Broadcast_ID that stopped streaming." },
    ],
    src: "scripting/broadcast.rs" },

  // Assistant. Each BASS operation answers with exactly one of a
  // success/failure pair, decided by the control-point write's ATT status.
  { group: "callbacks", kind: "callback", mode: "doc",
    sig: "fn on_source_added(assistant, sink, source_id, reason)", ret: "ignored",
    desc: "Assistant. The Delegator accepted an Add Source and assigned it a Source_ID.",
    prose: "Deferred on purpose: the Source_ID is BASS's to assign, so this waits for the Broadcast " +
      "Receive State that carries it rather than firing when the write is answered. " +
      "<code>on_receive_state_changed</code> follows with the state itself.",
    params: [
      { key: "a", label: "assistant", type: "BluetoothLeBroadcastAssistant", doc: "The Assistant." },
      { key: "sink", label: "sink", type: "string", doc: SINK_ARG_DOC },
      { key: "id", label: "source_id", type: "int", doc: SOURCE_ID_DOC },
      { key: "reason", label: "reason", type: "int", doc: REASON_DOC },
    ],
    src: "scripting/broadcast.rs" },

  { group: "callbacks", kind: "callback", mode: "doc",
    sig: "fn on_source_add_failed(assistant, sink, metadata, reason)", ret: "ignored",
    desc: "Assistant. The Delegator rejected the Add Source write.",
    prose: "The failure half carries the <em>metadata</em> that was refused, not a Source_ID — none " +
      "was ever assigned. That is why its third argument differs from every other failure callback's.",
    params: [
      { key: "a", label: "assistant", type: "BluetoothLeBroadcastAssistant", doc: "The Assistant." },
      { key: "sink", label: "sink", type: "string", doc: SINK_ARG_DOC },
      { key: "metadata", label: "metadata", type: "map", doc: "The metadata map that was refused." },
      { key: "reason", label: "reason", type: "int", doc: REASON_DOC },
    ],
    src: "scripting/broadcast.rs" },

  { group: "callbacks", kind: "callback", mode: "doc",
    sig: "fn on_source_modified(assistant, sink, source_id, reason)", ret: "ignored",
    desc: "Assistant. A Modify Source write was accepted.",
    params: [
      { key: "a", label: "assistant", type: "BluetoothLeBroadcastAssistant", doc: "The Assistant." },
      { key: "sink", label: "sink", type: "string", doc: SINK_ARG_DOC },
      { key: "id", label: "source_id", type: "int", doc: SOURCE_ID_DOC },
      { key: "reason", label: "reason", type: "int", doc: REASON_DOC },
    ],
    src: "scripting/broadcast.rs" },

  { group: "callbacks", kind: "callback", mode: "doc",
    sig: "fn on_source_modify_failed(assistant, sink, source_id, reason)", ret: "ignored",
    desc: "Assistant. A Modify Source write was rejected.",
    params: [
      { key: "a", label: "assistant", type: "BluetoothLeBroadcastAssistant", doc: "The Assistant." },
      { key: "sink", label: "sink", type: "string", doc: SINK_ARG_DOC },
      { key: "id", label: "source_id", type: "int", doc: SOURCE_ID_DOC },
      { key: "reason", label: "reason", type: "int", doc: REASON_DOC },
    ],
    src: "scripting/broadcast.rs" },

  { group: "callbacks", kind: "callback", mode: "doc",
    sig: "fn on_source_removed(assistant, sink, source_id, reason)", ret: "ignored",
    desc: "Assistant. A Remove Source write was accepted and the slot forgotten.",
    prose: "The Assistant drops its cached state for that Source_ID here, so " +
      "<code>get_all_sources</code> stops returning it.",
    params: [
      { key: "a", label: "assistant", type: "BluetoothLeBroadcastAssistant", doc: "The Assistant." },
      { key: "sink", label: "sink", type: "string", doc: SINK_ARG_DOC },
      { key: "id", label: "source_id", type: "int", doc: SOURCE_ID_DOC },
      { key: "reason", label: "reason", type: "int", doc: REASON_DOC },
    ],
    src: "scripting/broadcast.rs" },

  { group: "callbacks", kind: "callback", mode: "doc",
    sig: "fn on_source_remove_failed(assistant, sink, source_id, reason)", ret: "ignored",
    desc: "Assistant. A Remove Source write was rejected.",
    params: [
      { key: "a", label: "assistant", type: "BluetoothLeBroadcastAssistant", doc: "The Assistant." },
      { key: "sink", label: "sink", type: "string", doc: SINK_ARG_DOC },
      { key: "id", label: "source_id", type: "int", doc: SOURCE_ID_DOC },
      { key: "reason", label: "reason", type: "int", doc: REASON_DOC },
    ],
    src: "scripting/broadcast.rs" },

  { group: "callbacks", kind: "callback", mode: "doc",
    sig: "fn on_search_started(assistant, reason)", ret: "ignored",
    desc: "Assistant. Remote Scan Started reached the sink.",
    prose: "Arity 2: a sink-wide operation names no source, so there is no Source_ID to carry. It " +
      "confirms the <em>BASS write</em> only — no scan is running, and no " +
      "<code>on_source_found</code> will follow.",
    params: [
      { key: "a", label: "assistant", type: "BluetoothLeBroadcastAssistant", doc: "The Assistant." },
      { key: "reason", label: "reason", type: "int", doc: REASON_DOC },
    ],
    src: "scripting/broadcast.rs" },

  { group: "callbacks", kind: "callback", mode: "doc",
    sig: "fn on_search_start_failed(assistant, reason)", ret: "ignored",
    desc: "Assistant. The sink rejected Remote Scan Started.",
    params: [
      { key: "a", label: "assistant", type: "BluetoothLeBroadcastAssistant", doc: "The Assistant." },
      { key: "reason", label: "reason", type: "int", doc: REASON_DOC },
    ],
    src: "scripting/broadcast.rs" },

  { group: "callbacks", kind: "callback", mode: "doc",
    sig: "fn on_search_stopped(assistant, reason)", ret: "ignored",
    desc: "Assistant. Remote Scan Stopped reached the sink.",
    params: [
      { key: "a", label: "assistant", type: "BluetoothLeBroadcastAssistant", doc: "The Assistant." },
      { key: "reason", label: "reason", type: "int", doc: REASON_DOC },
    ],
    src: "scripting/broadcast.rs" },

  { group: "callbacks", kind: "callback", mode: "doc",
    sig: "fn on_search_stop_failed(assistant, reason)", ret: "ignored",
    desc: "Assistant. The sink rejected Remote Scan Stopped.",
    params: [
      { key: "a", label: "assistant", type: "BluetoothLeBroadcastAssistant", doc: "The Assistant." },
      { key: "reason", label: "reason", type: "int", doc: REASON_DOC },
    ],
    src: "scripting/broadcast.rs" },

  { group: "callbacks", kind: "callback", mode: "doc",
    sig: "fn on_receive_state_changed(assistant, sink, source_id, state)", ret: "ignored",
    desc: "Assistant. A Broadcast Receive State notification arrived — what the earbud is actually synchronised to.",
    prose: "The callback that tells you whether the join worked, as opposed to whether the write was " +
      "accepted. An empty Receive State — the slot a Remove Source leaves behind — is <em>not</em> " +
      "reported here; that edge is <code>on_source_removed</code>.",
    note: "One slot. A Delegator may publish several Broadcast Receive State characteristics, one per " +
      "source, but <code>LeCentral</code> resolves a characteristic by UUID and takes the first match " +
      "— so this Assistant reads and subscribes to the first slot only.",
    params: [
      { key: "a", label: "assistant", type: "BluetoothLeBroadcastAssistant", doc: "The Assistant." },
      { key: "sink", label: "sink", type: "string", doc: SINK_ARG_DOC },
      { key: "id", label: "source_id", type: "int", doc: SOURCE_ID_DOC },
      { key: "state", label: "state", type: "map",
        doc: "The Broadcast Receive State — the same map <code>server.receive_states()</code> and " +
          "<code>assistant.get_all_sources()</code> return. <code>state.pa_sync_state</code> is the " +
          "field that says whether the earbud joined." },
    ],
    src: "scripting/broadcast.rs" },

  { group: "callbacks", kind: "syntax", mode: "doc", sig: 'wait_for "event_name" { … }',
    ret: "the block's value",
    desc: "Custom syntax, not a function: drain queued events until one matches, bind it as `event`, run the block.",
    prose: "Semantics are synchronous. Earlier non-matching events are consumed — <code>wait_for</code> " +
      "models “everything up to this event has happened”. If no matching event is left in the queue it " +
      "raises an error rather than silently skipping the block: under synchronous ATT dispatch, an event " +
      "that has not arrived by now never will.",
    note: "Not usable from this page: the Explorer drains the queue after every Execute, so " +
      "<code>wait_for</code> here always finds an empty queue and errors.",
    params: [
      { key: "name", label: "event_name", type: "string literal",
        doc: "The event kind to wait for — <code>\"connected\"</code>, " +
          "<code>\"characteristic_write\"</code>, … See Event maps." },
      { key: "block", label: "{ … }", type: "block",
        doc: "Runs with <code>event</code> bound to the matched event map. Captured at parse time, " +
          "which is why this is syntax rather than a function." },
    ],
    src: "scripting/bindings.rs" },

  // ---- Event maps ----------------------------------------------------------
  { group: "events", kind: "consts", mode: "doc", sig: "event — peripheral fields", ret: "map",
    desc: "What `on_event(server, event)` and `take_events()` hand a peripheral script.",
    prose: "Absent fields are omitted, so test with <code>\"uuid\" in event</code> before reading one.",
    table: { cols: ["Field", "Type", "Meaning"], rows: [
      ["event", "string", "The event kind — the value listed in the next table."],
      ["server", "string", "Name of the server that raised it; one queue serves every server in a session."],
      ["device", "BluetoothDevice", "The peer involved, on the events that have one."],
      ["uuid", "Uuid", "The characteristic or descriptor the event concerns."],
      ["value", "blob", "The bytes carried — the value written, on a write request."],
      ["request_id", "int", "ATT request id, to be echoed back to send_response."],
      ["offset", "int", "Byte offset the read or write began at."],
      ["status", "int", "Status or error code, where the event carries one."],
      ["mtu", "int", "The negotiated MTU, on mtu_changed."],
      ["response_needed", "bool", "Whether a write expects a response."],
      ["…payload", "any", "A host-pushed event merges its own JSON payload keys in at the top level."],
    ] },
    src: "scripting/bindings.rs" },

  { group: "events", kind: "consts", mode: "doc", sig: "event.event — peripheral kinds", ret: "string",
    desc: "The event kinds a server raises.",
    table: { cols: ["Kind", "Carries", "Raised when"], rows: [
      ["connected", "device, status", "A central connected. Named by edge rather than by Android's numeric STATE_* pair."],
      ["disconnected", "device, status", "The link went away."],
      ["service_added", "uuid, status", "add_service finished registering a service."],
      ["characteristic_read", "device, request_id, offset, uuid", "A peer read a characteristic."],
      ["characteristic_write", "device, request_id, offset, uuid, value, response_needed", "A peer wrote a characteristic."],
      ["descriptor_read", "device, request_id, offset, uuid", "A peer read a descriptor."],
      ["descriptor_write", "device, request_id, offset, uuid, value, response_needed", "A peer wrote a descriptor — a CCCD write is a subscribe."],
      ["notification_sent", "device, status", "A notification or indication went out."],
      ["mtu_changed", "device, mtu", "The ATT MTU was negotiated."],
    ] },
    src: "scripting/bindings.rs" },

  { group: "events", kind: "consts", mode: "doc", sig: "event.event — central kinds", ret: "string",
    desc: "The event kinds `on_event(client, event)` sees.",
    prose: "Same vocabulary as the peripheral's maps, so a script that reads one reads the other. Note " +
      "that a central's map carries <code>peer</code> (a string) rather than a " +
      "<code>BluetoothDevice</code>, and <code>handle</code> alongside each UUID.",
    table: { cols: ["Kind", "Carries", "Raised when"], rows: [
      ["connected", "peer, status", "The link came up."],
      ["disconnected", "peer, status", "The link went away."],
      ["mtu_changed", "mtu", "The MTU exchange completed."],
      ["services_discovered", "services (a count)", "Discovery finished."],
      ["characteristic_read", "uuid, handle, value, status", "A read came back."],
      ["characteristic_write", "uuid, handle, status", "A write was answered."],
      ["characteristic_changed", "uuid, handle, value", "A notification or indication arrived."],
      ["subscription_changed", "uuid, handle, enabled, status", "A CCCD write took effect."],
      ["operation_failed", "uuid, operation, reason", "An operation could not start at all."],
    ] },
    src: "scripting/client.rs" },

  // ---- Constants -----------------------------------------------------------
  { group: "constants", kind: "consts", mode: "doc", sig: "android::PROPERTY_*", ret: "int (bitmask)",
    desc: "What a peer may do with a characteristic. OR them together.",
    prose: "Independent of permissions: a property advertises a capability, a permission decides whether " +
      "the peer is allowed to use it.",
    table: { cols: ["Constant", "Value", "Meaning"],
      rows: PROPERTY_OPTIONS.map(([n, , v, d]) => [`PROPERTY_${n}`, v, d]) },
    src: "android/gatt_service.rs" },

  { group: "constants", kind: "consts", mode: "doc", sig: "android::PERMISSION_*", ret: "int (bitmask)",
    desc: "What security a peer needs to exercise a property. OR them together.",
    table: { cols: ["Constant", "Value", "Meaning"],
      rows: PERMISSION_OPTIONS.map(([n, , v, d]) => [`PERMISSION_${n}`, v, d]) },
    src: "android/gatt_service.rs" },

  { group: "constants", kind: "consts", mode: "doc", sig: "android::SERVICE_TYPE_*", ret: "int",
    desc: "Primary or secondary. Not a bitmask — pick one.",
    table: { cols: ["Constant", "Value", "Meaning"], rows: [
      ["SERVICE_TYPE_PRIMARY", "0", "A service a peer discovers directly. Almost always this one."],
      ["SERVICE_TYPE_SECONDARY", "1", "A service meant only to be included by another; not returned by primary service discovery."],
    ] },
    src: "android/gatt_service.rs" },

  { group: "constants", kind: "consts", mode: "doc", sig: "android::GATT_*", ret: "int",
    desc: "Operation status, as `send_response` takes it and `notify_characteristic_changed` returns it.",
    prose: "These are Android's application-level codes. The low ones coincide with ATT error codes; " +
      "<code>GATT_FAILURE</code> (257) is Android's own catch-all and has no wire equivalent.",
    table: { cols: ["Constant", "Value", "Meaning"], rows: [
      ["GATT_SUCCESS", "0", "The operation succeeded."],
      ["GATT_READ_NOT_PERMITTED", "2", "The attribute cannot be read."],
      ["GATT_WRITE_NOT_PERMITTED", "3", "The attribute cannot be written."],
      ["GATT_INSUFFICIENT_AUTHENTICATION", "5", "The link is not authenticated enough."],
      ["GATT_REQUEST_NOT_SUPPORTED", "6", "The server does not support this request."],
      ["GATT_INVALID_OFFSET", "7", "The offset is past the end of the attribute."],
      ["GATT_INSUFFICIENT_AUTHORIZATION", "8", "The peer is authenticated but not authorised."],
      ["GATT_INVALID_ATTRIBUTE_LENGTH", "13", "The value is the wrong length for this attribute."],
      ["GATT_INSUFFICIENT_ENCRYPTION", "15", "The link must be encrypted first."],
      ["GATT_CONNECTION_CONGESTED", "143", "Too much is already queued on this link."],
      ["GATT_FAILURE", "257", "Anything else. Android's catch-all."],
    ] },
    src: "android/gatt_service.rs" },

  { group: "constants", kind: "consts", mode: "doc", sig: "android::STATE_*", ret: "int",
    desc: "Connection states, from Android's BluetoothProfile.",
    prose: "Registered for completeness. Scripts here match connection edges by event name " +
      "(<code>\"connected\"</code> / <code>\"disconnected\"</code>) rather than on these numbers.",
    table: { cols: ["Constant", "Value", "Meaning"], rows: [
      ["STATE_DISCONNECTED", "0", "No link."],
      ["STATE_CONNECTING", "1", "Bring-up in progress."],
      ["STATE_CONNECTED", "2", "Link established."],
      ["STATE_DISCONNECTING", "3", "Tear-down in progress."],
    ] },
    src: "android/gatt_server.rs" },

  { group: "constants", kind: "consts", mode: "doc", sig: "uuid::* — GATT and generic", ret: "Uuid",
    desc: "The assigned numbers you reach for building an ordinary device.",
    table: { cols: ["Constant", "16-bit", "Meaning"], rows: [
      ["HEART_RATE_SERVICE", "180D", "Heart Rate Service."],
      ["HEART_RATE_MEASUREMENT", "2A37", "Heart Rate Measurement — [flags, bpm, …], notify-only."],
      ["BODY_SENSOR_LOCATION", "2A38", "Where on the body the sensor sits."],
      ["BATTERY_SERVICE", "180F", "Battery Service."],
      ["BATTERY_LEVEL", "2A19", "Battery Level, one byte of percent."],
      ["CLIENT_CHARACTERISTIC_CONFIGURATION", "2902", "The CCCD. A central writes 0x0001 to notify, 0x0002 to indicate."],
      ["CHARACTERISTIC_USER_DESCRIPTION", "2901", "A human-readable name for a characteristic."],
      ["GENERIC_ATTRIBUTE_SERVICE", "1801", "The GATT service itself."],
      ["SERVICE_CHANGED", "2A05", "Tells a bonded client its cached database is stale."],
      ["CLIENT_SUPPORTED_FEATURES", "2B29", "Client feature bits."],
      ["DATABASE_HASH", "2B2A", "Hash of the database, so a client can skip rediscovery."],
      ["SERVER_SUPPORTED_FEATURES", "2B3A", "Server feature bits."],
    ] },
    src: "scripting/constants.rs · profiles/hrs.rs · profiles/bas.rs · gatt/database.rs" },

  { group: "constants", kind: "consts", mode: "doc", sig: "uuid::* — Device Information", ret: "Uuid",
    desc: "The Device Information Service characteristics (DIS, 180A).",
    table: { cols: ["Constant", "16-bit", "Meaning"], rows: [
      ["SYSTEM_ID", "2A23", "IEEE 11073-20601 system id."],
      ["MODEL_NUMBER", "2A24", "Model number, as a string."],
      ["SERIAL_NUMBER", "2A25", "Serial number, as a string."],
      ["FIRMWARE_REVISION", "2A26", "Firmware revision, as a string."],
      ["HARDWARE_REVISION", "2A27", "Hardware revision, as a string."],
      ["SOFTWARE_REVISION", "2A28", "Software revision, as a string."],
      ["MANUFACTURER_NAME", "2A29", "Manufacturer name, as a string."],
      ["IEEE_REGULATORY", "2A2A", "IEEE 11073-20601 regulatory certification data."],
      ["PNP_ID", "2A50", "Vendor and product ids."],
    ] },
    src: "profiles/dis.rs" },

  { group: "constants", kind: "consts", mode: "doc", sig: "uuid::* — LE Audio and ranging", ret: "Uuid",
    desc: "The profile UUIDs the Rust registrars publish.",
    prose: "You rarely name these directly — <code>add_pacs</code>, <code>add_ascs</code> and " +
      "<code>add_ras</code> install the services for you. They are here for assertions and for " +
      "<code>advertise_service_uuid</code>.",
    table: { cols: ["Constant", "16-bit", "Meaning"], rows: [
      ["PACS_SERVICE", "1850", "Published Audio Capabilities."],
      ["SINK_PAC", "2BC9", "Sink PAC records — what the device can decode."],
      ["SINK_AUDIO_LOCATIONS", "2BCA", "Sink channel allocation bitmask."],
      ["SOURCE_PAC", "2BCB", "Source PAC records — what it can encode."],
      ["SOURCE_AUDIO_LOCATIONS", "2BCC", "Source channel allocation bitmask."],
      ["AVAILABLE_AUDIO_CONTEXTS", "2BCD", "Contexts available right now."],
      ["SUPPORTED_AUDIO_CONTEXTS", "2BCE", "Contexts supported at all."],
      ["AUDIO_STREAM_CONTROL_SERVICE", "184E", "Audio Stream Control (ASCS)."],
      ["SINK_ASE", "2BC4", "A sink Audio Stream Endpoint."],
      ["SOURCE_ASE", "2BC5", "A source Audio Stream Endpoint."],
      ["ASE_CONTROL_POINT", "2BC6", "Where Config Codec / Enable / Release are written."],
      ["BASIC_AUDIO_ANNOUNCEMENT_SERVICE", "1851", "Broadcast BASE announcement."],
      ["BROADCAST_AUDIO_ANNOUNCEMENT_SERVICE", "1852", "Broadcast id announcement."],
      ["VOLUME_CONTROL_SERVICE", "1844", "Volume Control (VCS)."],
      ["VOLUME_STATE", "2B7D", "Current volume, mute and change counter."],
      ["VOLUME_CONTROL_POINT", "2B7E", "Where volume changes are written."],
      ["VOLUME_FLAGS", "2B7F", "Whether the volume setting is persisted."],
      ["VOLUME_OFFSET_CONTROL_SERVICE", "1845", "Volume Offset Control (VOCS)."],
      ["VOLUME_OFFSET_STATE", "2B80", "Per-output offset and change counter."],
      ["AUDIO_LOCATION", "2B81", "This output's channel allocation."],
      ["VOLUME_OFFSET_CONTROL_POINT", "2B82", "Where offset changes are written."],
      ["AUDIO_OUTPUT_DESCRIPTION", "2B83", "A name for the output."],
      ["AUDIO_INPUT_CONTROL_SERVICE", "1843", "Audio Input Control (AICS)."],
      ["AUDIO_INPUT_STATE", "2B77", "Gain, mute and gain mode."],
      ["GAIN_SETTINGS_ATTRIBUTE", "2B78", "Gain units, minimum and maximum."],
      ["AUDIO_INPUT_TYPE", "2B79", "What kind of input this is."],
      ["AUDIO_INPUT_STATUS", "2B7A", "Whether the input is active."],
      ["AUDIO_INPUT_CONTROL_POINT", "2B7B", "Where gain and mute are written."],
      ["AUDIO_INPUT_DESCRIPTION", "2B7C", "A name for the input."],
      ["CSIS_SERVICE", "1846", "Coordinated Set Identification (CSIS) — the earbud-pair service."],
      ["SET_IDENTITY_RESOLVING_KEY", "2B84", "The SIRK that identifies the set."],
      ["SET_MEMBER_SIZE", "2B85", "How many members the set has."],
      ["SET_MEMBER_LOCK", "2B86", "The lock that serialises access to the set."],
      ["SET_MEMBER_RANK", "2B87", "This member's rank within the set."],
      ["RANGING_SERVICE", "185B", "Ranging Service — the GATT half of Channel Sounding."],
      ["RANGING_FEATURES", "2C14", "Supported ranging features."],
      ["RANGING_REALTIME_DATA", "2C15", "Streamed ranging data."],
      ["RANGING_ON_DEMAND_DATA", "2C16", "Ranging data fetched on request."],
      ["RANGING_CONTROL_POINT", "2C17", "Where ranging procedures are started."],
    ] },
    src: "profiles/pacs.rs · ascs.rs · bap.rs · vcp.rs · vocs.rs · aics.rs · csip.rs · ras.rs" },

  { group: "constants", kind: "consts", mode: "doc", sig: "audio::location::* · audio::context::*",
    ret: "int (bitmask)",
    desc: "LE Audio channel allocations and context types, for `add_pacs`.",
    prose: "<code>audio::location::*</code> holds the 32-bit Audio Location bits — " +
      "<code>FRONT_LEFT</code>, <code>FRONT_RIGHT</code>, <code>STEREO</code> (both), through the full " +
      "surround set: centre, LFE 1 and 2, back, side, top and bottom positions, the wide and surround " +
      "pairs, and <code>NOT_ALLOWED</code> for none. <code>audio::context::*</code> holds the context " +
      "types — <code>UNSPECIFIED</code>, <code>CONVERSATIONAL</code>, <code>MEDIA</code>, " +
      "<code>GAME</code>, <code>INSTRUCTIONAL</code>, <code>VOICE_ASSISTANTS</code>, <code>LIVE</code>, " +
      "<code>SOUND_EFFECTS</code>, <code>NOTIFICATIONS</code>, <code>RINGTONE</code>, " +
      "<code>ALERTS</code>, <code>EMERGENCY_ALARM</code>, and <code>PROHIBITED</code>. Both are " +
      "sub-modules, so the values are spelled out in full: <code>audio::location::FRONT_LEFT</code>.",
    src: "scripting/constants.rs · profiles/bap.rs" },
];

const NAME_PREFIX = { server: "srv", service: "svc", characteristic: "chr", descriptor: "dsc", device: "dev" };
// Three-or-four letters, so the gutter stays one width and the signatures line
// up down the page rather than stepping in and out per section.
const KIND_LABEL = { ctor: "new", prop: "get", method: "fn", callback: "cb", syntax: "syn", consts: "doc" };
const SECTION_LABEL = { ctor: "Constructor", prop: "Properties", method: "Methods",
  callback: "Callbacks", syntax: "Syntax", consts: "Reference" };

// --- session + registry state ----------------------------------------------
const $ = (id) => document.getElementById(id);
let session = null;
let lastConnectAttempt = 0;
let openedOnce = false;
let startTime = performance.now();
const prevValues = new Map();

// Every object type a member can be a receiver of, bind, or take as an objref —
// keep in sync with the `receiver`/`binds`/`objType` values in the member data,
// or `refreshObjrefs` reads `.length` off an undefined entry and throws.
const OBJ_TYPES = [
  "server", "service", "characteristic", "descriptor", "device",
  "client", "host", "scanner", "broadcast", "assistant",
];
const registry = Object.fromEntries(OBJ_TYPES.map((t) => [t, []]));
const counters = Object.fromEntries(OBJ_TYPES.map((t) => [t, 0]));
const sessionLines = []; // successful lines, for "copy as script"

const peekName = (type) => `${NAME_PREFIX[type]}${counters[type] + 1}`;
function allocName(type) {
  counters[type] += 1;
  const name = `${NAME_PREFIX[type]}${counters[type]}`;
  registry[type].push(name);
  return name;
}

// --- form field rendering --------------------------------------------------
// Attribute values go through the shared escapeHtml, which escapes quotes as
// well as angle brackets — this file used to wrap it in a local `attr` that
// re-escaped `"` a second time, from back when the shared one left quotes
// alone. (Descriptions, by contrast, are authored HTML — they carry <code> on
// purpose, so they are interpolated raw.)

function uuidField(p) {
  // presetExpr, not an index: the option list grows and a numeric preset would
  // silently start pointing at a different UUID.
  const preset = p.presetExpr ? Math.max(0, uuidIndex(p.presetExpr)) : 0;
  const opts = UUID_OPTIONS.map(([label, expr], i) =>
    `<option value="${i}"${i === preset ? " selected" : ""}>${escapeHtml(label)}</option>`).join("");
  return `<select data-role="uuid-select">${opts}</select>
    <input type="text" class="custom-uuid" data-role="uuid-custom" placeholder='e.g. 2A6E or a 128-bit UUID' hidden>`;
}
function flagsField(p) {
  return `<div class="flags">${p.options.map(([label, , value, doc], i) =>
    `<label title="${escapeHtml(`${value} — ${doc || ""}`)}"><input type="checkbox" data-role="flag" value="${i}"${
      (p.defaults || []).includes(i) ? " checked" : ""}> ${escapeHtml(label)}</label>`).join("")}</div>`;
}
function selectField(role, options) {
  return `<select data-role="${role}">${options.map(([label, expr]) =>
    `<option value="${escapeHtml(expr)}">${escapeHtml(label)}</option>`).join("")}</select>`;
}

function fieldControl(p) {
  switch (p.kind) {
    case "text": return `<input type="text" data-role="text" value="${escapeHtml(p.default || "")}">`;
    case "code": return `<input type="text" data-role="code" value="${escapeHtml(p.default || "")}" spellcheck="false">`;
    case "number": return `<input type="text" data-role="number" value="${escapeHtml(p.default || "0")}">`;
    case "bytes": return `<input type="text" data-role="bytes" value="${escapeHtml(p.default || "")}" placeholder="e.g. 0x00, 72">`;
    case "uuid": return uuidField(p);
    case "flags": return flagsField(p);
    case "servicetype": return selectField("expr", SERVICE_TYPE_OPTIONS);
    case "status": return selectField("expr", GATT_STATUS_OPTIONS);
    case "bool": return `<select data-role="expr"><option value="false">false</option><option value="true">true</option></select>`;
    case "objref": return `<select data-role="objref" data-objtype="${p.objType}"></select>`;
    default: return "";
  }
}

// One parameter row: name, type and required-ness in the left column; the
// control and its own description in the right. The same table serves
// documentation-only members, minus the control — so a callback's arguments are
// documented in exactly the shape a method's are.
function paramRow(p, withControl) {
  const req = p.opt
    ? `<div class="preq opt">optional</div>`
    : `<div class="preq">required</div>`;
  const ctl = withControl ? `<div class="pctl">${fieldControl(p)}</div>` : "";
  return `<div class="param${withControl ? "" : " doconly"}" data-key="${p.key}">
    <div class="pname">${escapeHtml(p.label)}</div>
    <div class="ptype">${escapeHtml(p.type || "")}</div>
    ${withControl ? req : ""}
    ${ctl}
    <div class="pdoc">${p.doc || ""}</div>
  </div>`;
}

function paramsTable(m) {
  const withControl = m.mode !== "doc";
  const rows = [];
  if (m.receiver) {
    // The receiver is an argument like any other, so it belongs in the table
    // rather than floating above it as a stray "on" row.
    const control = m.mode === "ref"
      ? `<div class="pctl"><span class="fixed">${escapeHtml(m.receiverName)}</span></div>`
      : `<div class="pctl"><select data-role="receiver" data-objtype="${m.receiver}"></select></div>`;
    rows.push(`<div class="param" data-key="__receiver">
      <div class="pname">${escapeHtml(m.receiver === "client" ? "client" : m.receiver)}</div>
      <div class="ptype">${escapeHtml(receiverType(m.receiver))}</div>
      <div class="preq">receiver</div>
      ${control}
      <div class="pdoc">${m.mode === "ref"
        // Named by type rather than "the client": four different types are
        // reference-only now, and only one of them is a client.
        ? `The variable your script bound the ${escapeHtml(receiverType(m.receiver))} to.`
        : "Which bound object to call this on."}</div>
    </div>`);
  }
  // Prose-only reference entries (the audio constant modules) carry neither
  // params nor a table — they are a paragraph and nothing else.
  for (const p of m.params || []) rows.push(paramRow(p, withControl));
  if (!rows.length) return "";
  return `<div class="params"><div class="params-head">Parameters</div>${rows.join("")}</div>`;
}

const receiverType = (t) => ({
  server: "BluetoothGattServer", service: "BluetoothGattService",
  characteristic: "BluetoothGattCharacteristic", descriptor: "BluetoothGattDescriptor",
  device: "BluetoothDevice", client: "BluetoothGatt", host: "BluetoothHidHost",
  broadcast: "BluetoothLeBroadcast", assistant: "BluetoothLeBroadcastAssistant",
}[t] || t);

function constTable(t) {
  const head = `<tr>${t.cols.map((c) => `<th>${escapeHtml(c)}</th>`).join("")}</tr>`;
  const body = t.rows.map(([n, v, m]) =>
    `<tr><td class="n">${escapeHtml(n)}</td><td class="v">${escapeHtml(v)}</td>` +
    `<td class="m">${escapeHtml(m)}</td></tr>`).join("");
  // The wrapper scrolls rather than the page: a wide table must not be the
  // reason the whole document gains a horizontal scrollbar.
  return `<div class="params"><div class="tblwrap"><table class="consts">${head}${body}</table></div></div>`;
}

// One-line descriptions are written as plain text with backticks, the way the
// Rust doc comments they mirror are. Escape first, then promote the ticks —
// otherwise a stray ` shows up in the summary line.
const descHtml = (s) => escapeHtml(s).replace(/`([^`]+)`/g, "<code>$1</code>");

function methodHtml(m) {
  const kindCls = m.kind === "ctor" ? " new" : (m.mode === "ref" ? " ref" : "");
  const body = [];
  // Open, the summary's one-liner is hidden (it duplicates the prose). A member
  // with no prose would then have no description at all, so the one-liner is
  // promoted into the body instead of vanishing.
  body.push(`<p class="prose">${m.prose || descHtml(m.desc)}</p>`);
  if (m.note) body.push(`<p class="note warn">${m.note}</p>`);
  if (m.table) body.push(constTable(m.table));
  else body.push(paramsTable(m));
  if (m.ret) {
    // A constant family has a type, not a return value.
    const lbl = m.kind === "consts" ? "Type" : "Returns";
    body.push(`<p class="returns"><span class="lbl">${lbl}</span><span class="t">${escapeHtml(m.ret)}</span>` +
      (m.retDesc ? ` <span class="rd">— ${escapeHtml(m.retDesc)}</span>` : "") + `</p>`);
  }
  if (m.mode !== "doc") {
    const label = m.mode === "ref" ? "Copy line" : "Execute";
    const cls = m.mode === "ref" ? "exec" : "exec primary";
    body.push(`<div class="exec-row"><code class="preview"></code>
      <button class="${cls}">${label}</button></div><div class="gate"></div>`);
  }
  if (m.src) body.push(`<p class="srcref">defined in <code>src/${escapeHtml(m.src)}</code></p>`);

  return `<details class="method">
    <summary>
      <span class="kind${kindCls}">${KIND_LABEL[m.kind]}</span>
      <span class="sig">${escapeHtml(m.sig)}</span>
      <span class="rettype">${m.ret ? "→ " + escapeHtml(m.ret) : ""}</span>
      <span class="d">${descHtml(m.desc)}</span>
    </summary>
    <div class="method-body">${body.join("")}</div>
  </details>`;
}

function renderMethods() {
  const html = TYPES.map((t) => {
    const members = METHODS.filter((m) => m.group === t.id);
    if (!members.length) return "";
    // Members run in catalog order, but the "Constructor / Properties /
    // Methods" runs are labelled the way an API reference labels them.
    const chunks = [];
    let lastKind = null;
    for (const m of members) {
      if (m.kind !== lastKind) {
        chunks.push(`<div class="members-label">${SECTION_LABEL[m.kind] || ""}</div>`);
        lastKind = m.kind;
      }
      chunks.push(methodHtml(m));
    }
    return `<section class="type" id="t-${t.id}">
      <div class="type-head" data-role="${t.role}">
        <h2>${escapeHtml(t.name)} <span class="role">${escapeHtml(t.role)}</span></h2>
        <p>${t.blurb}</p>
      </div>
      ${chunks.join("")}
    </section>`;
  }).join("");
  $("methods").innerHTML = html;

  // Wire in the same order the DOM was written, so element and entry match.
  const els = $("methods").querySelectorAll(".method");
  let i = 0;
  for (const t of TYPES) {
    for (const m of METHODS.filter((x) => x.group === t.id)) wireMethod(m, els[i++]);
  }
}

// --- resolving a form back into Rhai ---------------------------------------
function resolveParam(fieldEl, p) {
  switch (p.kind) {
    case "text": return fieldEl.querySelector('[data-role=text]').value;
    case "code": return fieldEl.querySelector('[data-role=code]').value.trim() || "()";
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
  for (const p of m.params || []) {
    const fieldEl = bodyEl.querySelector(`.param[data-key="${p.key}"]`);
    a[p.key] = resolveParam(fieldEl, p);
  }
  return a;
}

function buildLine(m, bodyEl, nameForBind) {
  const on = m.receiver
    ? (m.mode === "ref" ? m.receiverName : bodyEl.querySelector('[data-role=receiver]').value)
    : null;
  const expr = m.build(on, collectArgs(bodyEl, m));
  if (!m.binds) return `${expr};`;
  return `let ${m.mode === "ref" ? m.bindsName : nameForBind} = ${expr};`;
}

// Whether every object this call needs already exists. Reference members are
// always "ready": nothing about them depends on the session.
function methodReady(m) {
  if (m.mode === "ref") return true;
  if (m.receiver && registry[m.receiver].length === 0) return false;
  for (const p of m.params || []) {
    if (p.kind === "objref" && registry[p.objType].length === 0) return false;
  }
  return true;
}

function wireMethod(m, el) {
  if (!el || m.mode === "doc") return;
  const body = el.querySelector(".method-body");
  const preview = el.querySelector(".preview");
  const button = el.querySelector(".exec");
  const gate = el.querySelector(".gate");

  for (const sel of body.querySelectorAll('[data-role=uuid-select]')) {
    const custom = sel.parentElement.querySelector('[data-role=uuid-custom]');
    sel.addEventListener("change", () => {
      custom.hidden = UUID_OPTIONS[+sel.value][1] !== "__custom__";
      refresh();
    });
  }

  function refresh() {
    const ready = methodReady(m);
    button.disabled = !ready;
    gate.textContent = ready ? "" : (m.needsDevice
      ? "needs a connected central — bind one with take_events()[0].device first"
      : `create ${missingFor(m)} first`);
    if (m.mode === "ref" && !gate.textContent) {
      gate.textContent = "reference only — nothing on this page hosts a central; copy it into a script";
    }
    preview.innerHTML = highlightRhai(buildLine(m, body, m.binds ? peekName(m.binds) : null));
  }

  body.addEventListener("input", refresh);
  body.addEventListener("change", refresh);
  button.addEventListener("click", () => (m.mode === "ref" ? copyLine(m, body) : execute(m, body)));
  el._refresh = refresh;
  refresh();
}

// Names what is missing, rather than "create the required object(s) first" —
// the reader should not have to re-read the form to find out which one.
function missingFor(m) {
  const want = [];
  if (m.receiver && !registry[m.receiver].length) want.push(`a ${m.receiver}`);
  for (const p of m.params || []) {
    if (p.kind === "objref" && !registry[p.objType].length) want.push(`a ${p.objType}`);
  }
  return want.join(" and ") || "the required object";
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
    } else {
      // Keep the control ENABLED (a disabled select reads as broken); the
      // placeholder plus the row's disabled button convey that you need to
      // create the object first.
      sel.innerHTML = `<option value="">— create a ${type} first —</option>`;
    }
  }
  for (const el of $("methods").querySelectorAll(".method")) el._refresh && el._refresh();
}

// --- execute ---------------------------------------------------------------
function logEntry(html) {
  const log = $("log");
  if (log.firstElementChild === null) log.textContent = "";
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

// A reference member's line is never evaluated — see the file header. Copying
// it is the whole interaction, so say so in the log too, or the click looks
// like nothing happened.
async function copyLine(m, body) {
  const line = buildLine(m, body, null);
  try {
    await navigator.clipboard.writeText(line + "\n");
    logEntry(`<div class="rhai">${highlightRhai(line)}</div><div class="evt">copied — not run (this page hosts servers only)</div>`);
  } catch (_) {
    logEntry(`<div class="rhai">${highlightRhai(line)}</div><div class="err">copy failed — select the line above</div>`);
  }
}

// --- filter + jump chips ---------------------------------------------------
function renderChips() {
  $("chips").innerHTML = TYPES.map((t) =>
    `<a class="jump" href="#t-${t.id}">${escapeHtml(t.short)}</a>`).join(" ");
  $("count").textContent = `${METHODS.length} members · ${TYPES.length} sections`;
}

function applyFilter(q) {
  const needle = q.trim().toLowerCase();
  let shown = 0;
  const els = $("methods").querySelectorAll(".method");
  let i = 0;
  const ordered = TYPES.flatMap((t) => METHODS.filter((m) => m.group === t.id));
  for (const m of ordered) {
    const el = els[i++];
    // Match the signature, the summary and the parameter names — enough to
    // find a member by what you half-remember about it, without matching the
    // whole prose and returning everything.
    const hay = [m.sig, m.desc, ...(m.params || []).map((p) => p.label)].join(" ").toLowerCase();
    const hit = !needle || hay.includes(needle);
    el.hidden = !hit;
    if (hit) shown++;
  }
  // A "Properties" heading over nothing is worse than no heading, so a label
  // survives only if something between it and the next label survived.
  for (const section of $("methods").querySelectorAll(".type")) {
    let label = null, run = false;
    for (const child of section.children) {
      if (child.classList.contains("members-label")) {
        if (label) label.hidden = !run;
        label = child; run = false;
      } else if (child.classList.contains("method") && !child.hidden) {
        run = true;
      }
    }
    if (label) label.hidden = !run;
    section.hidden = ![...section.querySelectorAll(".method")].some((el) => !el.hidden);
  }
  $("count").textContent = needle
    ? `${shown} of ${METHODS.length} members`
    : `${METHODS.length} members · ${TYPES.length} sections`;
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
  const chips = Object.entries(registry).flatMap(([t, arr]) =>
    arr.map((n) => `<span class="obj" data-t="${t}" title="${t}">${n}</span>`));
  $("registry").innerHTML = chips.length ? chips.join("") : `<span class="empty">none yet</span>`;
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
renderChips();
renderMethods();
refreshObjrefs(); // render placeholders in the (empty) objref/receiver selects on load

$("filter").addEventListener("input", (e) => applyFilter(e.target.value));

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
  applyFilter($("filter").value);
  renderSession({ name: "", services: [] });
});

renderSession({ name: "", services: [] });
setInterval(loop, 250);
