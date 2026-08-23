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
        name: "le-audio-sink",
        summary: "LE Audio sink (PACS/ASCS/VCS): the control-point idiom, driven by the Audio page",
        // From the repository-root catalog/, not from web/: this script is
        // shared by the browser, the MCP `example` tool and the Rust tests, so
        // it does not live inside any one surface. The web server maps
        // /catalog/ to that directory rather than copying the file.
        script: include_str!("../../catalog/devices/le-audio-sink.rhai"),
    },
    DeviceExample {
        name: "auracast_source",
        summary: "Auracast broadcast source (android::BluetoothLeBroadcast): a BIG driven from a script",
        script: include_str!("../../catalog/devices/auracast_source.rhai"),
    },
    DeviceExample {
        name: "auracast_sink",
        summary: "Auracast earbud: the BASS Scan Delegator an Assistant adds a source to",
        script: include_str!("../../catalog/devices/auracast_sink.rhai"),
    },
    DeviceExample {
        name: "hrm",
        summary: "Heart-rate monitor (180D): named uuid consts, live values via fn tick",
        script: include_str!("../../catalog/devices/hrm.rhai"),
    },
    DeviceExample {
        name: "thermometer",
        summary: "Health Thermometer (1809): uuid::from_u16 for UUIDs with no named const",
        script: include_str!("../../catalog/devices/thermometer.rhai"),
    },
    DeviceExample {
        name: "battery",
        summary: "Battery service (180F): the minimal static peripheral — no fn tick",
        script: include_str!("../../catalog/devices/battery.rhai"),
    },
    DeviceExample {
        name: "env_sensor",
        summary: "Environmental Sensing (181A): several characteristics on one service",
        script: include_str!("../../catalog/devices/env_sensor.rhai"),
    },
    DeviceExample {
        name: "volume",
        summary: "LE Audio Volume Control (1844): a control point the phone writes to change state",
        script: include_str!("../../catalog/devices/volume.rhai"),
    },
    DeviceExample {
        name: "hid_keyboard",
        summary: "HID over GATT keyboard (1812): report map + input reports Android reads as a keyboard",
        script: include_str!("../../catalog/devices/hid_keyboard.rhai"),
    },
    DeviceExample {
        name: "hid_mouse",
        summary: "HID over GATT mouse (1812): relative-motion reports, buttons + X/Y",
        script: include_str!("../../catalog/devices/hid_mouse.rhai"),
    },
    DeviceExample {
        name: "gamepad",
        summary: "HID over GATT game controller (1812): two analog axes + 8 buttons",
        script: include_str!("../../catalog/devices/gamepad.rhai"),
    },
    DeviceExample {
        name: "cycling",
        summary: "Cycling Speed and Cadence (1816): cumulative counters a phone differentiates into speed",
        script: include_str!("../../catalog/devices/cycling.rhai"),
    },
    DeviceExample {
        name: "pulse_oximeter",
        summary: "Pulse Oximeter (1822): SpO2 + pulse rate as IEEE-11073 SFLOATs",
        script: include_str!("../../catalog/devices/pulse_oximeter.rhai"),
    },
    DeviceExample {
        name: "weight_scale",
        summary: "Smart scale: Weight Scale (181D) + Body Composition (181B) measurements",
        script: include_str!("../../catalog/devices/weight_scale.rhai"),
    },
    DeviceExample {
        name: "smart_lock",
        summary: "Smart lock: a custom control point that locks/unlocks, with state notifications",
        script: include_str!("../../catalog/devices/smart_lock.rhai"),
    },
    DeviceExample {
        name: "fitness_tracker",
        summary: "Smartwatch / band: several services on one device (heart rate, battery, steps)",
        script: include_str!("../../catalog/devices/fitness_tracker.rhai"),
    },
    DeviceExample {
        name: "eddystone",
        summary: "Eddystone-UID beacon (FEAA): Google's open beacon format, broadcast-only",
        script: include_str!("../../catalog/devices/eddystone.rhai"),
    },
    DeviceExample {
        name: "ranging",
        summary: "Channel Sounding responder (185B): distance estimates over the Ranging Service",
        script: include_str!("../../catalog/devices/ranging.rhai"),
    },
    DeviceExample {
        name: "ranging_tag",
        summary: "Channel Sounding finder tag: ranging + battery, non-connectable until found",
        script: include_str!("../../catalog/devices/ranging_tag.rhai"),
    },
    DeviceExample {
        name: "fast_pair",
        summary: "Fast Pair beacon (FE2C): custom advertising payload — service data + manufacturer data",
        script: include_str!("../../catalog/devices/fast_pair.rhai"),
    },
    DeviceExample {
        name: "color_bulb",
        summary: "Colour bulb: a writable [R, G, B] characteristic on a custom 128-bit service",
        script: include_str!("../../catalog/devices/color_bulb.rhai"),
    },
    DeviceExample {
        name: "thermostat",
        summary: "Settable device: custom 128-bit writable setpoint + convergence physics",
        script: include_str!("../../catalog/devices/thermostat.rhai"),
    },
    DeviceExample {
        name: "earbud",
        summary: "CSIP set member (1846): a SIRK-keyed Resolvable Set Identifier a coordinator resolves",
        script: include_str!("../../catalog/devices/earbud.rhai"),
    },
    DeviceExample {
        name: "hearing_aid",
        summary: "Hearing Access presets (1854): a control point that indicates its preset records",
        script: include_str!("../../catalog/devices/hearing_aid.rhai"),
    },
    DeviceExample {
        name: "media_player",
        summary: "Media Control (1848/1849): a phone's player, so a headset's buttons reach the music",
        script: include_str!("../../catalog/devices/media_player.rhai"),
    },
];

/// One **client** catalog entry: a script that *drives* a device instead of
/// being one (`android::BluetoothGatt`).
///
/// It carries `peer` — the peripheral entry it was written against — because
/// a client is only meaningful pointed at something. That is what lets a test
/// (or an agent) stand the pair up in one step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientExample {
    /// The catalog name.
    pub name: &'static str,
    /// One-line description, shown when the catalog is listed.
    pub summary: &'static str,
    /// The name of the [`EXAMPLES`] entry this client was written for.
    pub peer: &'static str,
    /// The Rhai source. Complete and runnable as-is.
    pub script: &'static str,
}

/// The address every client example connects to.
///
/// A script names its own target, which is right for the playground and for
/// a scene file where the topology is written down. A host that allocates
/// addresses itself (MCP, a page) overrides it with
/// `ScriptedCentral::set_target` — topology beats script — so this literal is
/// a default, not a requirement.
pub const EXAMPLE_PEER_ADDRESS: &str = "AA:BB:CC:00:00:01";

/// Client-side catalog entries: scripted GATT *centrals*.
pub const CENTRAL_EXAMPLES: &[ClientExample] = &[
    ClientExample {
        name: "hrm_client",
        summary: "Heart-rate client: connect, subscribe, assert the readings are plausible",
        peer: "hrm",
        script: r#"// The client half of GATT, in the same Android vocabulary as the
// server half: android::BluetoothGatt is Android's BluetoothGatt, and the
// on_* functions are its BluetoothGattCallback.
//
// This script is also a test: `assert` inside a callback fails the run.
let client = android::BluetoothGatt("HRM Client");
client.connect("AA:BB:CC:00:00:01");

// Discovery is what makes UUIDs usable — before it finishes the peer's
// handles are unknown, so subscribe from here rather than at top level.
fn on_services_discovered(client) {
    assert(client.has_characteristic(uuid::HEART_RATE_MEASUREMENT),
        "the peer is a heart-rate monitor");
    client.subscribe(uuid::HEART_RATE_MEASUREMENT);
}

fn on_characteristic_changed(client, uuid, value) {
    // [flags, bpm] — Heart Rate Measurement, Vol 3 Part G / the HRS spec.
    assert(value.len() >= 2, "measurement carries flags and a rate");
    assert(value[1] > 30 && value[1] < 220, "a plausible heart rate");
    client.emit("bpm", value[1]);
}
"#,
    },
    ClientExample {
        name: "bulb_client",
        summary: "Colour-bulb client: connect, read and subscribe, then write colours over GATT",
        peer: "color_bulb",
        script: r#"// The client half of the colour bulb, and the reason the page no longer
// needs to cheat: writing the colour used to be a host-side poke into the
// device's own database, because central-role scripting did not exist. Now
// the write crosses GATT like a phone app's would.
let client = android::BluetoothGatt("Bulb Client");
client.connect("AA:BB:CC:00:00:01");

fn on_services_discovered(client) {
    // Read once so the client shows the colour it found, then let the page
    // drive writes through it.
    client.read(uuid::of("f0ff0002-1234-5678-90ab-cdef01234567"));
    client.subscribe(uuid::of("f0ff0002-1234-5678-90ab-cdef01234567"));
}

fn on_characteristic_changed(client, uuid, value) {
    assert(value.len() == 3, "a colour is three bytes");
}
"#,
    },
    ClientExample {
        name: "thermostat_client",
        summary: "Write then watch: sets a setpoint, subscribes, asserts the room converges",
        peer: "thermostat",
        script: r#"// A client that changes the device and then checks the device changed:
// write the setpoint, subscribe to the temperature, and assert it moves
// toward the target. The `this` map is how a callback remembers anything
// — script functions are pure and cannot see the calling scope.
let client = android::BluetoothGatt("Thermostat Client");
client.connect("AA:BB:CC:00:00:01");

const SETPOINT = uuid::of("5e7b0002-c0de-4a11-b1e5-0000c0ffee01");
const TEMPERATURE = uuid::from_u16(0x2A6E);

fn on_services_discovered(client) {
    this.target = 26;
    this.seen = 0;
    client.write(SETPOINT, [this.target]);
    client.subscribe(TEMPERATURE);
}

fn on_characteristic_write(client, uuid, status) {
    assert(status == 0, "the setpoint was accepted");
}

fn on_characteristic_changed(client, uuid, value) {
    // [flags, degrees C].
    let degrees = value[1];
    this.seen += 1;
    client.emit("temperature", degrees);
    // The room starts at 18 and steps one degree per tick, so it must
    // never overshoot the target it was told to reach.
    assert(degrees <= this.target, "the room does not overshoot the setpoint");
}
"#,
    },
    ClientExample {
        name: "broadcast_assistant",
        summary: "Auracast Assistant (android::BluetoothLeBroadcastAssistant): Add Source, then watch the Receive State",
        peer: "auracast_sink",
        script: r#"// The phone in an Auracast handover, as Android's own profile proxy.
// Underneath it is pure GATT: a BASS Add Source written to the earbud's
// control point, and the Broadcast Receive State notified back.
//
// There is no connect() here, and Android has none either — the framework
// owns the link, and addSource reaches a sink whether or not one is up.
let assistant = android::BluetoothLeBroadcastAssistant("Phone");

// A BluetoothLeBroadcastMetadata: what a scanner found, or in this case what
// the catalog's auracast_source publishes. `broadcast.get_all_broadcast_metadata()`
// hands out exactly this shape.
const SOURCE = #{
    source_device: "AA:BB:CC:00:00:01",
    source_address_type: 0,
    source_advertising_sid: 0,
    broadcast_id: 0xC0FFEE,
    pa_sync_interval: 80,
    subgroups: [ #{ bis_sync: 0x03 } ],   // BIS 1 and 2
};

assistant.add_source("AA:BB:CC:00:00:01", SOURCE, false);
// Remote Scan Started tells the earbud a phone is scanning on its behalf.
// It names no sink, exactly as Android's startSearchingForSources does — so
// it has to follow a call that did, because that is where this Assistant
// learned which earbud it is talking to.
assistant.start_searching_for_sources();

fn on_source_added(assistant, sink, source_id, reason) {
    assistant.emit("source_id", source_id);
}

fn on_receive_state_changed(assistant, sink, source_id, state) {
    assert(state.broadcast_id == 0xC0FFEE, "the source the phone asked for");
    assistant.emit("pa_sync_state", state.pa_sync_state);
}
"#,
    },
    ClientExample {
        name: "gatt_walker",
        summary: "Discovery only: prints every service and characteristic the peer exposes",
        peer: "smart_lock",
        script: r#"// The smallest useful client: connect, discover, and report. Useful
// against an unknown device — and against a device you wrote, to check it
// publishes what you think it does.
let client = android::BluetoothGatt("Walker");
client.connect("AA:BB:CC:00:00:01");

fn on_services_discovered(client) {
    for service in client.services() {
        client.emit("service", service.to_string());
        for characteristic in client.characteristics(service) {
            client.emit("characteristic", characteristic.to_string());
        }
    }
    assert(client.services().len() > 0, "the peer published a GATT database");
}
"#,
    },
];

/// The script for catalog entry `name`, or `None` if there is no such entry.
/// Peripherals first, then clients: the two namespaces are disjoint.
pub fn script(name: &str) -> Option<&'static str> {
    EXAMPLES
        .iter()
        .find(|e| e.name == name)
        .map(|e| e.script)
        .or_else(|| {
            CENTRAL_EXAMPLES
                .iter()
                .find(|e| e.name == name)
                .map(|e| e.script)
        })
}

/// The client catalog entry called `name`, or `None`.
pub fn central(name: &str) -> Option<&'static ClientExample> {
    CENTRAL_EXAMPLES.iter().find(|e| e.name == name)
}

/// Every catalog name, in catalog order — for error messages that tell a
/// caller what they *could* have asked for.
pub fn names() -> Vec<&'static str> {
    EXAMPLES
        .iter()
        .map(|e| e.name)
        .chain(CENTRAL_EXAMPLES.iter().map(|e| e.name))
        .collect()
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

    #[test]
    fn every_client_example_names_a_peripheral_that_exists() {
        let mut seen = std::collections::HashSet::new();
        for example in CENTRAL_EXAMPLES {
            assert!(seen.insert(example.name), "duplicate name {}", example.name);
            assert!(
                EXAMPLES.iter().any(|p| p.name == example.peer),
                "{} names peer {:?}, which is not in the catalog",
                example.name,
                example.peer
            );
            assert!(
                example.script.contains(EXAMPLE_PEER_ADDRESS),
                "{} does not connect to the documented example address",
                example.name
            );
            assert_eq!(script(example.name), Some(example.script));
        }
        assert!(central("hrm").is_none(), "peripherals are not clients");
    }
}
