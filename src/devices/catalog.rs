// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! The **device catalog**: named, ready-to-run Rhai device scripts.
//!
//! One registry, three consumers. The MCP `example` tool serves these to an
//! agent that has no checkout; a JSON scene file names one with
//! `"device": "hrm"` so the scene stays a topology instead of a wall of
//! embedded script; and the tests exercise every entry (lint +
//! `add_peripheral` + tick), so the samples and the engine cannot drift
//! apart.
//!
//! It lives here rather than in `mcp.rs` because a catalog is a property of
//! the device library, not of one agent-facing protocol — a scene loader that
//! had to depend on the MCP module to learn what `"hrm"` means would have the
//! dependency backwards.
//!
//! Each entry teaches a distinct idiom, and every script is self-contained:
//! copy it into the playground, `add_peripheral` it, or point a scene at it,
//! and it runs unchanged.

/// One catalog entry: the name a scene or `example` call refers to, a
/// one-line summary for listings, and the script itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceExample {
    /// The catalog name — what `"device": "..."` in a scene resolves against.
    pub name: &'static str,
    /// One-line description, shown when the catalog is listed.
    pub summary: &'static str,
    /// The Rhai source. Complete and runnable as-is.
    pub script: &'static str,
}

/// Every catalog entry, in teaching order (simplest idioms first within a
/// family). The order is stable: listings and the docs quote it.
pub const EXAMPLES: &[DeviceExample] = &[
    DeviceExample {
        name: "hrm",
        summary: "Heart-rate monitor (180D): named uuid consts, live values via fn tick",
        script: r#"// Heart Rate service with a measurement that changes over time.
let server = android::BluetoothGattServer("HRM");
let hrs = android::BluetoothGattService(uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY);
let hr = android::BluetoothGattCharacteristic(uuid::HEART_RATE_MEASUREMENT,
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
hr.set_value([0x00, 72]); // [flags, bpm]
hr.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
hrs.add_characteristic(hr);
server.add_service(hrs);

// Optional: runs on every scene tick; update_value pushes notifications
// to subscribed centrals.
fn tick(server, t) {
    let bpm = 68 + (t * 2.0).to_int() % 9;
    server.update_value(uuid::HEART_RATE_MEASUREMENT, [0x00, bpm]);
}
"#,
    },
    DeviceExample {
        name: "thermometer",
        summary: "Health Thermometer (1809): uuid::from_u16 for UUIDs with no named const",
        script: r#"// Health Thermometer service. No named const for these assigned
// numbers yet, so lift the 16-bit values with uuid::from_u16.
let server = android::BluetoothGattServer("Thermo");
let hts = android::BluetoothGattService(uuid::from_u16(0x1809), android::SERVICE_TYPE_PRIMARY);
let temp = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A1C),
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
temp.set_value([0x00, 37]); // [flags, degrees C] — byte 1 is what assert checks
temp.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
hts.add_characteristic(temp);
server.add_service(hts);

fn tick(server, t) {
    let c = 36 + t.to_int() % 3;
    server.update_value(uuid::from_u16(0x2A1C), [0x00, c]);
}
"#,
    },
    DeviceExample {
        name: "battery",
        summary: "Battery service (180F): the minimal static peripheral — no fn tick",
        script: r#"// Battery service: one static read-only value. The smallest
// complete peripheral.
let server = android::BluetoothGattServer("Batt");
let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
let level = android::BluetoothGattCharacteristic(uuid::BATTERY_LEVEL,
    android::PROPERTY_READ, android::PERMISSION_READ);
level.set_value([100]); // percent
bas.add_characteristic(level);
server.add_service(bas);
"#,
    },
    DeviceExample {
        name: "env_sensor",
        summary: "Environmental Sensing (181A): several characteristics on one service",
        script: r#"// Environmental Sensing: temperature (2A6E) and humidity (2A6F)
// on the same service.
let server = android::BluetoothGattServer("EnvSense");
let ess = android::BluetoothGattService(uuid::from_u16(0x181A), android::SERVICE_TYPE_PRIMARY);
let temp = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A6E),
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
temp.set_value([0x00, 21]); // [flags, degrees C]
temp.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
let hum = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A6F),
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
hum.set_value([0x00, 45]); // [flags, percent RH]
hum.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
ess.add_characteristic(temp);
ess.add_characteristic(hum);
server.add_service(ess);

fn tick(server, t) {
    server.update_value(uuid::from_u16(0x2A6E), [0x00, 20 + t.to_int() % 4]);
}
"#,
    },
    DeviceExample {
        name: "volume",
        summary: "LE Audio Volume Control (1844): a control point the phone writes to change state",
        script: r#"// Volume Control Service — the LE Audio profile a phone uses to set a
// speaker's volume. This is the control-point idiom: the peer WRITES a
// command opcode, and the device applies it and notifies the new state.
let server = android::BluetoothGattServer("Speaker");
let vcs = android::BluetoothGattService(uuid::VOLUME_CONTROL_SERVICE, android::SERVICE_TYPE_PRIMARY);

let state = android::BluetoothGattCharacteristic(uuid::VOLUME_STATE,
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
state.set_value([128, 0, 0]); // [volume 0-255, muted, change counter]
// A characteristic that declares NOTIFY needs a CCCD, or no real central
// can subscribe to it (Core Spec Vol 3, Part G, Section 3.3.3.3).
state.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
vcs.add_characteristic(state);

let point = android::BluetoothGattCharacteristic(uuid::VOLUME_CONTROL_POINT,
    android::PROPERTY_WRITE, android::PERMISSION_WRITE);
point.set_value([0xFF]); // 0xFF = no command pending
vcs.add_characteristic(point);

let flags = android::BluetoothGattCharacteristic(uuid::VOLUME_FLAGS,
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
flags.set_value([0x01]); // volume setting persisted
flags.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
vcs.add_characteristic(flags);
server.add_service(vcs);

// Opcodes (Volume Control Service 1.0, Table 3.3): 0x00 down, 0x01 up,
// 0x02/0x03 unmute+down/up, 0x04 set absolute, 0x05 unmute, 0x06 mute.
// A write is [opcode, change_counter] (+ volume for 0x04).
fn tick(server, t) {
    let command = server.value(uuid::VOLUME_CONTROL_POINT);
    if command.len() < 1 || command[0] == 0xFF { return; }
    let state = server.value(uuid::VOLUME_STATE);
    let volume = state[0];
    let muted = state[1];
    let op = command[0];
    if op == 0x00 || op == 0x02 { volume = if volume > 16 { volume - 16 } else { 0 }; }
    if op == 0x01 || op == 0x03 { volume = if volume < 239 { volume + 16 } else { 255 }; }
    if op == 0x02 || op == 0x03 || op == 0x05 { muted = 0; }
    if op == 0x04 && command.len() > 2 { volume = command[2]; }
    if op == 0x06 { muted = 1; }
    // The change counter increments on every state change, so a peer can
    // detect a command it raced against.
    server.update_value(uuid::VOLUME_STATE, [volume, muted, (state[2] + 1) % 256]);
    server.update_value(uuid::VOLUME_CONTROL_POINT, [0xFF]); // consumed
}
"#,
    },
    DeviceExample {
        name: "hid_keyboard",
        summary: "HID over GATT keyboard (1812): report map + input reports Android reads as a keyboard",
        script: r#"// HOGP keyboard. The Report Map (2A4B) is a USB HID report
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

// Types "hello" on a loop: press a key, then release it. A real keyboard
// sends the same two reports — a key is held until an empty report.
fn tick(server, t) {
    let keys = [0x0B, 0x08, 0x0F, 0x0F, 0x12]; // HID usage codes: h e l l o
    let step = (t * 2.0).to_int();
    let slot = (step / 2) % 5;
    let key = keys[slot];
    if step % 2 == 1 { key = 0; } // the release report
    server.update_value(uuid::from_u16(0x2A4D), [0, 0, key, 0, 0, 0, 0, 0]);
}
"#,
    },
    DeviceExample {
        name: "hid_mouse",
        summary: "HID over GATT mouse (1812): relative-motion reports, buttons + X/Y",
        script: r#"// HOGP mouse — same shape as the keyboard, different report map.
// The report is [buttons, dx, dy] with dx/dy as SIGNED relative motion,
// which is why the descriptor declares Logical Minimum -127.
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
    0x15, 0x81, 0x25, 0x7F, //   Logical -127..127
    0x75, 0x08, 0x95, 0x02,
    0x81, 0x06,       //     Input (Data,Var,Rel) — relative motion
    0xC0, 0xC0,       // End Collection x2
]);
hid.add_characteristic(map);

let report = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4D),
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
report.set_value([0, 0, 0]); // [buttons, dx, dy]
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

// Walks the pointer around a square: four headings, 3 seconds each.
fn tick(server, t) {
    let leg = (t / 3.0).to_int() % 4;
    let dx = 0;
    let dy = 0;
    if leg == 0 { dx = 5; }
    if leg == 1 { dy = 5; }
    if leg == 2 { dx = 251; } // -5 as a signed byte
    if leg == 3 { dy = 251; }
    server.update_value(uuid::from_u16(0x2A4D), [0, dx, dy]);
}
"#,
    },
    DeviceExample {
        name: "gamepad",
        summary: "HID over GATT game controller (1812): two analog axes + 8 buttons",
        script: r#"// HOGP game controller. Note most console pads (Xbox, DualSense)
// pair over CLASSIC HID, not this — but LE gamepads use exactly this
// profile, and the report map is what makes Android map the axes.
let server = android::BluetoothGattServer("SimGamepad");
let hid = android::BluetoothGattService(uuid::from_u16(0x1812), android::SERVICE_TYPE_PRIMARY);

let info = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4A),
    android::PROPERTY_READ, android::PERMISSION_READ);
info.set_value([0x11, 0x01, 0x00, 0x03]);
hid.add_characteristic(info);

let map = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4B),
    android::PROPERTY_READ, android::PERMISSION_READ);
map.set_value([
    0x05, 0x01,       // Usage Page (Generic Desktop)
    0x09, 0x05,       // Usage (Game Pad)
    0xA1, 0x01,       // Collection (Application)
    0x09, 0x01,       //   Usage (Pointer)
    0xA1, 0x00,       //   Collection (Physical)
    0x09, 0x30, 0x09, 0x31, //   Usage X, Y — the left stick
    0x15, 0x81, 0x25, 0x7F, //   Logical -127..127
    0x75, 0x08, 0x95, 0x02,
    0x81, 0x02,       //     Input (Data,Var,Abs) — absolute stick position
    0xC0,             //   End Collection
    0x05, 0x09,       //   Usage Page (Button)
    0x19, 0x01, 0x29, 0x08, // Buttons 1..8
    0x15, 0x00, 0x25, 0x01,
    0x75, 0x01, 0x95, 0x08,
    0x81, 0x02,       //   Input (Data,Var,Abs) — 8 button bits
    0xC0,             // End Collection
]);
hid.add_characteristic(map);

let report = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4D),
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
report.set_value([0, 0, 0]); // [x, y, buttons]
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

// Sweeps the stick and cycles one button at a time.
fn tick(server, t) {
    let step = t.to_int();
    let x = (step * 16) % 127;
    let y = (step * 8) % 127;
    let buttons = 1 << (step % 8);
    server.update_value(uuid::from_u16(0x2A4D), [x, y, buttons]);
}
"#,
    },
    DeviceExample {
        name: "cycling",
        summary: "Cycling Speed and Cadence (1816): cumulative counters a phone differentiates into speed",
        script: r#"// CSCS sensor. The measurement carries CUMULATIVE revolution counts
// plus the time of the last event (1/1024 s units) — the phone computes
// speed and cadence by differentiating between notifications, which is
// why a counter that only ever increases is the right model.
let server = android::BluetoothGattServer("CadenceSensor");
let cscs = android::BluetoothGattService(uuid::from_u16(0x1816), android::SERVICE_TYPE_PRIMARY);

// Flags bit0 = wheel data present, bit1 = crank data present.
let measurement = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A5B),
    android::PROPERTY_NOTIFY, android::PERMISSION_READ);
measurement.set_value([0x03, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
measurement.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
cscs.add_characteristic(measurement);

// CSC Feature: wheel + crank revolution data supported.
let feature = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A5C),
    android::PROPERTY_READ, android::PERMISSION_READ);
feature.set_value([0x03, 0x00]);
cscs.add_characteristic(feature);

// Sensor Location: 5 = left crank.
let location = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A5D),
    android::PROPERTY_READ, android::PERMISSION_READ);
location.set_value([0x05]);
cscs.add_characteristic(location);

// SC Control Point — this is where a phone resets the odometer.
let control = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A55),
    android::PROPERTY_WRITE | android::PROPERTY_INDICATE,
    android::PERMISSION_WRITE);
// CSCS 1.1, 3.3: the SC Control Point indicates its result, so it needs a
// CCCD for the client to enable those indications.
control.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
cscs.add_characteristic(control);
server.add_service(cscs);

// ~1 wheel rev/s (roughly 8 km/h on a 700c wheel) and 80 rpm cranks.
fn tick(server, t) {
    let seconds = t.to_int();
    let wheel = seconds;
    let crank = (seconds * 4) / 3;
    let event = (seconds * 1024) % 65536;
    let w0 = wheel & 0xFF;
    let w1 = (wheel >> 8) & 0xFF;
    let c0 = crank & 0xFF;
    let c1 = (crank >> 8) & 0xFF;
    let e0 = event & 0xFF;
    let e1 = (event >> 8) & 0xFF;
    server.update_value(uuid::from_u16(0x2A5B),
        [0x03, w0, w1, 0, 0, e0, e1, c0, c1, e0, e1]);
}
"#,
    },
    DeviceExample {
        name: "pulse_oximeter",
        summary: "Pulse Oximeter (1822): SpO2 + pulse rate as IEEE-11073 SFLOATs",
        script: r#"// PLXS continuous measurement. Values are SFLOATs: 16 bits split as a
// 4-bit signed exponent and a 12-bit signed mantissa. With exponent 0 the
// mantissa is just the integer, so 98% SpO2 is 0x0062 little-endian —
// which is why these look like plain numbers below.
let server = android::BluetoothGattServer("PulseOx");
let plxs = android::BluetoothGattService(uuid::from_u16(0x1822), android::SERVICE_TYPE_PRIMARY);

// Continuous Measurement: flags 0x00 (no extra fields) + SpO2 + pulse rate.
let continuous = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A5F),
    android::PROPERTY_NOTIFY, android::PERMISSION_READ);
continuous.set_value([0x00, 0x62, 0x00, 0x3E, 0x00]);
continuous.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
plxs.add_characteristic(continuous);

// Spot-check Measurement is indicated, not notified: it is a single
// reading the collector must acknowledge.
let spot = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A5E),
    android::PROPERTY_INDICATE, android::PERMISSION_READ);
spot.set_value([0x00, 0x62, 0x00, 0x3E, 0x00]);
spot.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
plxs.add_characteristic(spot);

let features = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A60),
    android::PROPERTY_READ, android::PERMISSION_READ);
features.set_value([0x00, 0x00]);
plxs.add_characteristic(features);
server.add_service(plxs);

// SpO2 drifts 96-99%, pulse 58-70 bpm.
fn tick(server, t) {
    let step = t.to_int();
    let spo2 = 96 + (step % 4);
    let pulse = 58 + (step % 13);
    server.update_value(uuid::from_u16(0x2A5F), [0x00, spo2, 0x00, pulse, 0x00]);
}
"#,
    },
    DeviceExample {
        name: "weight_scale",
        summary: "Smart scale: Weight Scale (181D) + Body Composition (181B) measurements",
        script: r#"// A smart scale exposes two services: Weight Scale for the raw mass
// and Body Composition for the derived numbers. Both measurements are
// INDICATED rather than notified — a weigh-in is a record the phone must
// acknowledge, not a stream it can miss.
let server = android::BluetoothGattServer("SmartScale");

let wss = android::BluetoothGattService(uuid::from_u16(0x181D), android::SERVICE_TYPE_PRIMARY);
// Flags 0x00 = SI units; weight is uint16 in 5 g steps, so 74.5 kg = 14900.
let weight = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A9D),
    android::PROPERTY_INDICATE, android::PERMISSION_READ);
weight.set_value([0x00, 0x34, 0x3A]);
weight.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
wss.add_characteristic(weight);

let wss_feature = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A9E),
    android::PROPERTY_READ, android::PERMISSION_READ);
wss_feature.set_value([0x00, 0x00, 0x00, 0x0C]); // 5 g mass resolution
wss.add_characteristic(wss_feature);
server.add_service(wss);

let bcs = android::BluetoothGattService(uuid::from_u16(0x181B), android::SERVICE_TYPE_PRIMARY);
// Flags 0x00 (SI) + body fat percentage in 0.1% steps: 182 = 18.2%.
let composition = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A9C),
    android::PROPERTY_INDICATE, android::PERMISSION_READ);
composition.set_value([0x00, 0x00, 0xB6, 0x00]);
composition.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
bcs.add_characteristic(composition);

let bcs_feature = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A9B),
    android::PROPERTY_READ, android::PERMISSION_READ);
bcs_feature.set_value([0x02, 0x00, 0x00, 0x00]); // body fat supported
bcs.add_characteristic(bcs_feature);
server.add_service(bcs);

// A step-on wobble that settles: real scales average a noisy load cell.
fn tick(server, t) {
    let wobble = (t * 3.0).to_int() % 7;
    let grams = 14900 + wobble * 5;
    let lo = grams & 0xFF;
    let hi = (grams >> 8) & 0xFF;
    server.update_value(uuid::from_u16(0x2A9D), [0x00, lo, hi]);
}
"#,
    },
    DeviceExample {
        name: "smart_lock",
        summary: "Smart lock: a custom control point that locks/unlocks, with state notifications",
        script: r#"// A BLE smart lock. No SIG profile covers locks, so real products use
// a vendor service — the shape is always the control-point idiom: the
// phone WRITES a command, the lock applies it and notifies the new state.
// Nothing here trusts the writer; a real lock authenticates first (see
// the pairing/bonding path) before honouring a command.
let server = android::BluetoothGattServer("SmartLock");
let svc = android::BluetoothGattService(
    uuid::of("d3a70001-1f8a-4b2c-9a11-000000000001"), android::SERVICE_TYPE_PRIMARY);

// 0 = unlocked, 1 = locked, 2 = jammed.
let state = android::BluetoothGattCharacteristic(
    uuid::of("d3a70002-1f8a-4b2c-9a11-000000000001"),
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
state.set_value([0x01]);
state.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
svc.add_characteristic(state);

// Commands: 0x01 lock, 0x02 unlock. 0xFF means "no command pending".
let control = android::BluetoothGattCharacteristic(
    uuid::of("d3a70003-1f8a-4b2c-9a11-000000000001"),
    android::PROPERTY_WRITE, android::PERMISSION_WRITE);
control.set_value([0xFF]);
svc.add_characteristic(control);

// Battery, because every lock's real failure mode is a dead battery.
let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
let level = android::BluetoothGattCharacteristic(uuid::BATTERY_LEVEL,
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
level.set_value([72]);
bas.add_characteristic(level);
server.add_service(bas);
server.add_service(svc);

fn tick(server, t) {
    let command = server.value(uuid::of("d3a70003-1f8a-4b2c-9a11-000000000001"));
    if command.len() < 1 || command[0] == 0xFF { return; }
    let op = command[0];
    if op == 0x01 {
        server.update_value(uuid::of("d3a70002-1f8a-4b2c-9a11-000000000001"), [0x01]);
    }
    if op == 0x02 {
        server.update_value(uuid::of("d3a70002-1f8a-4b2c-9a11-000000000001"), [0x00]);
    }
    // Consume the command so the next write is seen as new.
    server.update_value(uuid::of("d3a70003-1f8a-4b2c-9a11-000000000001"), [0xFF]);
}
"#,
    },
    DeviceExample {
        name: "fitness_tracker",
        summary: "Smartwatch / band: several services on one device (heart rate, battery, steps)",
        script: r#"// A wearable is not one profile — it is a handful of services on one
// GATT server, which is what makes it a useful shape to copy: standard
// services where they exist (Heart Rate, Battery, Device Information)
// and a vendor service for everything the SIG never standardised (steps).
let server = android::BluetoothGattServer("FitBand");

let hrs = android::BluetoothGattService(uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY);
let hr = android::BluetoothGattCharacteristic(uuid::HEART_RATE_MEASUREMENT,
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
hr.set_value([0x00, 64]);
hr.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
hrs.add_characteristic(hr);
let location = android::BluetoothGattCharacteristic(uuid::BODY_SENSOR_LOCATION,
    android::PROPERTY_READ, android::PERMISSION_READ);
location.set_value([0x02]); // wrist
hrs.add_characteristic(location);
server.add_service(hrs);

let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
let level = android::BluetoothGattCharacteristic(uuid::BATTERY_LEVEL,
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
level.set_value([84]);
bas.add_characteristic(level);
server.add_service(bas);

// Device Information — how a phone labels the device in its UI.
let dis = android::BluetoothGattService(uuid::from_u16(0x180A), android::SERVICE_TYPE_PRIMARY);
let manufacturer = android::BluetoothGattCharacteristic(uuid::MANUFACTURER_NAME,
    android::PROPERTY_READ, android::PERMISSION_READ);
manufacturer.set_value([0x53, 0x69, 0x6D, 0x42, 0x4C, 0x45]); // "SimBLE"
dis.add_characteristic(manufacturer);
let model = android::BluetoothGattCharacteristic(uuid::MODEL_NUMBER,
    android::PROPERTY_READ, android::PERMISSION_READ);
model.set_value([0x42, 0x61, 0x6E, 0x64, 0x20, 0x31]); // "Band 1"
dis.add_characteristic(model);
server.add_service(dis);

// Vendor step counter: a 32-bit cumulative count, like the cycling sensor.
let steps_svc = android::BluetoothGattService(
    uuid::of("f1e20001-8c3d-4a5b-9e6f-000000000001"), android::SERVICE_TYPE_PRIMARY);
let steps = android::BluetoothGattCharacteristic(
    uuid::of("f1e20002-8c3d-4a5b-9e6f-000000000001"),
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
steps.set_value([0, 0, 0, 0]);
steps.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
steps_svc.add_characteristic(steps);
server.add_service(steps_svc);

// Heart rate wanders with activity; steps accumulate about 2 per second.
fn tick(server, t) {
    let seconds = t.to_int();
    let bpm = 64 + (seconds * 3) % 40;
    server.update_value(uuid::HEART_RATE_MEASUREMENT, [0x00, bpm]);
    let count = seconds * 2;
    let b0 = count & 0xFF;
    let b1 = (count >> 8) & 0xFF;
    let b2 = (count >> 16) & 0xFF;
    server.update_value(uuid::of("f1e20002-8c3d-4a5b-9e6f-000000000001"), [b0, b1, b2, 0]);
}
"#,
    },
    DeviceExample {
        name: "eddystone",
        summary: "Eddystone-UID beacon (FEAA): Google's open beacon format, broadcast-only",
        script: r#"// Eddystone-UID: service data on 0xFEAA carrying a frame type, a
// calibrated TX power, and a 16-byte ID split into a 10-byte namespace
// (the operator) and a 6-byte instance (which beacon). Compare the
// fast_pair example — same advertising mechanism, different payload.
let server = android::BluetoothGattServer("Eddystone");
let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
let level = android::BluetoothGattCharacteristic(uuid::BATTERY_LEVEL,
    android::PROPERTY_READ, android::PERMISSION_READ);
level.set_value([95]);
bas.add_characteristic(level);
server.add_service(bas);

server.advertise_service_data(0xFEAA, [
    0x00,             // frame type: UID
    0xEB,             // ranging data: RSSI at 0 m, -21 dBm
    0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, // namespace
    0x00, 0x00, 0x00, 0x00, 0x00, 0x01, // instance
    0x00, 0x00,       // reserved
]);
server.advertise_connectable(false); // beacons broadcast, they do not connect
"#,
    },
    DeviceExample {
        name: "ranging",
        summary: "Channel Sounding responder (185B): distance estimates over the Ranging Service",
        script: r#"// Channel Sounding responder — a Bluetooth 6.0 ranging tag, the
// thing a phone measures its distance to (finder tags, car keys, "where
// did I leave it" trackers).
//
// The distance measurement itself is a CONTROLLER procedure: the two
// radios exchange tones and phase, and the host never sees the RF. What a
// phone talks to is this GATT service, which publishes the results — so
// this device models the reachable half, with `tick` standing in for the
// procedure's output.
let server = android::BluetoothGattServer("Ranger");
server.add_ras(); // Ranging Features, Real-Time Data, Control Point
server.advertise_service_uuid(0x185B);

// Real-Time Ranging Data is [f32 distance_metres, f32 confidence], little
// endian — the encoding RangingService::encode_ranging_data produces.
fn tick(server, t) {
    // A tag drifting slowly between 1 and 5 metres.
    let phase = (t / 4.0) % 2.0;
    let metres = if phase < 1.0 { 1.0 + phase * 4.0 } else { 5.0 - (phase - 1.0) * 4.0 };
    server.update_value(uuid::RANGING_REALTIME_DATA, f32_le(metres) + f32_le(0.87));
}
"#,
    },
    DeviceExample {
        name: "ranging_tag",
        summary: "Channel Sounding finder tag: ranging + battery, non-connectable until found",
        script: r#"// A finder tag (car key, luggage tag, "where are my keys"): the
// device a phone ranges to. Same Ranging Service as `ranging`, plus the
// battery every real tag exposes and a name a phone will show.
//
// Pair this with `ranging` to have two ranging devices on the air at once
// — a phone measuring distance to several tags is the actual use case.
let server = android::BluetoothGattServer("FinderTag");
server.add_ras();

let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
let level = android::BluetoothGattCharacteristic(uuid::BATTERY_LEVEL,
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
level.set_value([92]);
level.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
bas.add_characteristic(level);
server.add_service(bas);

server.advertise_service_uuid(0x185B);

fn tick(server, t) {
    // Held still, so the estimate jitters around 2.4 m the way a real
    // phase-based measurement does rather than sitting perfectly still.
    let jitter = ((t * 3.0).to_int() % 7).to_float() / 100.0;
    server.update_value(uuid::RANGING_REALTIME_DATA, f32_le(2.4 + jitter) + f32_le(0.91));
}
"#,
    },
    DeviceExample {
        name: "fast_pair",
        summary: "Fast Pair beacon (FE2C): custom advertising payload — service data + manufacturer data",
        script: r#"// Fast Pair beacon: what makes Android pop the pairing sheet. The
// identity lives in the ADVERTISEMENT, not the GATT — service data on
// the Fast Pair UUID (FE2C) carrying a 3-byte Model ID. The same
// advertise_* calls build any beacon (Eddystone, Quick Share nudge).
let server = android::BluetoothGattServer("FastPairBeacon");
let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
let level = android::BluetoothGattCharacteristic(uuid::BATTERY_LEVEL,
    android::PROPERTY_READ, android::PERMISSION_READ);
level.set_value([88]); // percent
bas.add_characteristic(level);
server.add_service(bas);

server.advertise_service_data(0xFE2C, [0x00, 0x11, 0x22]); // Model ID
server.advertise_manufacturer_data(0x00E0, [0x01]); // 0x00E0 = Google
server.advertise_connectable(false); // a real beacon is broadcast-only
"#,
    },
    DeviceExample {
        name: "thermostat",
        summary: "Settable device: custom 128-bit writable setpoint + convergence physics",
        script: r#"// Thermostat: the SIG has no thermostat service, so like real BLE
// thermostats this pairs standard Environmental Sensing temperature
// (read/notify) with a custom 128-bit writable setpoint. Set it from a
// central with the write tool; the room then drifts toward the target.
let server = android::BluetoothGattServer("Thermostat");

let ess = android::BluetoothGattService(uuid::from_u16(0x181A), android::SERVICE_TYPE_PRIMARY);
let temp = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A6E),
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
temp.set_value([0x00, 18]); // [flags, degrees C]
temp.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
ess.add_characteristic(temp);
server.add_service(ess);

let ctl = android::BluetoothGattService(uuid::of("5e7b0001-c0de-4a11-b1e5-0000c0ffee01"),
    android::SERVICE_TYPE_PRIMARY);
let setpoint = android::BluetoothGattCharacteristic(uuid::of("5e7b0002-c0de-4a11-b1e5-0000c0ffee01"),
    android::PROPERTY_READ | android::PROPERTY_WRITE,
    android::PERMISSION_READ | android::PERMISSION_WRITE);
setpoint.set_value([21]); // target degrees C — a central write replaces this
ctl.add_characteristic(setpoint);
server.add_service(ctl);

// fn tick keeps no variables between calls — the GATT database is the
// device's state. server.value(uuid) reads it back, central writes included.
fn tick(server, t) {
    let target = server.value(uuid::of("5e7b0002-c0de-4a11-b1e5-0000c0ffee01"))[0];
    let current = server.value(uuid::from_u16(0x2A6E))[1];
    if current < target { current += 1; }
    if current > target { current -= 1; }
    server.update_value(uuid::from_u16(0x2A6E), [0x00, current]);
}
"#,
    },
];

/// The script for catalog entry `name`, or `None` if there is no such entry.
pub fn script(name: &str) -> Option<&'static str> {
    EXAMPLES.iter().find(|e| e.name == name).map(|e| e.script)
}

/// Every catalog name, in catalog order — for error messages that tell a
/// caller what they *could* have asked for.
pub fn names() -> Vec<&'static str> {
    EXAMPLES.iter().map(|e| e.name).collect()
}

/// Every catalog name joined with ", ", the tail of an "unknown device" error.
pub fn names_joined() -> String {
    names().join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_catalog_name_is_unique_and_resolves_to_its_own_script() {
        let mut seen = std::collections::HashSet::new();
        for example in EXAMPLES {
            assert!(seen.insert(example.name), "duplicate name {}", example.name);
            assert_eq!(script(example.name), Some(example.script));
            assert!(
                !example.summary.is_empty(),
                "{} has no summary",
                example.name
            );
        }
        assert!(script("no-such-device").is_none());
    }
}
