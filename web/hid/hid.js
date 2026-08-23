// Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
//
// HID over GATT, both halves in one page. Four devices share a single in-page
// `WebLink`: two HOGP peripherals (a keyboard and a mouse) and two centrals
// that connect to them and act as HID hosts.
//
// The module's job is deliberately small. It owns the *device* state — which
// keys are physically down, where the pointer has moved since the last frame —
// builds the input report that state implies, and writes it into the
// peripheral's GATT database. Everything after that is simble: the report is
// notified across the simulated radio, the central's `HidHost` reads the peer's
// Report Map, decides it is looking at a keyboard, subscribes, and decodes the
// notifications into events. The host side of the UI renders only those decoded
// events. It never reads the device state.
//
// Exported as a mountable domain module: `mount(root)` builds the whole UI into
// `root` and starts the one timer everything runs on; `unmount()` stops that
// timer, drops the wasm devices and removes every listener, so a shell can swap
// domains without leaving a device running.
//
// The two device scripts below are the `hid_keyboard` and `hid_mouse` examples
// the MCP `example` tool serves, minus their demo `tick` functions — those type
// "hello" and walk the pointer in a square, which would fight with the user.

import init, { WebLink } from "../pkg/simble.js";
import { createDeviceHeader } from "../common/device-header.js";
import { createAboutBox } from "../common/about-box.js";

// All four devices share one `WebLink`, and `WebLink` has `add_peripheral` and
// `add_central` and no way at all to remove either: freeing the link is the
// only teardown there is, and it takes every device with it. So none of these
// headers offers a working stop — they say so instead.
const SHARED_LINK = "four devices, one in-page link — they stop together";

// --- addresses --------------------------------------------------------------
const ADDR = {
  keyboard: "AA:BB:CC:00:00:01",
  mouse: "AA:BB:CC:00:00:02",
  kbdHost: "AA:BB:CC:00:00:11",
  mouseHost: "AA:BB:CC:00:00:12",
};

const REPORT_UUID = "2A4D";

// --- device scripts ---------------------------------------------------------
const KEYBOARD_SCRIPT = `// HOGP keyboard. The Report Map (2A4B) is a USB HID report
// descriptor: it tells the host how to interpret the bytes that arrive
// on the Report characteristic, so the same 8-byte report becomes
// keystrokes. A Report Reference descriptor (2908) tags each report with
// its ID and direction, which is how a host tells inputs from outputs.
let server = android::BluetoothGattServer("SimKeyboard");
let hid = android::BluetoothGattService(uuid::from_u16(0x1812), android::SERVICE_TYPE_PRIMARY);

// bcdHID 1.11, country 0 (not localized), flags: remote wake + normally connectable.
let info = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4A),
    android::PROPERTY_READ, android::PERMISSION_READ);
info.set_value([0x11, 0x01, 0x00, 0x03]);
hid.add_characteristic(info);

let map = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4B),
    android::PROPERTY_READ, android::PERMISSION_READ);
map.set_value([
    0x05, 0x01,       // Usage Page (Generic Desktop)
    0x09, 0x06,       // Usage (Keyboard)
    0xA1, 0x01,       // Collection (Application)
    0x05, 0x07,       //   Usage Page (Keyboard/Keypad)
    0x19, 0xE0, 0x29, 0xE7, // Usage Min/Max (modifier keys)
    0x15, 0x00, 0x25, 0x01, // Logical 0..1
    0x75, 0x01, 0x95, 0x08, // 8 x 1-bit
    0x81, 0x02,       //   Input (Data,Var,Abs) — modifier byte
    0x95, 0x01, 0x75, 0x08,
    0x81, 0x01,       //   Input (Const) — reserved byte
    0x95, 0x06, 0x75, 0x08, // 6 x 8-bit
    0x15, 0x00, 0x25, 0x65, // Logical 0..101
    0x05, 0x07, 0x19, 0x00, 0x29, 0x65,
    0x81, 0x00,       //   Input (Data,Array) — the 6 key slots
    0xC0,             // End Collection
]);
hid.add_characteristic(map);

// The input report: [modifiers, reserved, key1..key6].
let report = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4D),
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
report.set_value([0, 0, 0, 0, 0, 0, 0, 0]);
report.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
// Report Reference: report ID 1, type 1 (Input).
let reference = android::BluetoothGattDescriptor(uuid::from_u16(0x2908),
    android::PERMISSION_READ);
reference.set_value([0x01, 0x01]);
report.add_descriptor(reference);
hid.add_characteristic(report);

// Protocol Mode: 1 = Report (0 would be Boot).
let mode = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4E),
    android::PROPERTY_READ | android::PROPERTY_WRITE_NO_RESPONSE,
    android::PERMISSION_READ | android::PERMISSION_WRITE);
mode.set_value([0x01]);
hid.add_characteristic(mode);

// HID Control Point: the host writes 0x00 (suspend) / 0x01 (exit suspend).
let control = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4C),
    android::PROPERTY_WRITE_NO_RESPONSE, android::PERMISSION_WRITE);
hid.add_characteristic(control);
server.add_service(hid);
`;

const MOUSE_SCRIPT = `// HOGP mouse — same shape as the keyboard, different report map.
// The report is [buttons, dx, dy, wheel] with the three axes as SIGNED
// relative motion, which is why the descriptor declares Logical Minimum
// -127: read as unsigned, one step left becomes 255 steps right.
let server = android::BluetoothGattServer("SimMouse");
let hid = android::BluetoothGattService(uuid::from_u16(0x1812), android::SERVICE_TYPE_PRIMARY);

let info = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4A),
    android::PROPERTY_READ, android::PERMISSION_READ);
info.set_value([0x11, 0x01, 0x00, 0x03]);
hid.add_characteristic(info);

let map = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4B),
    android::PROPERTY_READ, android::PERMISSION_READ);
map.set_value([
    0x05, 0x01,       // Usage Page (Generic Desktop)
    0x09, 0x02,       // Usage (Mouse)
    0xA1, 0x01,       // Collection (Application)
    0x09, 0x01,       //   Usage (Pointer)
    0xA1, 0x00,       //   Collection (Physical)
    0x05, 0x09,       //     Usage Page (Button)
    0x19, 0x01, 0x29, 0x03, //   Buttons 1..3
    0x15, 0x00, 0x25, 0x01,
    0x95, 0x03, 0x75, 0x01,
    0x81, 0x02,       //     Input (Data,Var,Abs) — 3 button bits
    0x95, 0x01, 0x75, 0x05,
    0x81, 0x01,       //     Input (Const) — 5 bits padding
    0x05, 0x01,       //     Usage Page (Generic Desktop)
    0x09, 0x30, 0x09, 0x31, //   Usage X, Y
    0x09, 0x38,       //     Usage Wheel
    0x15, 0x81, 0x25, 0x7F, //   Logical -127..127
    0x75, 0x08, 0x95, 0x03,
    0x81, 0x06,       //     Input (Data,Var,Rel) — relative motion
    0xC0, 0xC0,       // End Collection x2
]);
hid.add_characteristic(map);

let report = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4D),
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
report.set_value([0, 0, 0, 0]); // [buttons, dx, dy, wheel]
report.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
let reference = android::BluetoothGattDescriptor(uuid::from_u16(0x2908),
    android::PERMISSION_READ);
reference.set_value([0x01, 0x01]);
report.add_descriptor(reference);
hid.add_characteristic(report);

let mode = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4E),
    android::PROPERTY_READ | android::PROPERTY_WRITE_NO_RESPONSE,
    android::PERMISSION_READ | android::PERMISSION_WRITE);
mode.set_value([0x01]);
hid.add_characteristic(mode);
server.add_service(hid);
`;

// --- the keyboard's key matrix ----------------------------------------------
// Each key carries the HID usage ID its position sends (Usage Tables 1.12,
// Keyboard/Keypad Page 0x07) and the browser `KeyboardEvent.code` for the same
// position, so the rendered keyboard and the typing box are driven by one
// table. Usages 0xE0-0xE7 are the modifiers: they live in the report's first
// byte as a bitmap, not in the six key slots.
const K = (code, label, usage, cls = "") => ({ code, label, usage, cls });
const LAYOUT = [
  [K("Backquote", "`", 0x35), K("Digit1", "1", 0x1e), K("Digit2", "2", 0x1f),
   K("Digit3", "3", 0x20), K("Digit4", "4", 0x21), K("Digit5", "5", 0x22),
   K("Digit6", "6", 0x23), K("Digit7", "7", 0x24), K("Digit8", "8", 0x25),
   K("Digit9", "9", 0x26), K("Digit0", "0", 0x27), K("Minus", "-", 0x2d),
   K("Equal", "=", 0x2e), K("Backspace", "⌫", 0x2a, "wide1")],
  [K("Tab", "tab", 0x2b, "wide1"), K("KeyQ", "q", 0x14), K("KeyW", "w", 0x1a),
   K("KeyE", "e", 0x08), K("KeyR", "r", 0x15), K("KeyT", "t", 0x17),
   K("KeyY", "y", 0x1c), K("KeyU", "u", 0x18), K("KeyI", "i", 0x0c),
   K("KeyO", "o", 0x12), K("KeyP", "p", 0x13), K("BracketLeft", "[", 0x2f),
   K("BracketRight", "]", 0x30), K("Backslash", "\\", 0x31)],
  [K("CapsLock", "caps", 0x39, "wide1 mod"), K("KeyA", "a", 0x04), K("KeyS", "s", 0x16),
   K("KeyD", "d", 0x07), K("KeyF", "f", 0x09), K("KeyG", "g", 0x0a),
   K("KeyH", "h", 0x0b), K("KeyJ", "j", 0x0d), K("KeyK", "k", 0x0e),
   K("KeyL", "l", 0x0f), K("Semicolon", ";", 0x33), K("Quote", "'", 0x34),
   K("Enter", "⏎", 0x28, "wide1")],
  [K("ShiftLeft", "shift", 0xe1, "wide2 mod"), K("KeyZ", "z", 0x1d), K("KeyX", "x", 0x1b),
   K("KeyC", "c", 0x06), K("KeyV", "v", 0x19), K("KeyB", "b", 0x05),
   K("KeyN", "n", 0x11), K("KeyM", "m", 0x10), K("Comma", ",", 0x36),
   K("Period", ".", 0x37), K("Slash", "/", 0x38),
   K("ShiftRight", "shift", 0xe5, "wide2 mod")],
  [K("ControlLeft", "ctrl", 0xe0, "wide1 mod"), K("AltLeft", "alt", 0xe2, "wide1 mod"),
   K("Space", "space", 0x2c, "space"), K("AltRight", "alt", 0xe6, "wide1 mod"),
   K("ControlRight", "ctrl", 0xe4, "wide1 mod")],
];
const BY_CODE = new Map();
for (const row of LAYOUT) for (const k of row) BY_CODE.set(k.code, k);

const isModifier = (usage) => usage >= 0xe0 && usage <= 0xe7;
const modifierBit = (usage) => 1 << (usage - 0xe0);

const escapeHtml = (s) => String(s).replace(/[&<>"']/g, (c) =>
  ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);

// ===========================================================================
//  Markup and styles
// ===========================================================================
const STYLE_ID = "simble-hid-style";
const STYLES = `
  .hid { display: grid; grid-template-columns: minmax(22rem, 1fr) minmax(22rem, 1fr);
    gap: 1.25rem; padding: 1.25rem 1.5rem; max-width: 82rem; margin: 0 auto; }
  @media (max-width: 62rem) { .hid { grid-template-columns: 1fr; } }
  .hid .wide { grid-column: 1 / -1; }
  .hid .typebox { width: 100%; font-size: 0.95rem; padding: 0.5rem 0.6rem;
    border: 1px solid var(--border); border-radius: 6px; background: var(--bg);
    color: var(--text); font-family: ui-monospace, Menlo, monospace; }
  .hid .typebox:focus { outline: 2px solid var(--accent); outline-offset: -1px; }
  .hid .typebox:disabled { opacity: 0.6; cursor: not-allowed; }

  .hid .kbd { display: flex; flex-direction: column; gap: 4px; margin: 0.7rem 0 0;
    user-select: none; }
  .hid .kbd .row { display: flex; gap: 4px; justify-content: center; }
  .hid .key { flex: 0 0 auto; min-width: 1.9rem; height: 1.9rem; padding: 0 0.35rem;
    display: flex; align-items: center; justify-content: center;
    border: 1px solid var(--border); border-radius: 5px; background: var(--panel2);
    font-size: 0.75rem; font-family: ui-monospace, Menlo, monospace; cursor: pointer;
    transition: background 0.06s, color 0.06s, border-color 0.06s; }
  .hid .key:hover { border-color: var(--dim); }
  .hid .key.wide1 { min-width: 3.1rem; } .hid .key.wide2 { min-width: 4.4rem; }
  .hid .key.space { flex: 1 1 auto; max-width: 14rem; }
  .hid .key.mod { color: var(--dim); }
  .hid .key.down { background: var(--accent); border-color: var(--accent); color: #fff; }
  .hid .key.mod.latched { background: var(--warn); border-color: var(--warn); color: #fff; }

  .hid .pad { margin-top: 0.5rem; }
  .hid .pad-surface { height: 8.5rem; border: 1px dashed var(--border); border-radius: 8px;
    background: var(--panel2); display: flex; align-items: center; justify-content: center;
    color: var(--dim); font-size: 0.8rem; cursor: crosshair; touch-action: none;
    user-select: none; text-align: center; padding: 0 1rem; }
  .hid .pad-surface.active { border-style: solid; border-color: var(--accent); }
  .hid .pad-buttons { display: flex; gap: 4px; margin-top: 4px; }
  .hid .pad-buttons button { flex: 1; font-size: 0.75rem; }
  .hid .pad-buttons button.down { background: var(--accent); border-color: var(--accent);
    color: #fff; }

  .hid .wire { margin-top: 0.8rem; padding: 0.5rem 0.65rem; border-radius: 6px;
    border: 1px solid var(--border); background: var(--bg);
    font-family: ui-monospace, Menlo, monospace; font-size: 0.78rem; }
  .hid .wire .lbl { color: var(--dim); }
  .hid .wire .bytes { letter-spacing: 0.06em; }
  .hid .wire .bytes b { color: var(--accent); font-weight: 600; }
  .hid .wire.pulse { border-color: var(--accent); }

  .hid .computer { display: flex; flex-direction: column; align-items: center; }
  .hid .monitor { width: 100%; max-width: 26rem; border: 2px solid var(--dim);
    border-radius: 10px; background: #24292f; padding: 8px; }
  .hid .screen { position: relative; height: 12.5rem; border-radius: 4px; overflow: hidden;
    background: #0d1117; color: #c9d1d9; font-family: ui-monospace, Menlo, monospace; }
  .hid .screen .titlebar { background: #161b22; color: #8b949e; font-size: 0.7rem;
    padding: 3px 8px; border-bottom: 1px solid #30363d; }
  .hid .screen .doc { padding: 0.5rem 0.6rem; font-size: 0.82rem; line-height: 1.5;
    white-space: pre-wrap; word-break: break-word; height: calc(100% - 1.4rem);
    overflow: hidden; }
  .hid .caret { display: inline-block; width: 0.5em; border-bottom: 2px solid #58a6ff;
    animation: simble-hid-blink 1s steps(1) infinite; }
  @keyframes simble-hid-blink { 50% { border-color: transparent; } }
  .hid .ptr { position: absolute; left: 0; top: 0; width: 14px; height: 20px;
    pointer-events: none; filter: drop-shadow(0 1px 1px rgba(0,0,0,0.6)); }
  .hid .ptr path { fill: #fff; stroke: #24292f; stroke-width: 1.2; }
  .hid .ptr.b1 path { fill: #58a6ff; } .hid .ptr.b2 path { fill: #f0883e; }
  .hid .ptr.b3 path { fill: #a5d6ff; }
  .hid .stand { width: 4.5rem; height: 0.6rem; background: var(--dim); }
  .hid .foot { width: 9rem; height: 0.45rem; border-radius: 3px; background: var(--dim); }

  .hid .hostkeys { display: flex; flex-wrap: wrap; gap: 4px; margin-top: 0.8rem;
    min-height: 1.9rem; align-items: center; }
  .hid .hostkeys .empty { color: var(--dim); font-size: 0.8rem; }
  .hid .chipkey { font-family: ui-monospace, Menlo, monospace; font-size: 0.75rem;
    padding: 0.2rem 0.45rem; border-radius: 5px; border: 1px solid var(--accent);
    background: var(--accent); color: #fff; }
  .hid .chipkey .u { opacity: 0.7; margin-left: 0.35rem; }
  .hid .btnlights { display: flex; gap: 0.5rem; margin-top: 0.5rem; align-items: center;
    font-size: 0.78rem; color: var(--dim); }
  .hid .lamp { display: inline-flex; align-items: center; gap: 0.3rem; }
  .hid .lamp i { width: 0.7rem; height: 0.7rem; border-radius: 50%;
    border: 1px solid var(--border); background: var(--panel2); font-style: normal; }
  .hid .lamp.on i { background: var(--accent); border-color: var(--accent); }

  .hid .decode { width: 100%; border-collapse: collapse; margin-top: 0.6rem;
    font-family: ui-monospace, Menlo, monospace; font-size: 0.76rem; }
  .hid .decode th { text-align: left; color: var(--dim); font-weight: 600;
    border-bottom: 1px solid var(--border); padding: 0.25rem 0.4rem; }
  .hid .decode td { padding: 0.22rem 0.4rem; border-bottom: 1px solid var(--panel2);
    vertical-align: top; }
  .hid .decode td.raw { white-space: nowrap; letter-spacing: 0.05em; }
  .hid .decode tr.fresh td { background: rgba(9, 105, 218, 0.08); }
  .hid .decode .none { color: var(--dim); }

  .hid .steps { list-style: none; padding: 0; margin: 0.5rem 0 0; font-size: 0.82rem;
    color: var(--dim); }
  .hid .steps li { display: flex; gap: 0.5rem; align-items: baseline; padding: 0.15rem 0; }
  .hid .steps li .tick { width: 1rem; color: var(--dim); }
  .hid .steps li.done { color: var(--text); }
  .hid .steps li.done .tick { color: var(--good); }

  .hid details.raw-map { margin-top: 0.7rem; }
  .hid details.raw-map pre { font-size: 0.72rem; white-space: pre-wrap; word-break: break-all;
    background: var(--bg); border: 1px solid var(--border); border-radius: 6px;
    padding: 0.5rem; margin: 0.4rem 0 0; }
  .hid .prose p { font-size: 0.88rem; line-height: 1.6; }
`;

const ABOUT = `<p>A keyboard and a mouse as HID-over-GATT peripherals, and hosts that connect, subscribe
   to their input reports and decode them. What each host shows is the report bytes interpreted
   the way a real computer would — held keys differenced against the previous report, relative
   motion read as signed — not the page reading the keyboard's variables.</p>`;

const TEMPLATE = `
<section class="panel wide prose">
  <h2 style="margin:0">What is on this page</h2>
  <p>
    Two GATT peripherals — a keyboard and a mouse — and two GATT centrals that connect to them,
    all four hosted in this tab on one simulated radio. Type on the left and the characters appear
    on the right, but nothing on the right reads a variable on the left. The device turns your
    keystroke into an <strong>HID input report</strong>, notifies it on the Report characteristic
    (<code>0x2A4D</code>), the report crosses the link as an ATT Handle Value Notification, and the
    host decodes it back into a character — which is exactly the work a real computer does.
  </p>
  <p class="hint">
    Everything runs on one timer in this page. A hidden tab is throttled by Chrome to about one
    tick a second, so keep this tab in front while you drive it.
  </p>
</section>

<section class="panel">

  <div id="hid-kbd-head"></div>
  <div id="hid-kbd-script"></div>
  <input id="hid-typebox" class="typebox" disabled
         placeholder="waiting for a host to connect…"
         autocomplete="off" autocorrect="off" autocapitalize="off" spellcheck="false">
  <div class="kbd" id="hid-kbd" aria-label="on-screen keyboard"></div>
  <div class="wire" id="hid-kbd-wire">
    <span class="lbl">notify 0x2A4D →</span> <span class="bytes" id="hid-kbd-bytes">—</span>
  </div>
  <p class="hint" style="margin-top:0.5rem">
    A keyboard report is eight bytes: a modifier bitmap, a reserved byte, and up to six
    <em>currently held</em> key usages. It is a snapshot, not an event — releasing a key means
    sending a report that no longer lists it. Hold seven keys at once and the device sends
    ErrorRollOver instead, because the matrix can no longer say which they are.
  </p>

  <div id="hid-mouse-head" style="margin-top:1.4rem"></div>
  <div id="hid-mouse-script"></div>
  <div class="pad">
    <div class="pad-surface" id="hid-pad">drag here to move the pointer · shift-drag to scroll</div>
    <div class="pad-buttons">
      <button data-btn="1">left</button>
      <button data-btn="2">right</button>
      <button data-btn="3">middle</button>
    </div>
  </div>
  <div class="wire" id="hid-mouse-wire">
    <span class="lbl">notify 0x2A4D →</span> <span class="bytes" id="hid-mouse-bytes">—</span>
  </div>
  <p class="hint" style="margin-top:0.5rem">
    A mouse report is <code>[buttons, dx, dy, wheel]</code>, and the three axes are
    <strong>signed relative</strong> motion. Read as unsigned, one step left (<code>FB</code>)
    becomes 251 steps right.
  </p>
</section>

<section class="panel">
  <p class="hint" style="margin-top:0">
    One computer, but two centrals: HOGP is a per-device connection, so the keyboard and the mouse
    each get their own, and each host reads its peer's Report Map before it knows what it is
    holding.
  </p>
  <div id="hid-kbdhost-head"></div>
  <div id="hid-mousehost-head"></div>

  <div class="computer">
    <div class="monitor">
      <div class="screen" id="hid-screen">
        <div class="titlebar">untitled.txt</div>
        <div class="doc"><span id="hid-typed"></span><span class="caret">&nbsp;</span></div>
        <svg class="ptr" id="hid-ptr" viewBox="0 0 14 20"><path d="M1 1 L1 15 L4.6 11.6 L7 17.6 L9.6 16.6 L7.2 10.8 L12 10.6 Z"/></svg>
      </div>
    </div>
    <div class="stand"></div>
    <div class="foot"></div>
  </div>

  <div class="hostkeys" id="hid-hostkeys"><span class="empty">no keys held</span></div>
  <div class="btnlights">
    buttons:
    <span class="lamp" id="hid-lamp1"><i></i>left</span>
    <span class="lamp" id="hid-lamp2"><i></i>right</span>
    <span class="lamp" id="hid-lamp3"><i></i>middle</span>
  </div>

  <h3 class="sub" style="margin-top:1rem">Reports received, and what they decoded to</h3>
  <table class="decode">
    <thead><tr><th style="width:1%">from</th><th style="width:1%">raw report</th><th>decoded</th></tr></thead>
    <tbody id="hid-decode-body">
      <tr><td colspan="3" class="none">nothing yet — type on the left</td></tr>
    </tbody>
  </table>

  <h3 class="sub" style="margin-top:1rem">How the host got here</h3>
  <ul class="steps" id="hid-steps">
    <li data-step="connected"><span class="tick">○</span><span>connected and discovered the peer's GATT</span></li>
    <li data-step="map"><span class="tick">○</span><span>read the Report Map <code>0x2A4B</code> — a USB HID report descriptor</span></li>
    <li data-step="identified"><span class="tick">○</span><span>identified the device from its first Application Collection</span></li>
    <li data-step="subscribed"><span class="tick">○</span><span>wrote the Report characteristic's CCCD to subscribe</span></li>
  </ul>
  <details class="raw-map">
    <summary>the Report Maps the host read</summary>
    <pre id="hid-map-kbd">keyboard: not read yet</pre>
    <pre id="hid-map-mouse">mouse: not read yet</pre>
  </details>
</section>

<section class="panel wide prose">
  <h2>Why the host has to read a descriptor first</h2>
  <p>
    HOGP does not tell a host what a device is. The advertisement carries the HID Service UUID
    <code>0x1812</code> and nothing more; both peripherals on this page advertise identically. What
    separates them is the <strong>Report Map</strong> (<code>0x2A4B</code>), a USB HID report
    descriptor copied verbatim into a GATT characteristic. Its first Application Collection names a
    Usage Page and a Usage — Generic Desktop / Keyboard (<code>0x01</code>, <code>0x06</code>) or
    Generic Desktop / Mouse (<code>0x01</code>, <code>0x02</code>) — and the rest of it says how
    wide each field of a report is. Until the host has parsed that, eight arriving bytes mean
    nothing: the same eight bytes are a keyboard's held keys under one descriptor and something
    else entirely under another.
  </p>
  <p>
    The other thing worth watching is <strong>rollover</strong>. Because a keyboard report lists the
    keys held right now, a host that treated each report as a keystroke would type a character
    again every time an unrelated key changed. Hold a letter down here and press a second one: the
    second report still carries the first key, and the decoded output still gains only one new
    character. That difference — set subtraction against the previous report — is the whole of what
    makes typing work, and getting it wrong is the classic HID host bug.
  </p>
  <p class="hint">
    Decoding lives in <code>src/devices/helpers/hid_reports.rs</code> (report formats and the
    Keyboard/Keypad usage page) and <code>src/device/hid_host.rs</code> (the transport-free HID
    Host: a discovered GATT view and notifications in, decoded events out). This module is a thin
    driver over <code>WebLink</code>, the in-page controller that hosts all four devices.
  </p>
  <div id="hid-error" class="error"></div>
</section>
`;

// ===========================================================================
//  Mounted state
// ===========================================================================
// Everything mutable lives here so `unmount` can drop it in one go. `null`
// means nothing is mounted, which is also how the tick loop knows to stop.
let S = null;
let wasmReady = null;

const $ = (id) => S.root.querySelector("#hid-" + id);

// ===========================================================================
//  Device-side state — what a real keyboard and mouse know about themselves
// ===========================================================================

// A keyboard report is the device's whole state, not an event. More than six
// non-modifier keys down is the rollover case: the matrix cannot say which, so
// the spec has it fill every slot with ErrorRollOver (0x01).
// The modifier byte is the union of what the browser says is physically held
// and what the on-screen keyboard has latched.
function modifierByte() {
  let bits = S.kbd.eventMods;
  for (const usage of S.kbd.latched) bits |= modifierBit(usage);
  return bits;
}

function keyboardReport() {
  const report = new Uint8Array(8);
  report[0] = modifierByte();
  const keys = [...S.kbd.held];
  if (keys.length > 6) report.fill(0x01, 2, 8);
  else keys.forEach((usage, i) => { report[2 + i] = usage; });
  return report;
}

const clamp127 = (v) => Math.max(-127, Math.min(127, Math.round(v)));
const toSigned = (v) => (v < 0 ? v + 256 : v);

function mouseReport() {
  const m = S.mouse;
  return new Uint8Array([
    m.buttons, toSigned(clamp127(m.dx)), toSigned(clamp127(m.dy)), toSigned(clamp127(m.wheel)),
  ]);
}

// One report per device per frame: a notification is raised by the *change* to
// the characteristic, so two writes between ticks would collapse into one and a
// keystroke would vanish. Queueing preserves every report in order.
function queueKeyboardReport() { S.queue.keyboard.push(keyboardReport()); }

// Motion may coalesce — a real mouse reports the sum of what happened since its
// last report — but a button edge may not. A click that goes down and back up
// inside one frame would otherwise be reported as nothing happening, so a
// button change snapshots the report immediately.
function queueMouseReport() {
  S.queue.mouse.push(mouseReport());
  S.mouse.dx = 0; S.mouse.dy = 0; S.mouse.wheel = 0; S.mouse.dirty = false;
}

// ===========================================================================
//  Host-side view — rendered only from decoded HID events
// ===========================================================================

// Turn one decoded event into (a) its effect on the mock computer and (b) a
// human sentence for the report table.
function applyEvent(event) {
  const host = S.host;
  const hex2 = (n) => "0x" + n.toString(16).padStart(2, "0");
  switch (event.type) {
    case "identified":
      host.steps.map = true;
      host.steps.identified = true;
      return `Report Map parsed → this peer is a ${event.kind}`;
    case "key_down": {
      const c = event.character;
      const name = c != null
        ? (c === "\n" ? "Enter" : c === "\t" ? "Tab" : c === " " ? "space" : c)
        : (event.label || `usage ${hex2(event.usage)}`);
      host.heldKeys.set(event.usage, name);
      if (c === "\n") host.text += "\n";
      else if (c === "\t") host.text += "    ";
      else if (c != null) host.text += c;
      else if (event.usage === 0x2a) host.text = host.text.slice(0, -1); // Backspace
      const mods = event.modifiers ? ` (modifiers ${hex2(event.modifiers)})` : "";
      const shown = c != null ? JSON.stringify(c) : event.label || "no character";
      return `key down: usage ${hex2(event.usage)} → ${shown}${mods}`;
    }
    case "key_up":
      host.heldKeys.delete(event.usage);
      return `key up: usage ${hex2(event.usage)}`;
    case "roll_over":
      return "rollover — more keys held than the matrix can name; report ignored";
    case "pointer": {
      // The report is relative, so the host integrates it. The screen edge is
      // the clamp; on a real host it is the edge of the desktop.
      host.x = Math.max(0, Math.min(1, host.x + event.dx / 260));
      host.y = Math.max(0, Math.min(1, host.y + event.dy / 200));
      return `pointer: dx ${event.dx}, dy ${event.dy}` + (event.wheel ? `, wheel ${event.wheel}` : "");
    }
    case "button_down":
      host.buttons.add(event.button);
      return `button ${event.button} down`;
    case "button_up":
      host.buttons.delete(event.button);
      return `button ${event.button} up`;
    default:
      return event.type;
  }
}

function pollHost(role, source) {
  const idx = S.index[role];
  if (!S.hidStarted[role]) {
    S.hidStarted[role] = S.link.central_start_hid(idx);
    if (S.hidStarted[role]) { S.host.steps.connected = true; S.host.steps.subscribed = true; }
  }
  const view = JSON.parse(S.link.central_hid_events_json(idx));
  if (view.report_map) {
    S.host.steps.map = true;
    const pre = $(role === "kbdHost" ? "map-kbd" : "map-mouse");
    pre.textContent = `${source} (${view.kind}, ${view.report_map.length / 2} bytes)\n` +
      view.report_map.match(/../g).join(" ");
  }
  for (const event of view.events || []) {
    // Every event is stamped with the exact bytes it was decoded from.
    S.host.rows.unshift({ source, raw: view.report || "", decoded: applyEvent(event), fresh: true });
  }
  if (S.host.rows.length > 8) S.host.rows.length = 8;
}

function renderHost() {
  const host = S.host;
  $("typed").textContent = host.text.length > 400 ? host.text.slice(-400) : host.text;

  const keys = $("hostkeys");
  keys.innerHTML = host.heldKeys.size
    ? [...host.heldKeys.entries()].map(([usage, name]) =>
        `<span class="chipkey">${escapeHtml(name)}<span class="u">0x${
          usage.toString(16).padStart(2, "0").toUpperCase()}</span></span>`).join("")
    : '<span class="empty">no keys held</span>';
  for (const b of [1, 2, 3]) $("lamp" + b).classList.toggle("on", host.buttons.has(b));

  const screen = $("screen");
  const ptr = $("ptr");
  ptr.style.left = (host.x * (screen.clientWidth - 14)) + "px";
  ptr.style.top = (host.y * (screen.clientHeight - 20)) + "px";
  // An SVG element's `className` is a read-only SVGAnimatedString.
  ptr.setAttribute("class", "ptr" + (host.buttons.size ? " b" + Math.min(...host.buttons) : ""));

  const body = $("decode-body");
  if (!host.rows.length) {
    body.innerHTML = '<tr><td colspan="3" class="none">nothing yet — type on the left</td></tr>';
  } else {
    body.innerHTML = host.rows.map((r, i) =>
      `<tr class="${i === 0 && r.fresh ? "fresh" : ""}"><td>${escapeHtml(r.source)}</td>` +
      `<td class="raw">${escapeHtml(r.raw)}</td><td>${escapeHtml(r.decoded)}</td></tr>`).join("");
    host.rows.forEach((r) => { r.fresh = false; });
  }

  for (const li of $("steps").querySelectorAll("li")) {
    const done = !!host.steps[li.dataset.step];
    li.classList.toggle("done", done);
    li.querySelector(".tick").textContent = done ? "●" : "○";
  }
}

// ===========================================================================
//  Device-side rendering
// ===========================================================================
function renderKeyboard() {
  $("kbd").innerHTML = LAYOUT.map((row) =>
    `<div class="row">${row.map((k) =>
      `<div class="key ${k.cls}" data-usage="${k.usage}">${escapeHtml(k.label)}</div>`
    ).join("")}</div>`).join("");
}

function refreshKeyStyles() {
  for (const el of $("kbd").querySelectorAll(".key")) {
    const usage = Number(el.dataset.usage);
    const down = isModifier(usage)
      ? (modifierByte() & modifierBit(usage)) !== 0
      : S.kbd.held.has(usage);
    el.classList.toggle("down", down && !S.kbd.latched.has(usage));
    el.classList.toggle("latched", S.kbd.latched.has(usage));
  }
}

function showSent(which, bytes) {
  // Highlight the bytes that carry meaning; zeroes stay quiet.
  $(which + "-bytes").innerHTML = [...bytes]
    .map((b) => { const h = b.toString(16).padStart(2, "0").toUpperCase(); return b ? `<b>${h}</b>` : h; })
    .join(" ");
  const wire = $(which + "-wire");
  wire.classList.add("pulse");
  S.timers.add(setTimeout(() => wire.classList.remove("pulse"), 140));
}

// ===========================================================================
//  Device input
// ===========================================================================
function pressUsage(usage) {
  if (!isModifier(usage)) S.kbd.held.add(usage);
  queueKeyboardReport();
}

function releaseUsage(usage) {
  S.kbd.held.delete(usage);
  queueKeyboardReport();
}

function releaseEverything() {
  if (!S || (!S.kbd.held.size && !modifierByte())) return;
  S.kbd.held.clear();
  S.kbd.eventMods = 0;
  S.kbd.latched.clear();
  queueKeyboardReport();
}

/// The modifier bitmap a key event implies.
///
/// The browser reports modifier *state* rather than which key holds it, so the
/// left-hand bits stand in for both sides. That is enough: a host reads the
/// bitmap, and Left Shift and Right Shift mean the same thing to it. The
/// on-screen keyboard, which does know the side, latches the exact usage.
function eventModifiers(e) {
  return (e.shiftKey ? 0x02 : 0) | (e.ctrlKey ? 0x01 : 0)
    | (e.altKey ? 0x04 : 0) | (e.metaKey ? 0x08 : 0);
}

// The typing box is never allowed to hold text: it is a key-event source, and
// the only place characters accumulate is the host's screen.
function wireTypeBox(signal) {
  const box = $("typebox");
  box.addEventListener("keydown", (e) => {
    e.preventDefault();
    // Auto-repeat is the host's job, not the device's: a real keyboard sends
    // one report when the key goes down and nothing more until it changes.
    if (e.repeat) return;
    S.kbd.eventMods = eventModifiers(e);
    const key = BY_CODE.get(e.code);
    // A modifier going down changes the report even with no key in it.
    if (key) pressUsage(key.usage); else queueKeyboardReport();
  }, { signal });
  box.addEventListener("keyup", (e) => {
    e.preventDefault();
    S.kbd.eventMods = eventModifiers(e);
    const key = BY_CODE.get(e.code);
    if (key) releaseUsage(key.usage); else queueKeyboardReport();
  }, { signal });
  // Losing focus with keys held would strand them down forever.
  box.addEventListener("blur", releaseEverything, { signal });
}

// Clicking an on-screen key presses and releases it. Modifiers latch instead,
// so Shift + a letter is reachable with one pointer.
function wireOnScreenKeyboard(signal) {
  $("kbd").addEventListener("pointerdown", (e) => {
    const el = e.target.closest(".key");
    if (!el) return;
    e.preventDefault();
    const usage = Number(el.dataset.usage);
    if (isModifier(usage)) {
      if (S.kbd.latched.has(usage)) { S.kbd.latched.delete(usage); releaseUsage(usage); }
      else { S.kbd.latched.add(usage); pressUsage(usage); }
      return;
    }
    pressUsage(usage);
    const up = () => {
      releaseUsage(usage);
      // A latched modifier is consumed by the keystroke, as a sticky-keys
      // implementation would.
      for (const mod of [...S.kbd.latched]) { S.kbd.latched.delete(mod); releaseUsage(mod); }
      window.removeEventListener("pointerup", up);
    };
    window.addEventListener("pointerup", up, { signal });
  }, { signal });
}

function wireTrackpad(signal) {
  const pad = $("pad");
  let dragging = false;
  let last = null;
  pad.addEventListener("pointerdown", (e) => {
    dragging = true;
    last = { x: e.clientX, y: e.clientY };
    pad.setPointerCapture(e.pointerId);
    pad.classList.add("active");
  }, { signal });
  pad.addEventListener("pointermove", (e) => {
    if (!dragging) return;
    const dx = e.clientX - last.x;
    const dy = e.clientY - last.y;
    last = { x: e.clientX, y: e.clientY };
    // Shift turns the drag into wheel detents — the same report's third axis.
    if (e.shiftKey) S.mouse.wheel += -dy / 6;
    else { S.mouse.dx += dx; S.mouse.dy += dy; }
    S.mouse.dirty = true;
  }, { signal });
  const stop = (e) => {
    if (!dragging) return;
    dragging = false;
    pad.classList.remove("active");
    try { pad.releasePointerCapture(e.pointerId); } catch (_) { /* already gone */ }
  };
  pad.addEventListener("pointerup", stop, { signal });
  pad.addEventListener("pointercancel", stop, { signal });

  for (const button of S.root.querySelectorAll(".pad-buttons button")) {
    const bit = 1 << (Number(button.dataset.btn) - 1);
    const set = (held) => {
      if (((S.mouse.buttons & bit) !== 0) === held) return;
      if (held) S.mouse.buttons |= bit; else S.mouse.buttons &= ~bit;
      button.classList.toggle("down", held);
      queueMouseReport();
    };
    button.addEventListener("pointerdown", (e) => { e.preventDefault(); set(true); }, { signal });
    button.addEventListener("pointerup", () => set(false), { signal });
    button.addEventListener("pointerleave", () => set(false), { signal });
  }
}

// ===========================================================================
//  Tick loop — one timer for all four devices
// ===========================================================================
function sendQueued() {
  const kbdBytes = S.queue.keyboard.shift();
  if (kbdBytes) {
    // notify_value rather than set_value: two identical reports in a row are
    // meaningful here (the same key pressed twice, the same mouse step
    // repeated), and a value-diff would swallow the second.
    S.link.peripheral_notify_value(S.index.keyboard, REPORT_UUID, kbdBytes);
    showSent("kbd", kbdBytes);
  }
  let mouseBytes = S.queue.mouse.shift();
  if (!mouseBytes && S.mouse.dirty) {
    mouseBytes = mouseReport();
    S.mouse.dx = 0; S.mouse.dy = 0; S.mouse.wheel = 0; S.mouse.dirty = false;
  }
  if (mouseBytes) {
    S.link.peripheral_notify_value(S.index.mouse, REPORT_UUID, mouseBytes);
    showSent("mouse", mouseBytes);
  }
}

function loop() {
  if (!S) return;                      // unmounted between frames
  S.raf = requestAnimationFrame(loop);
  try {
    sendQueued();
    S.link.tick((performance.now() - S.t0) / 1000);
    pollHost("kbdHost", "keyboard");
    pollHost("mouseHost", "mouse");
  } catch (e) {
    console.error("hid tick:", e);
    $("error").textContent = String(e);
    cancelAnimationFrame(S.raf);
    S.raf = null;
    for (const head of Object.values(S.heads)) head.setState(false, "stopped — see the error below", "bad");
    return;
  }
  refreshKeyStyles();
  renderHost();

  // A HOGP peripheral's dot is lit when a host has actually subscribed to its
  // Report characteristic: until then it is advertising into an empty room and
  // every keystroke goes nowhere, which is the failure this page most often
  // has to explain.
  const kbdReady = S.hidStarted.kbdHost;
  const mouseReady = S.hidStarted.mouseHost;
  S.heads.keyboard.setState(kbdReady, kbdReady ? "a host is subscribed" : "advertising…",
    kbdReady ? "ok" : "");
  S.heads.mouse.setState(mouseReady, mouseReady ? "a host is subscribed" : "advertising…",
    mouseReady ? "ok" : "");
  S.heads.kbdHost.setState(kbdReady, kbdReady ? "subscribed to its peer's reports" : "connecting…",
    kbdReady ? "ok" : "");
  S.heads.mouseHost.setState(mouseReady, mouseReady ? "subscribed to its peer's reports" : "connecting…",
    mouseReady ? "ok" : "");
  // Typing before the host has subscribed goes nowhere, which looks like a
  // broken page rather than a keyboard talking to no one.
  const box = $("typebox");
  if (box.disabled === kbdReady) {
    box.disabled = !kbdReady;
    box.placeholder = kbdReady
      ? "click here and type — each keystroke becomes an input report"
      : "waiting for a host to connect…";
    if (kbdReady) box.focus();
  }
}

// ===========================================================================
//  Mount / unmount
// ===========================================================================

/// The four device headers. The two peripherals show their scripts read-only:
/// the Report Map this page spends its prose on is *in* those scripts, and it
/// is worth being able to look at. They are not editable here because editing
/// one would mean rebuilding the link and every device on it, which is the
/// Playground's job. The two hosts have no script at all -- a `HidHost` is
/// Rust, and SimBLE has no central-role scripting -- so they get no pen.
function buildHeaders() {
  const peripheral = (key, name, source, target) => {
    const head = createDeviceHeader({
      name,
      kind: "peripheral · HOGP",
      accent: "good",
      address: ADDR[key],
      dotMeans: "a host has subscribed to its Report characteristic",
      script: {
        text: source,
        editable: false,
        note: "<strong>Read-only here.</strong> This is the <code>hid_" +
          (key === "keyboard" ? "keyboard" : "mouse") +
          "</code> example MCP's <code>example</code> tool serves, minus its demo " +
          "<code>tick</code> — that one types on its own and would fight with you. The " +
          "<code>0x2A4B</code> Report Map in it is the descriptor the host parses.",
      },
      run: { running: true, disabled: true, reason: SHARED_LINK },
    });
    $(target + "-head").append(head.el);
    $(target + "-script").append(head.panel);
    head.setState(false, "starting…");
    S.heads[key] = head;
  };
  peripheral("keyboard", "SimKeyboard", KEYBOARD_SCRIPT, "kbd");
  peripheral("mouse", "SimMouse", MOUSE_SCRIPT, "mouse");

  const host = (key, name, peer, target) => {
    const head = createDeviceHeader({
      name,
      kind: `central · HidHost (Rust) → ${peer}`,
      accent: "accent",
      address: ADDR[key],
      dotMeans: "it has read the peer's Report Map and subscribed to its reports",
      run: { running: true, disabled: true, reason: SHARED_LINK },
    });
    $(target).append(head.el);
    head.setState(false, "starting…");
    S.heads[key] = head;
  };
  host("kbdHost", "Keyboard Host", "SimKeyboard", "kbdhost-head");
  host("mouseHost", "Mouse Host", "SimMouse", "mousehost-head");
}

function injectStyles() {
  if (document.getElementById(STYLE_ID)) return;
  const style = document.createElement("style");
  style.id = STYLE_ID;
  style.textContent = STYLES;
  document.head.appendChild(style);
}

/**
 * Build the HID demo into `root` and start it. Safe to call again after
 * `unmount`; calling it twice without unmounting replaces the first mount.
 * @param {HTMLElement} root container to build into.
 */
export async function mount(root) {
  unmount();
  injectStyles();
  wasmReady ??= init();
  await wasmReady;

  root.classList.add("hid");
  root.innerHTML = TEMPLATE;
  root.prepend(createAboutBox(ABOUT));

  const link = new WebLink();
  S = {
    root,
    link,
    index: {
      keyboard: link.add_peripheral(ADDR.keyboard, KEYBOARD_SCRIPT),
      mouse: link.add_peripheral(ADDR.mouse, MOUSE_SCRIPT),
      kbdHost: link.add_central(ADDR.kbdHost, ADDR.keyboard),
      mouseHost: link.add_central(ADDR.mouseHost, ADDR.mouse),
    },
    hidStarted: { kbdHost: false, mouseHost: false },
    kbd: { held: new Set(), eventMods: 0, latched: new Set() },
    mouse: { buttons: 0, dx: 0, dy: 0, wheel: 0, dirty: false },
    queue: { keyboard: [], mouse: [] },
    host: {
      text: "",
      heldKeys: new Map(),   // usage -> the label shown while it is down
      buttons: new Set(),
      x: 0.5, y: 0.5,        // pointer position as a fraction of the screen
      rows: [],              // recent (source, raw bytes, decoded) triples
      steps: { connected: false, map: false, identified: false, subscribed: false },
    },
    listeners: new AbortController(),
    timers: new Set(),
    heads: {},
    t0: performance.now(),
    raf: null,
  };

  buildHeaders();
  renderKeyboard();

  const signal = S.listeners.signal;
  wireTypeBox(signal);
  wireOnScreenKeyboard(signal);
  wireTrackpad(signal);
  // A hidden tab is throttled hard enough to stall the protocol; at least do
  // not leave a key stuck down across the gap.
  document.addEventListener("visibilitychange", () => {
    if (document.hidden) releaseEverything();
  }, { signal });

  // A handle for the console: `simbleHid.link.central_status_json(…)` shows
  // what the host has discovered, the first thing to look at if the page sits
  // in "connecting".
  window.simbleHid = S;

  S.raf = requestAnimationFrame(loop);
}

/**
 * Stop everything this module started: the tick loop, the wasm devices and
 * every listener. Idempotent, and safe to call when nothing is mounted.
 */
export function unmount() {
  if (!S) return;
  if (S.raf !== null) cancelAnimationFrame(S.raf);
  S.listeners.abort();
  for (const timer of S.timers) clearTimeout(timer);
  for (const head of Object.values(S.heads)) head.destroy();
  // Dropping the WebLink drops all four devices with it; leaving it alive
  // would keep a scene running with nothing rendering it.
  try { S.link.free(); } catch (_) { /* already freed */ }
  S.root.classList.remove("hid");
  S.root.innerHTML = "";
  if (window.simbleHid === S) delete window.simbleHid;
  S = null;
}
