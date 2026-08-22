// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Browser (wasm32) transport and demo engines for the GitHub Pages demos in
//! `web/`: Simble compiled to WebAssembly, talking to the visitor's local
//! netsimd over the browser's native `WebSocket` (the same
//! `ws://localhost:7681/v1/websocket/bt?name=<n>&address=<mac>` endpoint as
//! `netsim`, but with the browser doing all RFC 6455 framing).
//!
//! Split in two so the demo logic stays natively testable:
//! - The pure-Rust engines in this module body ([`queue_scanner_start`],
//!   [`parse_scan_reports`], [`ScriptedPeripheral`]) compile and unit-test on
//!   every target — no browser required.
//! - The `web` submodule (gated `#[cfg(target_arch = "wasm32")]`) wraps
//!   `web_sys::WebSocket` and exports the page-facing wasm-bindgen types.
//!   Browser pages drive everything from a JS interval calling `tick()` —
//!   there are no blocking loops, because wasm shares the page's event loop.

use std::collections::HashMap;

use crate::android::gatt_service::BluetoothGattCharacteristic;
use crate::client::gatt_client::GattClient;
use crate::controller::sim::Link;
use crate::gap::{AdvertisingData, ad_type, flags};
/// The default script served by the scripted-device page — a single source of
/// truth shared by the page (via `default_heart_rate_script`) and the native
/// unit tests below, so what ships is what's tested. (The file keeps its
/// legacy `heart_rate.rhai` name but now builds a thermometer; the export name
/// is likewise kept for the page's stable wasm import.)
pub const DEFAULT_HEART_RATE_SCRIPT: &str = include_str!("../../web/hrm/heart_rate.rhai");

use crate::device::LeHost;
use crate::packets::hci_events::{
    HciEvent, advertising_reports, event_code as hci_event_code, le_subevent,
};
// Advertising payload builders live with `AdvertisingData` in `gap`; kept in
// this module's public surface for the browser bindings and existing callers.
pub use crate::gap::advertising::{build_adv_payload, build_adv_payload_with_extras};
use crate::l2cap::{AclReassembler, HciAclHeader, L2capHeader};
use crate::packets::att::opcode as att_op;
use crate::scripting::bindings::{dynamic_to_bytes, runtime_error};
use crate::scripting::{ScriptGattServer, new_engine};
use crate::types::{Address, AddressType, SimbleError, Uuid};
use rhai::{AST, Array, Blob, CallFnOptions, Dynamic, Engine, EvalAltResult, Map, Scope};
use serde::Serialize;

use super::hci_adapter::{HciChannel, h4_type};

/// Sends one HCI command on `channel`. The packet itself is built by the
/// host layer (`device::host::command`) so there is a single definition of
/// what an HCI command looks like.
fn queue_command(channel: &HciChannel, opcode: [u8; 2], params: &[u8]) -> Result<(), SimbleError> {
    let packet = crate::device::host::command(opcode, params);
    // `command` emits a complete H4 packet; the channel adds the type byte.
    channel.send_command(&packet[1..])
}

/// Queues the controller bring-up shared by both demos. The post-Reset
/// default event mask excludes LE Meta Events (Core Spec Vol 4, Part E,
/// Section 7.3.1, bit 61), so both masks must be opened before any
/// advertising report or connection event can arrive.
fn queue_common_init(channel: &HciChannel) -> Result<(), SimbleError> {
    // The demo advertiser/scanner share the host layer's bring-up, minus the
    // LE Audio host-feature command a real peripheral sends.
    for packet in crate::device::host::init_commands()
        .into_iter()
        .take(3)
    {
        channel.send_command(&packet[1..])?;
    }
    Ok(())
}

/// Queues the scanner's full HCI bring-up: reset, event masks, passive scan
/// parameters, then scan enable with duplicate filtering off (every repeat
/// report carries a fresh RSSI, which is what drives the page's live bars).
pub fn queue_scanner_start(channel: &HciChannel) -> Result<(), SimbleError> {
    queue_common_init(channel)?;
    // LE Set Scan Parameters: active (0x01), interval/window 0x0010, public
    // own address, accept all. Active scanning solicits SCAN_REQ so advertisers'
    // scan-response data (names) is collected — passive scanning never would
    // (rootcanal logs "Not sending LE Scan request ... scanner is passive").
    queue_command(
        channel,
        [0x0B, 0x20],
        &[0x01, 0x10, 0x00, 0x10, 0x00, 0x00, 0x00],
    )?;
    queue_command(channel, [0x0C, 0x20], &[0x01, 0x00]) // LE Set Scan Enable
}

/// A hex-tagged byte payload (manufacturer data with its company id, service
/// data with its service UUID).
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct TaggedBytes {
    /// Hex-encoded tag (company id or service UUID).
    pub tag: String,
    /// Hex-encoded payload bytes.
    pub data: String,
}

/// One decoded LE Advertising Report, ready to serialize for the page. The
/// AD-structure decoding happens here in Rust — the page's JS only renders.
#[derive(Debug, Serialize)]
pub struct ScanReport {
    /// The advertiser's Bluetooth address, formatted as a string.
    pub address: String,
    /// Human-readable address type ("public", "random", ...).
    pub address_type: &'static str,
    /// Whether the advertisement is connectable.
    pub connectable: bool,
    /// Whether this report came from a scan response rather than an advertisement.
    pub scan_response: bool,
    /// Received signal strength in dBm.
    pub rssi: i8,
    /// Decoded complete/shortened local name, if present.
    pub name: Option<String>,
    /// Decoded advertising flags octet, if present.
    pub flags: Option<u8>,
    /// Decoded Tx power level, if present.
    pub tx_power: Option<i8>,
    /// Advertised service UUIDs, formatted as strings.
    pub service_uuids: Vec<String>,
    /// Service data entries keyed by service UUID.
    pub service_data: Vec<TaggedBytes>,
    /// Manufacturer-specific data keyed by company id, if present.
    pub manufacturer_data: Option<TaggedBytes>,
    /// Hex dump of the raw advertising payload.
    pub raw: String,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

fn address_type_name(raw: u8) -> &'static str {
    match raw {
        0x00 => "public",
        0x01 => "random",
        0x02 => "public identity",
        0x03 => "random identity",
        _ => "unknown",
    }
}

/// Decodes the AD structures of one advertising payload into `report`
/// (Core Spec Vol 3, Part C, Section 11: length-type-data triplets).
fn decode_ad_structures(data: &[u8], report: &mut ScanReport) {
    let mut rest = data;
    while let [len, body @ ..] = rest {
        let len = *len as usize;
        if len == 0 || body.len() < len {
            break;
        }
        let (structure, remaining) = body.split_at(len);
        let (ad_kind, payload) = (structure[0], &structure[1..]);
        match ad_kind {
            ad_type::FLAGS if !payload.is_empty() => report.flags = Some(payload[0]),
            ad_type::SHORTENED_LOCAL_NAME | ad_type::COMPLETE_LOCAL_NAME => {
                report.name = Some(String::from_utf8_lossy(payload).into_owned());
            }
            ad_type::INCOMPLETE_16BIT_UUIDS | ad_type::COMPLETE_16BIT_UUIDS => {
                for pair in payload.as_chunks::<2>().0 {
                    report
                        .service_uuids
                        .push(Uuid::from_u16(u16::from_le_bytes([pair[0], pair[1]])).to_string());
                }
            }
            ad_type::INCOMPLETE_128BIT_UUIDS | ad_type::COMPLETE_128BIT_UUIDS => {
                for chunk in payload.as_chunks::<16>().0 {
                    if let Some(uuid) = Uuid::from_bytes(chunk) {
                        report.service_uuids.push(uuid.to_string());
                    }
                }
            }
            ad_type::TX_POWER_LEVEL if !payload.is_empty() => {
                report.tx_power = Some(payload[0] as i8);
            }
            ad_type::SERVICE_DATA_16BIT if payload.len() >= 2 => {
                report.service_data.push(TaggedBytes {
                    tag: Uuid::from_u16(u16::from_le_bytes([payload[0], payload[1]])).to_string(),
                    data: hex(&payload[2..]),
                });
            }
            ad_type::MANUFACTURER_SPECIFIC_DATA if payload.len() >= 2 => {
                report.manufacturer_data = Some(TaggedBytes {
                    tag: format!("{:04X}", u16::from_le_bytes([payload[0], payload[1]])),
                    data: hex(&payload[2..]),
                });
            }
            _ => {}
        }
        rest = remaining;
    }
}

/// Parses an H4 packet into its LE Advertising Reports (LE Meta Event,
/// subevent 0x02), or an empty vec for any other packet. Reports are laid
/// out sequentially per report (event type, address type, address, data,
/// RSSI), the on-air order every real controller and rootcanal emit.
pub fn parse_scan_reports(packet: &[u8]) -> Vec<ScanReport> {
    let Some(HciEvent::Other { code, parameters }) = HciEvent::parse_h4(packet) else {
        return Vec::new();
    };
    if code != hci_event_code::LE_META || parameters.first() != Some(&le_subevent::ADVERTISING_REPORT)
    {
        return Vec::new();
    }
    advertising_reports(parameters)
        .into_iter()
        .map(|report| {
            let mut scan = ScanReport {
                address: Address::new(report.header.address).to_string(),
                address_type: address_type_name(report.header.address_type),
                connectable: report.header.is_connectable(),
                scan_response: report.header.is_scan_response(),
                rssi: report.rssi,
                name: None,
                flags: None,
                tx_power: None,
                service_uuids: Vec::new(),
                service_data: Vec::new(),
                manufacturer_data: None,
                raw: hex(report.data),
            };
            decode_ad_structures(report.data, &mut scan);
            scan
        })
        .collect()
}

/// Pads an advertising payload into the fixed 32-byte HCI parameter block
/// (significant length byte + 31 data bytes) of LE Set Advertising Data /
/// LE Set Scan Response Data.
use crate::device::host::adv_data_param;

/// Builds the advertising payload for a lightweight demo advertiser (used by
/// the scanner page to populate an otherwise-empty scene): flags, an optional
/// 16-bit service UUID, optional manufacturer data, then the name. Extras are
/// dropped and the name trimmed if the 31-byte legacy limit is exceeded — the
/// name is the demo's identity, so it survives longest.
pub fn build_demo_adv_payload(
    name: &str,
    service_uuid: u16,
    mfg_company: u16,
    mfg_data: &[u8],
) -> Vec<u8> {
    const MAX_ADV_LEN: usize = 31;
    let build = |name: &str, with_extras: bool| {
        let mut ad = AdvertisingData::new()
            .with_flags(flags::LE_GENERAL_DISCOVERABLE | flags::BR_EDR_NOT_SUPPORTED);
        if with_extras {
            if service_uuid != 0 {
                ad = ad.with_service_uuid_16(service_uuid);
            }
            if !mfg_data.is_empty() {
                ad = ad.with_manufacturer_data(mfg_company, mfg_data);
            }
        }
        ad.with_name(name).to_bytes()
    };
    let mut bytes = build(name, true);
    if bytes.len() > MAX_ADV_LEN {
        bytes = build(name, false);
    }
    let mut trimmed = name.to_string();
    while bytes.len() > MAX_ADV_LEN && trimmed.pop().is_some() {
        bytes = build(&trimmed, false);
    }
    bytes
}

/// Queues a demo advertiser's full HCI bring-up: reset, event masks,
/// advertising parameters, advertising data (name + optional service UUID +
/// optional manufacturer data) and scan response, then advertising enable.
/// Advertise-only — no GATT server — so the scanner page can cheaply put a few
/// named devices on the air.
pub fn queue_advertiser_start(
    channel: &HciChannel,
    name: &str,
    service_uuid: u16,
    mfg_company: u16,
    mfg_data: &[u8],
) -> Result<(), SimbleError> {
    queue_common_init(channel)?;
    // LE Set Advertising Parameters: 100ms interval, ADV_IND, public own
    // address, all channels, no filter (same shape as ScriptedPeripheral).
    queue_command(
        channel,
        [0x06, 0x20],
        &[
            0xA0, 0x00, 0xA0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x07,
            0x00,
        ],
    )?;
    queue_command(
        channel,
        [0x08, 0x20],
        &adv_data_param(&build_demo_adv_payload(
            name,
            service_uuid,
            mfg_company,
            mfg_data,
        )),
    )?;
    let scan_rsp = AdvertisingData::new().with_name(name).to_bytes();
    queue_command(channel, [0x09, 0x20], &adv_data_param(&scan_rsp))?;
    queue_command(channel, [0x0A, 0x20], &[0x01]) // LE Set Advertising Enable
}

/// Sends one L2CAP PDU as ACL data, fragmented by the host layer so there
/// is a single definition of LE ACL fragmentation.
fn send_acl(channel: &HciChannel, handle: u16, l2cap: &[u8]) -> Result<(), SimbleError> {
    for packet in crate::device::host::acl_packets(handle, l2cap) {
        channel.send_acl_data(&packet[1..])?;
    }
    Ok(())
}

#[cfg_attr(not(any(target_arch = "wasm32", test)), allow(dead_code))]
/// Extracts the `address=` query parameter from a netsim WebSocket URL.
/// Pages write it in display order (`CC:1E:57:00:00:06`), which is what the
/// device's identity must be — SMP computes with it.
fn address_from_ws_url(url: &str) -> Option<Address> {
    url.split(['?', '&'])
        .find_map(|param| param.strip_prefix("address="))
        .and_then(|value| value.split('&').next())
        .and_then(|value| value.parse::<Address>().ok())
}

/// Rewrites a netsim WebSocket URL's `address=` parameter into the byte order
/// netsim actually puts on the air.
///
/// netsim reads that parameter **LSB-first**, so a URL written in display
/// order advertises the address reversed: a device asking for
/// `CC:1E:57:00:00:06` appears as `06:00:00:57:1E:CC`, and nothing can reach
/// it at the address the page believes it has (SMP then computes with the
/// wrong identity too). Pages keep writing display order; this puts the wire
/// order on the query string.
#[cfg_attr(not(any(target_arch = "wasm32", test)), allow(dead_code))]
fn ws_url_with_wire_address(url: &str) -> String {
    let Some(address) = address_from_ws_url(url) else {
        return url.to_string();
    };
    let wire = address.to_netsim_wire_string();
    let display = address.to_string();
    url.replace(&format!("address={display}"), &format!("address={wire}"))
}

fn find_value_handle(server: &ScriptGattServer, uuid: Uuid) -> Option<u16> {
    // Services built by a Rust profile registrar (add_ras, add_pacs,
    // add_ascs) exist only in the GATT database, so fall back to it when
    // the script's own service list does not know the UUID.
    if let Some(handle) = server.with_server(|s| s.device.gatt_db.value_handle_for_uuid(uuid)) {
        return Some(handle);
    }
    server.with_server(|s| {
        s.get_services()
            .iter()
            .flat_map(|service| service.characteristics.clone())
            .find(|characteristic| characteristic.uuid == uuid)
            .and_then(|characteristic| characteristic.value_handle)
    })
}

/// Registers the web runtime's scripting extension: `server.update_value(
/// uuid, bytes)` writes a characteristic's value into the live GattDatabase.
/// This exists because `notify_characteristic_changed` doesn't persist its
/// value into the database, and the web glue treats the database as the one
/// source of truth (the page UI reads it, and value changes become wire
/// notifications) — pending real advertising/notification Rhai bindings.
fn register_web_extensions(engine: &mut Engine) {
    engine.register_fn(
        "update_value",
        |server: &mut ScriptGattServer,
         uuid: Uuid,
         value: Dynamic|
         -> Result<(), Box<EvalAltResult>> {
            let bytes = dynamic_to_bytes(value)?;
            let handle = find_value_handle(server, uuid)
                .ok_or_else(|| runtime_error(format!("no characteristic with UUID {uuid}")))?;
            server
                .with_server(|s| s.device.gatt_db.set_value(handle, &bytes))
                .map_err(|status| runtime_error(format!("set_value failed: ATT error {status}")))
        },
    );
    // The read half: `server.value(uuid)` returns the characteristic's
    // current bytes from the live GattDatabase — including values a central
    // wrote. A script's `fn tick` has no variables that survive between
    // calls, so the database is the only state it can carry; this getter is
    // what lets a device react to writes (setpoints, control points).
    // Real profile implementations, callable from a script. The protocol
    // lives in Rust (`crate::profiles`) — a state machine with tests — and
    // the script just composes a device out of them. `add_ascs` in
    // particular installs the ASE control-point handler, so a peer's Config
    // Codec / Enable writes actually drive the endpoint state machine
    // instead of landing on inert bytes.
    engine.register_fn(
        "add_pacs",
        |server: &mut ScriptGattServer, sink_location: i64, source_location: i64| {
            server.with_server(|s| {
                crate::profiles::PublishedAudioCapabilitiesService::register(
                    &mut s.device.gatt_db,
                    sink_location as u32,
                    source_location as u32,
                );
            });
        },
    );
    // Several profiles carry IEEE-754 floats (ranging distance, weight
    // scales, some sensor readings) and a script has no other way to lay
    // one out. Little-endian, as everything on the wire is.
    engine.register_fn("f32_le", |value: f64| -> Blob {
        (value as f32).to_le_bytes().to_vec()
    });

    engine.register_fn(
        // Ranging Service (185B) — the GATT half of Bluetooth 6.0 Channel
        // Sounding: a responder publishes distance estimates a peer reads or
        // subscribes to. The ranging procedure itself is a controller
        // feature; this is what a phone actually talks to.
        "add_ras",
        |server: &mut ScriptGattServer| {
            server.with_server(|s| {
                crate::profiles::RangingService::register(&mut s.device.gatt_db);
            });
        },
    );
    engine.register_fn(
        "add_ascs",
        |server: &mut ScriptGattServer,
         sink_ase_ids: Dynamic,
         source_ase_ids: Dynamic|
         -> Result<(), Box<EvalAltResult>> {
            let sink = dynamic_to_bytes(sink_ase_ids)?;
            let source = dynamic_to_bytes(source_ase_ids)?;
            server.with_server(|s| {
                crate::profiles::AudioStreamControlService::register(
                    &mut s.device.gatt_db,
                    &sink,
                    &source,
                );
            });
            Ok(())
        },
    );
    // Adds a 16-bit UUID to the advertisement. Services registered by the
    // Rust profile registrars live in the GATT database rather than the
    // script's service list, so they need advertising explicitly.
    engine.register_fn(
        "advertise_service_uuid",
        |server: &mut ScriptGattServer, uuid16: i64| -> Result<(), Box<EvalAltResult>> {
            let uuid16 = u16::try_from(uuid16).map_err(|_| {
                runtime_error(format!("advertise_service_uuid: not a 16-bit uuid: {uuid16}"))
            })?;
            server.with_server(|s| {
                s.device
                    .advertising_data
                    .get_or_insert_with(crate::gap::AdvertisingData::new)
                    .service_uuids_16
                    .push(uuid16);
            });
            Ok(())
        },
    );

    // The return path of the event channel: a script tells the host (a page,
    // a test) something that isn't GATT state — a log line, a decoded frame,
    // a state transition. Payloads cross as JSON.
    engine.register_fn(
        "emit",
        |server: &mut ScriptGattServer,
         kind: &str,
         payload: Dynamic|
         -> Result<(), Box<EvalAltResult>> {
            let value: serde_json::Value = rhai::serde::from_dynamic(&payload)
                .map_err(|e| runtime_error(format!("emit payload is not serializable: {e}")))?;
            let message = serde_json::json!({ "event": kind, "payload": value });
            server.push_emitted(message.to_string());
            Ok(())
        },
    );

    // The LE Audio media plane, script side. `send_audio` streams one SDU to
    // the connected peer; `take_audio` drains what a sink has received.
    engine.register_fn(
        "send_audio",
        |server: &mut ScriptGattServer, sdu: Dynamic| -> Result<bool, Box<EvalAltResult>> {
            let bytes = dynamic_to_bytes(sdu)?;
            Ok(server.with_server(|s| {
                let handle = s.device.audio_handle();
                match handle.and_then(|h| s.device.build_audio_packet(h, &bytes)) {
                    Some(packet) => {
                        s.device.audio_tx_pending.push(packet);
                        true
                    }
                    None => false,
                }
            }))
        },
    );
    engine.register_fn(
        "take_audio",
        |server: &mut ScriptGattServer| -> rhai::Array {
            server.with_server(|s| {
                s.device
                    .take_audio()
                    .into_iter()
                    .map(Dynamic::from_blob)
                    .collect()
            })
        },
    );
    engine.register_fn(
        "value",
        |server: &mut ScriptGattServer, uuid: Uuid| -> Result<Blob, Box<EvalAltResult>> {
            let handle = find_value_handle(server, uuid)
                .ok_or_else(|| runtime_error(format!("no characteristic with UUID {uuid}")))?;
            // Host-side read: a device sees its own attributes regardless of
            // client permissions, so a script can read the write-only control
            // point a peer just wrote.
            server
                .with_server(|s| s.device.gatt_db.value(handle).map(Blob::from))
                .ok_or_else(|| runtime_error(format!("no value for characteristic {uuid}")))
        },
    );
}

/// What a client subscribed to on a characteristic's CCCD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CccdSubscription {
    /// Nothing enabled.
    None,
    /// Notifications (unacknowledged).
    Notify,
    /// Indications (confirmed by the peer).
    Indicate,
}

/// A notify-capable characteristic the host glue watches for value changes.
#[derive(Debug, Clone)]
struct WatchedCharacteristic {
    server_index: usize,
    value_handle: u16,
    cccd_handle: Option<u16>,
}

/// Everything the peripheral page reports back to its JS, per tick.
#[derive(Serialize)]
struct PeripheralStatus {
    name: String,
    address: String,
    connected: bool,
    peer: Option<String>,
    tick_defined: bool,
    last_error: Option<String>,
    services: Vec<ServiceStatus>,
}

#[derive(Serialize)]
struct ServiceStatus {
    uuid: String,
    characteristics: Vec<CharacteristicStatus>,
}

#[derive(Serialize)]
struct CharacteristicStatus {
    uuid: String,
    /// The GATT property bitmask (READ/WRITE/NOTIFY/INDICATE/…), so the page
    /// can render the generic R/W/N/I chips for any script-built device, not
    /// just the ones it recognizes.
    properties: i64,
    value: String,
    subscribed: bool,
}

/// A Simble peripheral whose entire behavior comes from a Rhai script — the
/// web demos' scripted-device engine, host-side. The script builds real
/// `android::BluetoothGattServer`s over real `VirtualDevice`s; this glue
/// connects the first one to an [`HciChannel`]:
///
/// - **Advertising is host glue for now**: the `android::*` bindings have no
///   `BluetoothLeAdvertiser` yet, so [`Self::queue_start`] issues the HCI
///   advertising sequence itself, carrying the script device's name and
///   16-bit service UUIDs (re-derived on every re-Run, so a renamed device
///   advertises its new name).
/// - **Ticking**: if the script defines `fn tick(server, t)`, it's called on
///   every host tick with seconds-since-start — behavior lives in the
///   script, not the page.
/// - **Notifications**: any notify-capable characteristic whose database
///   value changes (script `update_value`, or a peer write) is notified to
///   a subscribed central automatically.
pub struct ScriptedPeripheral {
    engine: Engine,
    ast: AST,
    scope: Scope<'static>,
    servers: Vec<ScriptGattServer>,
    /// The LE host layer: HCI event dispatch, ATT/SMP replies, ACL framing.
    host: LeHost,
    connection: Option<(u16, Address)>,
    tick_defined: bool,
    /// Whether the script defines `fn on_event(server, event)` — the
    /// handler that receives ATT events and host-pushed UI events.
    on_event_defined: bool,
    /// Per-device state bound as `this` for `tick`/`on_event`.
    state: Dynamic,
    watched: Vec<WatchedCharacteristic>,
    last_values: HashMap<u16, Vec<u8>>,
    last_error: Option<String>,
}

/// The result of evaluating one REPL line in a [`ScriptedPeripheral`] session
/// (the API Explorer emits exactly one Rhai statement per Execute): the
/// statement's return value rendered for display, and any queue events it
/// produced, already formatted for the Explorer's log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplOutcome {
    /// The statement's return value, rendered for display.
    pub value: String,
    /// Queue events produced by the statement, formatted for the log.
    pub events: Vec<String>,
}

/// The JSON shape returned to the Explorer page per Execute — success carries
/// the rendered return value and events; failure carries the Rhai error.
#[derive(Serialize)]
struct ReplResult {
    ok: bool,
    value: String,
    error: Option<String>,
    events: Vec<String>,
}

/// Renders a REPL line's return value for the Explorer log. Unit (the result
/// of a `let` binding or a void call) shows as `()`; a `Uuid` shows in its
/// canonical string form; everything else uses Rhai's own `Display`.
fn display_value(value: &Dynamic) -> String {
    if value.is_unit() {
        return "()".to_string();
    }
    if let Some(uuid) = value.clone().try_cast::<Uuid>() {
        return uuid.to_string();
    }
    let rendered = value.to_string();
    if rendered.is_empty() {
        "()".to_string()
    } else {
        rendered
    }
}

/// Formats one queued `ScriptEvent` map (as seen by scripts) into a short log
/// line for the Explorer, e.g. `service_added uuid=180D status=0`.
fn format_event(map: &Map) -> String {
    let mut parts = vec![
        map.get("event")
            .map(|v| v.clone().into_string().unwrap_or_default())
            .unwrap_or_default(),
    ];
    if let Some(uuid) = map.get("uuid").and_then(|v| v.clone().try_cast::<Uuid>()) {
        parts.push(format!("uuid={uuid}"));
    }
    if let Some(status) = map.get("status").and_then(|v| v.as_int().ok()) {
        parts.push(format!("status={status}"));
    }
    parts.join(" ")
}

impl ScriptedPeripheral {
    /// Compiles and runs `script` on a fresh engine, collecting every
    /// `android::BluetoothGattServer` the script left in a top-level
    /// variable. Compile and runtime errors come back as display strings
    /// ready for the page's error pane.
    pub fn run_script(script: &str) -> Result<Self, String> {
        let mut engine = new_engine();
        register_web_extensions(&mut engine);
        let ast = engine.compile(script).map_err(|e| e.to_string())?;
        let mut scope = Scope::new();
        engine
            .run_ast_with_scope(&mut scope, &ast)
            .map_err(|e| e.to_string())?;

        let servers: Vec<ScriptGattServer> = scope
            .iter()
            .filter_map(|(_, _, value)| value.try_cast::<ScriptGattServer>())
            .collect();
        if servers.is_empty() {
            return Err("script must create an android::BluetoothGattServer \
                 and keep it in a top-level variable"
                .to_string());
        }

        let tick_defined = ast
            .iter_functions()
            .any(|f| f.name == "tick" && f.params.len() == 2);
        let on_event_defined = ast
            .iter_functions()
            .any(|f| f.name == "on_event" && f.params.len() == 2);

        let mut peripheral = Self {
            engine,
            ast,
            scope,
            servers,
            host: LeHost::new(),
            connection: None,
            tick_defined,
            on_event_defined,
            // Persistent per-device state, bound as `this` for `tick` and
            // `on_event` (Rhai's documented event-handler pattern): script
            // functions are pure and cannot see the calling scope, so this
            // map is how a device remembers anything between calls.
            state: Dynamic::from_map(rhai::Map::new()),
            watched: Vec::new(),
            last_values: HashMap::new(),
            last_error: None,
        };
        peripheral.rebuild_watch_list();
        Ok(peripheral)
    }

    /// Creates an empty REPL session for the API Explorer: a fresh engine
    /// (with the web `update_value` extension), an empty persistent scope, and
    /// no servers yet. Lines are fed one at a time with [`Self::eval_line`];
    /// once a line binds an `android::BluetoothGattServer`, the session hosts
    /// it exactly like a scripted device (advertising, connections,
    /// notifications), so the Explorer's clicks build a real, hostable device.
    pub fn new_session() -> Self {
        let mut engine = new_engine();
        register_web_extensions(&mut engine);
        let ast = engine.compile("").expect("empty script compiles");
        Self {
            engine,
            ast,
            scope: Scope::new(),
            servers: Vec::new(),
            host: LeHost::new(),
            connection: None,
            tick_defined: false,
            on_event_defined: false,
            state: Dynamic::from_map(rhai::Map::new()),
            watched: Vec::new(),
            last_values: HashMap::new(),
            last_error: None,
        }
    }

    /// Evaluates one Rhai statement in the persistent session scope (top-level
    /// `let` bindings persist across calls, so `let svc1 = ...` stays usable by
    /// later Executes). Re-collects the servers the scope now holds and
    /// rebuilds the notify watch-list, so a service or characteristic added by
    /// this line is immediately hosted. Returns the statement's rendered return
    /// value and the events it produced, or the Rhai error as a string.
    pub fn eval_line(&mut self, line: &str) -> Result<ReplOutcome, String> {
        let value = self
            .engine
            .eval_with_scope::<Dynamic>(&mut self.scope, line)
            .map_err(|e| e.to_string())?;
        self.servers = self
            .scope
            .iter()
            .filter_map(|(_, _, value)| value.try_cast::<ScriptGattServer>())
            .collect();
        self.rebuild_watch_list();
        let events = self.drain_events_display();
        Ok(ReplOutcome {
            value: display_value(&value),
            events,
        })
    }

    /// [`Self::eval_line`] rendered as the JSON the Explorer page consumes.
    pub fn eval_line_json(&mut self, line: &str) -> String {
        let result = match self.eval_line(line) {
            Ok(outcome) => ReplResult {
                ok: true,
                value: outcome.value,
                error: None,
                events: outcome.events,
            },
            Err(error) => ReplResult {
                ok: false,
                value: String::new(),
                error: Some(error),
                events: Vec::new(),
            },
        };
        serde_json::to_string(&result)
            .unwrap_or_else(|e| format!("{{\"ok\":false,\"error\":\"{e}\"}}"))
    }

    /// Drains the session's queued events and formats them for the log.
    fn drain_events_display(&mut self) -> Vec<String> {
        match self.engine.eval::<Array>("take_events()") {
            Ok(events) => events
                .into_iter()
                .filter_map(|event| event.try_cast::<Map>())
                .map(|map| format_event(&map))
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Whether the session/script has produced at least one server to host.
    pub fn has_server(&self) -> bool {
        !self.servers.is_empty()
    }

    /// A signature of what the primary server would advertise (name + sorted
    /// 16-bit service UUIDs). The Explorer re-issues advertising when this
    /// changes, so a device gains its new services on the air as it's built.
    pub fn adv_signature(&self) -> String {
        if self.servers.is_empty() {
            return String::new();
        }
        let mut uuids = self.primary_service_uuids_16();
        uuids.sort_unstable();
        format!("{}|{uuids:?}", self.device_name())
    }

    fn primary(&self) -> &ScriptGattServer {
        &self.servers[0]
    }

    /// The scripted device's name (also what the page shows in its header).
    pub fn device_name(&self) -> String {
        self.primary().with_server(|s| s.device.name.clone())
    }

    /// Stamps the device's on-air identity. The script engine allocates a
    /// per-session placeholder address, but SMP pairing computes with
    /// `device.address`/`address_type` — so the scene must overwrite them
    /// with the address it actually advertises (public, per the advertising
    /// parameters in [`Self::queue_start`]), or pairing against a real
    /// stack fails its confirm/DHKey check.
    pub fn set_identity(&mut self, address: Address) {
        self.primary().with_server(|s| {
            s.device.address = address;
            s.device.address_type = AddressType::Public;
            // Scenes have a real controller, so SMP key distribution waits
            // for the Encryption Change event as the spec requires.
            s.device.defer_key_distribution = true;
        });
    }

    /// Drains the isochronous SDUs this device has received (media plane).
    pub fn take_audio(&mut self) -> Vec<Vec<u8>> {
        self.primary().with_server(|s| s.device.take_audio())
    }

    /// Records a non-fatal runtime problem for the page's error pane.
    pub fn record_error(&mut self, message: String) {
        self.last_error = Some(message);
    }

    /// Host-side write of a characteristic's value by UUID string — the same
    /// live-database path as the script's `update_value`, exposed to the page
    /// so UI (the lightbulb's colour picker) can drive a value directly. A
    /// subscribed central is notified of the change on the next tick. `uuid` is
    /// the string form (`"FFE9"` or a full 128-bit UUID).
    pub fn set_characteristic_value(&mut self, uuid: &str, bytes: &[u8]) -> Result<(), String> {
        let uuid = uuid.parse::<Uuid>().map_err(|e| e.to_string())?;
        let handle = find_value_handle(self.primary(), uuid)
            .ok_or_else(|| format!("no characteristic with UUID {uuid}"))?;
        self.primary()
            .with_server(|s| s.device.gatt_db.set_value(handle, bytes))
            .map_err(|status| format!("set_value failed: ATT error {status}"))
    }

    /// Queues the peripheral's full HCI bring-up: reset, event masks,
    /// advertising parameters, advertising data + scan response carrying the
    /// script device's identity, then advertising enable.
    pub fn queue_start(&self, channel: &HciChannel) -> Result<(), SimbleError> {
        let uuids = self.primary_service_uuids_16();
        let commands = self
            .primary()
            .with_server(|s| self.host.start_advertising(&s.device, &uuids))?;
        for packet in commands {
            channel.inject_host_packet(packet)?;
        }
        Ok(())
    }

    fn primary_service_uuids_16(&self) -> Vec<u16> {
        self.primary().with_server(|s| {
            s.get_services()
                .iter()
                .filter_map(|service| match service.uuid {
                    Uuid::Uuid16(u) => Some(u),
                    Uuid::Uuid128(_) => None,
                })
                .collect()
        })
    }

    /// Indexes every notify/indicate-capable characteristic and its CCCD:
    /// the descriptor the script attached if present, otherwise the next
    /// CCCD attribute in the database before the following declaration
    /// (covering natively-registered profiles like `HeartRateService`).
    fn rebuild_watch_list(&mut self) {
        self.watched.clear();
        self.last_values.clear();
        for (server_index, server) in self.servers.iter().enumerate() {
            server.with_server(|s| {
                for service in s.get_services() {
                    for characteristic in &service.characteristics {
                        let notifying = BluetoothGattCharacteristic::PROPERTY_NOTIFY
                            | BluetoothGattCharacteristic::PROPERTY_INDICATE;
                        if characteristic.properties & notifying == 0 {
                            continue;
                        }
                        let Some(value_handle) = characteristic.value_handle else {
                            continue;
                        };
                        let cccd_handle = characteristic
                            .descriptors
                            .iter()
                            .find(|d| d.uuid == Uuid::CCCD)
                            .and_then(|d| d.handle)
                            .or_else(|| find_cccd_after(&s.device.gatt_db, value_handle));
                        self.watched.push(WatchedCharacteristic {
                            server_index,
                            value_handle,
                            cccd_handle,
                        });
                    }
                }
            });
        }
        for watch in &self.watched {
            if let Some(value) = self.attribute_value(watch) {
                self.last_values.insert(watch.value_handle, value);
            }
        }
    }

    fn attribute_value(&self, watch: &WatchedCharacteristic) -> Option<Vec<u8>> {
        self.servers[watch.server_index].with_server(|s| {
            s.device
                .gatt_db
                .attributes
                .get(&watch.value_handle)
                .map(|attribute| attribute.value.clone())
        })
    }

    /// What the client asked for on this characteristic's CCCD (Core Spec
    /// Vol 3, Part G, Section 3.3.3.3): bit 0 notify, bit 1 indicate. Both
    /// matter — several SIG profiles mandate Indicate, and a device that
    /// only ever notifies delivers nothing to those clients.
    fn cccd_subscription(&self, watch: &WatchedCharacteristic) -> CccdSubscription {
        let Some(cccd) = watch.cccd_handle else {
            return CccdSubscription::None;
        };
        let value = self.servers[watch.server_index]
            .with_server(|s| s.device.cccd_value(cccd).unwrap_or(0));
        if value & 0x0002 != 0 {
            CccdSubscription::Indicate
        } else if value & 0x0001 != 0 {
            CccdSubscription::Notify
        } else {
            CccdSubscription::None
        }
    }

    /// Routes one controller-to-host H4 packet: connection events into the
    /// scripted device's connection state, ACL data through reassembly into
    /// the real L2CAP/ATT dispatch, responses back onto the channel.
    pub fn handle_packet(
        &mut self,
        channel: &HciChannel,
        packet: &[u8],
    ) -> Result<(), SimbleError> {
        // The host layer owns HCI event dispatch, ATT/SMP responses, and the
        // ACL fragmentation; this glue only moves its output to the channel
        // and mirrors the connection for `status_json`.
        let outgoing = self
            .primary()
            .clone()
            .with_server(|s| self.host.handle_packet(&mut s.device, packet))?;
        for out in outgoing {
            channel.inject_host_packet(out)?;
        }
        self.connection = self.host.connection();
        Ok(())
    }

    /// Delivers queued events (ATT activity, and anything the host pushed
    /// with `push_event`) to the script's `fn on_event(server, event)`.
    ///
    /// State is bound as `this` — Rhai's documented event-handler pattern —
    /// because script functions are pure and cannot see the calling scope,
    /// so a map bound this way is how a device remembers anything between
    /// calls. Handler errors land in `last_error` rather than killing the
    /// device, matching how tick errors are treated.
    fn dispatch_events(&mut self) {
        if !self.on_event_defined {
            return;
        }
        let events = self.primary().take_own_events();
        if events.is_empty() {
            return;
        }
        let server = Dynamic::from(self.primary().clone());
        let Self {
            engine,
            ast,
            scope,
            state,
            last_error,
            ..
        } = self;
        for event in events {
            let options = CallFnOptions::new().eval_ast(false).bind_this_ptr(state);
            let args = (server.clone(), event);
            match engine.call_fn_with_options::<Dynamic>(options, scope, ast, "on_event", args) {
                Ok(_) => *last_error = None,
                Err(e) => *last_error = Some(e.to_string()),
            }
        }
    }

    /// Pushes an event into the running script from outside the stack — a UI
    /// control, a test, a host simulating a condition. Delivered to
    /// `on_event` on the next tick.
    pub fn push_event(&mut self, kind: &str, payload_json: &str) {
        self.primary().push_event(kind, payload_json.to_string());
    }

    /// Drains what the script emitted for the host with `server.emit(...)`.
    pub fn take_emitted(&mut self) -> Vec<String> {
        self.primary().take_emitted()
    }

    /// One host tick: calls the script's `fn tick(server, t)` if defined
    /// (`t` = seconds since Run), then turns any changed notify-capable
    /// value into a real ATT notification for a subscribed central.
    pub fn tick(&mut self, channel: &HciChannel, t_seconds: f64) -> Result<(), SimbleError> {
        // Events first, so a write that arrived since the last tick is
        // handled before the periodic tick sees the world.
        self.dispatch_events();
        if self.tick_defined {
            let args = (Dynamic::from(self.primary().clone()), t_seconds);
            // eval_ast(false): the script body already ran in `run_script`;
            // re-evaluating it here would rebuild the device every tick.
            let result = self.engine.call_fn_with_options::<Dynamic>(
                CallFnOptions::new().eval_ast(false),
                &mut self.scope,
                &self.ast,
                "tick",
                args,
            );
            match result {
                Ok(_) => self.last_error = None,
                Err(e) => self.last_error = Some(e.to_string()),
            }
        }
        self.flush_value_notifications(channel)?;
        // Ship any SDUs the script queued with send_audio (the media plane
        // is unacknowledged, so this is fire-and-forget).
        let sdus = self
            .primary()
            .with_server(|s| std::mem::take(&mut s.device.audio_tx_pending));
        for packet in sdus {
            channel.inject_host_packet(packet)?;
        }
        // The observer queue records every ATT event for scripts; nothing
        // drains it across ticks, so cap it here (scripts that want events
        // must consume them with `take_events()` inside their own tick).
        let _ = self.engine.eval::<Array>("take_events()");
        Ok(())
    }

    fn flush_value_notifications(&mut self, channel: &HciChannel) -> Result<(), SimbleError> {
        let watched = self.watched.clone();
        for watch in watched {
            let Some(current) = self.attribute_value(&watch) else {
                continue;
            };
            if self.last_values.get(&watch.value_handle) == Some(&current) {
                continue;
            }
            self.last_values.insert(watch.value_handle, current.clone());
            let Some((handle, _)) = self.connection else {
                continue;
            };
            let l2cap = match self.cccd_subscription(&watch) {
                CccdSubscription::None => continue,
                CccdSubscription::Notify => self.servers[watch.server_index].with_server(|s| {
                    Ok(s.device
                        .create_notification_for(handle, watch.value_handle, &current))
                }),
                // An indication is confirmed by the peer, so only one may be
                // outstanding; `create_indication` refuses a second and the
                // value is picked up on a later tick.
                CccdSubscription::Indicate => self.servers[watch.server_index].with_server(|s| {
                    s.device
                        .create_indication(handle, watch.value_handle, &current)
                }),
            };
            match l2cap {
                Ok(l2cap) => send_acl(channel, handle, &l2cap)?,
                Err(_) => {
                    // Indication still in flight — re-send this value once the
                    // confirmation lands rather than dropping it.
                    self.last_values.remove(&watch.value_handle);
                }
            }
        }
        Ok(())
    }

    /// The page-facing status snapshot, as JSON.
    pub fn status_json(&self) -> String {
        // An empty REPL session (no server built yet) reports an empty device
        // so the Explorer's viewer can render "nothing here yet" cleanly.
        if self.servers.is_empty() {
            let empty = PeripheralStatus {
                name: String::new(),
                address: String::new(),
                connected: false,
                peer: None,
                tick_defined: false,
                last_error: self.last_error.clone(),
                services: Vec::new(),
            };
            return serde_json::to_string(&empty)
                .unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"));
        }
        // Subscription state is resolved before the server borrow below —
        // `cccd_subscription` borrows the same server, and `with_server`
        // borrows are not reentrant.
        let subscribed_handles: Vec<u16> = self
            .watched
            .iter()
            .filter(|w| self.cccd_subscription(w) != CccdSubscription::None)
            .map(|w| w.value_handle)
            .collect();
        let services = self.primary().with_server(|s| {
            s.get_services()
                .iter()
                .map(|service| ServiceStatus {
                    uuid: service.uuid.to_string(),
                    characteristics: service
                        .characteristics
                        .iter()
                        .map(|characteristic| {
                            let value = characteristic
                                .value_handle
                                .and_then(|h| s.device.gatt_db.attributes.get(&h))
                                .map(|attribute| hex(&attribute.value))
                                .unwrap_or_default();
                            let subscribed = characteristic
                                .value_handle
                                .is_some_and(|h| subscribed_handles.contains(&h));
                            CharacteristicStatus {
                                uuid: characteristic.uuid.to_string(),
                                properties: characteristic.properties as i64,
                                value,
                                subscribed,
                            }
                        })
                        .collect(),
                })
                .collect()
        });
        let status = PeripheralStatus {
            name: self.device_name(),
            address: self.primary().with_server(|s| s.device.address.to_string()),
            connected: self.connection.is_some(),
            peer: self.connection.map(|(_, peer)| peer.to_string()),
            tick_defined: self.tick_defined,
            last_error: self.last_error.clone(),
            services,
        };
        serde_json::to_string(&status).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
    }
}

/// Finds the CCCD belonging to the characteristic whose value sits at
/// `value_handle`, scanning forward until the next declaration bounds the
/// characteristic's descriptor group (Core Spec Vol 3, Part G, Section 3.3).
fn find_cccd_after(db: &crate::gatt::GattDatabase, value_handle: u16) -> Option<u16> {
    db.attributes
        .range(value_handle.checked_add(1)?..)
        .take_while(|(_, attribute)| {
            !matches!(
                attribute.uuid,
                Uuid::CHARACTERISTIC | Uuid::PRIMARY_SERVICE | Uuid::SECONDARY_SERVICE
            )
        })
        .find(|(_, attribute)| attribute.uuid == Uuid::CCCD)
        .map(|(&handle, _)| handle)
}

/// Runs a Rhai *test* script — one that builds devices and calls `assert(...)`
/// — in a fresh engine, returning `Ok(())` if every assertion passed or the
/// error message (a failed assert, or a compile/runtime error) otherwise.
///
/// Unlike [`ScriptedPeripheral::run_script`] this does not require the script to
/// build a server: a pure-assertion test is valid. The same script is a device,
/// a test, and a CI fixture — this is the runner for the "test" role.
pub fn run_test_script(script: &str) -> Result<(), String> {
    let mut engine = new_engine();
    register_web_extensions(&mut engine);
    let ast = engine
        .compile(script)
        .map_err(|e| format!("compile error: {e}"))?;
    let mut scope = Scope::new();
    engine
        .run_ast_with_scope(&mut scope, &ast)
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Lints a Rhai script without running it: compiles it in the same engine
/// [`run_test_script`] would use (so `android::*` / `uuid::*` and the test
/// bindings are all in scope) and reports a syntax/parse error if any, with
/// its position. Side-effect-free — nothing executes — so it's the cheap
/// pre-flight for an agent's generate-check-fix loop (`simble --no-run`).
pub fn lint_script(script: &str) -> Result<(), String> {
    let mut engine = new_engine();
    register_web_extensions(&mut engine);
    engine
        .compile(script)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// The central's connect → discover progression.
#[derive(Debug, Clone, Copy, PartialEq)]
enum CentralPhase {
    /// Waiting to connect to the target advertiser.
    Connecting,
    /// Connected; exchanging ATT MTU.
    ExchangingMtu,
    /// Reading the peer's primary services.
    DiscoveringServices,
    /// Reading characteristics of `services[i]`.
    DiscoveringCharacteristics(usize),
    /// Discovery complete.
    Ready,
}

impl CentralPhase {
    /// A short human label for the current phase.
    fn label(self) -> &'static str {
        match self {
            CentralPhase::Connecting => "connecting",
            CentralPhase::ExchangingMtu => "exchanging MTU",
            CentralPhase::DiscoveringServices => "discovering services",
            CentralPhase::DiscoveringCharacteristics(_) => "discovering characteristics",
            CentralPhase::Ready => "ready",
        }
    }
}

/// A client-initiated GATT operation, queued from the UI and sent when the
/// central is idle (one outstanding request at a time).
#[derive(Clone)]
enum CentralOp {
    /// Read a characteristic value handle.
    Read(u16),
    /// Write a value to a characteristic value handle.
    Write(u16, Vec<u8>),
    /// Enable notifications by writing 0x0001 to the CCCD after a value handle.
    Subscribe(u16),
}

/// A scene central: connects to a peripheral by address over the shared
/// [`Link`], exchanges MTU, and discovers its GATT — the *client* half of the
/// server/client view. It drives a [`GattClient`], framing ATT over L2CAP over
/// ACL, the mirror of the peripheral's inbound path, and supports interactive
/// read / write / subscribe once discovery is complete.
struct CentralDevice {
    target: Address,
    client: GattClient,
    reassembler: AclReassembler,
    phase: CentralPhase,
    connect_requested: bool,
    /// Operations requested from the UI, awaiting their turn on the link.
    pending_ops: std::collections::VecDeque<CentralOp>,
    /// The operation whose response we're waiting for, if any.
    in_flight: Option<CentralOp>,
    /// Latest value seen per handle (from a read response or a notification).
    values: std::collections::BTreeMap<u16, Vec<u8>>,
    /// Value handles with notifications enabled.
    subscribed: std::collections::BTreeSet<u16>,
    /// Outgoing isochronous SDU sequence number (media plane).
    audio_tx_sequence: u16,
}

impl CentralDevice {
    fn new(target: Address) -> Self {
        Self {
            target,
            client: GattClient::new(0, target),
            reassembler: AclReassembler::new(),
            phase: CentralPhase::Connecting,
            connect_requested: false,
            pending_ops: std::collections::VecDeque::new(),
            in_flight: None,
            values: std::collections::BTreeMap::new(),
            subscribed: std::collections::BTreeSet::new(),
            audio_tx_sequence: 0,
        }
    }

    /// Queue a read of the given value handle.
    fn queue_read(&mut self, value_handle: u16) {
        self.pending_ops.push_back(CentralOp::Read(value_handle));
    }

    /// Queue a write of `value` to the given value handle.
    fn queue_write(&mut self, value_handle: u16, value: Vec<u8>) {
        self.pending_ops
            .push_back(CentralOp::Write(value_handle, value));
    }

    /// Queue enabling notifications on the given value handle (writes its CCCD).
    fn queue_subscribe(&mut self, value_handle: u16) {
        self.pending_ops
            .push_back(CentralOp::Subscribe(value_handle));
    }

    /// Send the next queued operation when discovery is done and nothing is in
    /// flight.
    fn pump_ops(&mut self, channel: &HciChannel) {
        if self.phase != CentralPhase::Ready || self.in_flight.is_some() {
            return;
        }
        let Some(op) = self.pending_ops.pop_front() else {
            return;
        };
        let handle = self.client.connection_handle;
        let req = match &op {
            CentralOp::Read(h) => self.client.create_read_request(*h),
            CentralOp::Write(h, v) => self.client.create_write_request(*h, v),
            // The CCCD sits at value_handle + 1 in the standard layout.
            CentralOp::Subscribe(h) => self.client.create_write_request(h + 1, &[0x01, 0x00]),
        };
        let _ = send_acl(channel, handle, &req);
        self.in_flight = Some(op);
    }

    /// On the first tick, request a connection to the target advertiser.
    fn produce(&mut self, channel: &HciChannel) {
        if !self.connect_requested && self.phase == CentralPhase::Connecting {
            let mut params = vec![0x10, 0x00, 0x10, 0x00, 0x00, 0x00];
            let mut peer = self.target.to_be_bytes();
            peer.reverse(); // little-endian on the wire
            params.extend_from_slice(&peer);
            let _ = queue_command(channel, [0x0D, 0x20], &params); // LE Create Connection
            self.connect_requested = true;
        }
        self.pump_ops(channel);
    }

    /// Consume one controller→host packet: a connection completion starts the
    /// discovery flow; ATT responses advance it.
    fn consume(&mut self, channel: &HciChannel, packet: &[u8]) {
        match packet.first() {
            Some(&h4_type::HCI_EVENT) => {
                // Parsed through the same typed view the peripheral host
                // uses, rather than a second hand-rolled copy: this event
                // was where a dropped field silently broke pairing, and one
                // parser is easier to keep right than two. The Enhanced
                // variant is accepted too — a resolving central gets that
                // one, and the old byte test (`packet[3] == 0x01`) ignored
                // it, so such a connection never started discovery.
                if let Some(HciEvent::LeConnectionComplete(event)) = HciEvent::parse_h4(packet)
                    && event.status == 0x00
                {
                    let handle = event.connection_handle.get() & 0x0FFF;
                    self.client = GattClient::new(handle, self.target);
                    self.phase = CentralPhase::ExchangingMtu;
                    let req = self.client.create_exchange_mtu_request(517);
                    let _ = send_acl(channel, handle, &req);
                }
            }
            Some(&h4_type::HCI_ACL_DATA) => {
                if let Some((header, payload)) = HciAclHeader::parse(&packet[1..]) {
                    let handle = header.handle();
                    let is_first = header.is_first_fragment();
                    if let Ok(Some(frame)) =
                        self.reassembler.push_fragment(handle, is_first, payload)
                        && let Some((_, att)) = L2capHeader::parse(&frame)
                    {
                        self.dispatch_att(channel, att);
                    }
                }
            }
            _ => {}
        }
    }

    /// Advance the discovery state machine, or handle a read/write/notification,
    /// on one ATT response.
    fn dispatch_att(&mut self, channel: &HciChannel, att: &[u8]) {
        let handle = self.client.connection_handle;
        let Some(&op) = att.first() else { return };
        let is_error = op == att_op::ERROR_RSP;

        // Notifications can arrive at any time, independent of the request FSM.
        if op == att_op::HANDLE_VALUE_NTF && att.len() >= 3 {
            let value_handle = u16::from_le_bytes([att[1], att[2]]);
            self.values.insert(value_handle, att[3..].to_vec());
            return;
        }

        match self.phase {
            CentralPhase::ExchangingMtu => {
                if op == att_op::EXCHANGE_MTU_RSP && att.len() >= 3 {
                    let server_mtu = u16::from_le_bytes([att[1], att[2]]);
                    self.client.on_exchange_mtu_response(server_mtu, 517);
                }
                self.phase = CentralPhase::DiscoveringServices;
                let req = self.client.create_discover_services_request(0x0001, 0xFFFF);
                let _ = send_acl(channel, handle, &req);
            }
            CentralPhase::DiscoveringServices => {
                if is_error {
                    self.start_characteristic_discovery(channel);
                } else if op == att_op::READ_BY_GROUP_TYPE_RSP {
                    let _ = self.client.on_discover_services_response(att);
                    let last_end = self.client.services.last().map_or(0xFFFF, |s| s.end_handle);
                    if last_end < 0xFFFF {
                        let req = self
                            .client
                            .create_discover_services_request(last_end + 1, 0xFFFF);
                        let _ = send_acl(channel, handle, &req);
                    } else {
                        self.start_characteristic_discovery(channel);
                    }
                }
            }
            CentralPhase::DiscoveringCharacteristics(i) => {
                if is_error {
                    self.next_characteristic_service(channel, i);
                } else if op == att_op::READ_BY_TYPE_RSP {
                    let svc_uuid = self.client.services[i].uuid;
                    let _ = self
                        .client
                        .on_discover_characteristics_response(svc_uuid, att);
                    let svc = &self.client.services[i];
                    let end = svc.end_handle;
                    let last = svc
                        .characteristics
                        .last()
                        .map_or(svc.start_handle, |c| c.value_handle);
                    if last < end {
                        let req = self
                            .client
                            .create_discover_characteristics_request(last + 1, end);
                        let _ = send_acl(channel, handle, &req);
                    } else {
                        self.next_characteristic_service(channel, i);
                    }
                }
            }
            CentralPhase::Ready => {
                // Response to the in-flight read / write / subscribe.
                if let Some(pending) = self.in_flight.take()
                    && !is_error
                {
                    match pending {
                        CentralOp::Read(h) if op == att_op::READ_RSP => {
                            self.values.insert(h, att[1..].to_vec());
                        }
                        CentralOp::Subscribe(h) => {
                            self.subscribed.insert(h);
                        }
                        CentralOp::Read(_) | CentralOp::Write(..) => {}
                    }
                }
            }
            CentralPhase::Connecting => {}
        }
    }

    /// Begin characteristic discovery on the first service (or finish).
    fn start_characteristic_discovery(&mut self, channel: &HciChannel) {
        if self.client.services.is_empty() {
            self.phase = CentralPhase::Ready;
        } else {
            self.phase = CentralPhase::DiscoveringCharacteristics(0);
            self.discover_characteristics_for(channel, 0);
        }
    }

    /// Move to the next service's characteristics (or finish).
    fn next_characteristic_service(&mut self, channel: &HciChannel, i: usize) {
        let next = i + 1;
        if next < self.client.services.len() {
            self.phase = CentralPhase::DiscoveringCharacteristics(next);
            self.discover_characteristics_for(channel, next);
        } else {
            self.phase = CentralPhase::Ready;
        }
    }

    /// Send the characteristic-discovery request for `services[i]`.
    fn discover_characteristics_for(&mut self, channel: &HciChannel, i: usize) {
        let handle = self.client.connection_handle;
        let svc = &self.client.services[i];
        let req = self
            .client
            .create_discover_characteristics_request(svc.start_handle, svc.end_handle);
        let _ = send_acl(channel, handle, &req);
    }

    /// The discovered GATT as JSON: `{connected, peer, phase, services:[…]}`.
    fn status_json(&self) -> String {
        #[derive(serde::Serialize)]
        struct View {
            connected: bool,
            peer: String,
            phase: &'static str,
            services: Vec<Svc>,
        }
        #[derive(serde::Serialize)]
        struct Svc {
            uuid: String,
            characteristics: Vec<Chr>,
        }
        #[derive(serde::Serialize)]
        struct Chr {
            uuid: String,
            value_handle: u16,
            properties: u8,
            /// Latest value (read or notified) as uppercase hex, if any.
            value: Option<String>,
            /// Whether notifications are enabled on this characteristic.
            subscribed: bool,
        }
        let hex = |v: &[u8]| v.iter().map(|b| format!("{b:02X}")).collect::<String>();
        let view = View {
            connected: self.client.connection_handle != 0,
            peer: self.target.to_string(),
            phase: self.phase.label(),
            services: self
                .client
                .services
                .iter()
                .map(|s| Svc {
                    uuid: s.uuid.to_string(),
                    characteristics: s
                        .characteristics
                        .iter()
                        .map(|c| Chr {
                            uuid: c.uuid.to_string(),
                            value_handle: c.value_handle,
                            properties: c.properties,
                            value: self.values.get(&c.value_handle).map(|v| hex(v)),
                            subscribed: self.subscribed.contains(&c.value_handle),
                        })
                        .collect(),
                })
                .collect(),
        };
        serde_json::to_string(&view).unwrap_or_else(|_| "{}".to_string())
    }
}

/// The role a device plays in a [`SceneEngine`].
enum SceneRole {
    /// A scripted GATT peripheral that advertises and serves. Boxed because a
    /// `ScriptedPeripheral` is much larger than the scanner variant.
    Peripheral(Box<ScriptedPeripheral>),
    /// A scanner accumulating the advertising reports it has seen.
    Scanner(Vec<ScanReport>),
    /// A central that connects to a peripheral and discovers its GATT.
    Central(Box<CentralDevice>),
}

/// One device in a scene: the controller-side [`HciChannel`] it shares with the
/// [`Link`], its role, and whether its HCI bring-up has been queued yet.
struct SceneDevice {
    channel: std::sync::Arc<HciChannel>,
    role: SceneRole,
    started: bool,
}

/// An in-process scene of Rhai devices sharing one [`Link`] — the browser's
/// "in-page controller" backend, and a native, netsim-free way to run many
/// devices together. Peripherals advertise and serve GATT; scanners collect
/// advertising reports; the shared [`Link`] routes between them. Transport-free
/// (no WebSocket, no netsim), so it runs identically native and on wasm32, and
/// a single page can host a whole scene.
pub struct SceneEngine {
    link: Link,
    devices: Vec<SceneDevice>,
}

impl Default for SceneEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl SceneEngine {
    /// Creates an empty scene.
    pub fn new() -> Self {
        Self {
            link: Link::new(),
            devices: Vec::new(),
        }
    }

    /// Adds a scripted peripheral at `address`; returns its device index (or the
    /// script error).
    pub fn add_peripheral(&mut self, address: Address, script: &str) -> Result<usize, String> {
        let mut peripheral = ScriptedPeripheral::run_script(script)?;
        peripheral.set_identity(address);
        let channel = self.link.add_device(address);
        let index = self.devices.len();
        self.devices.push(SceneDevice {
            channel,
            role: SceneRole::Peripheral(Box::new(peripheral)),
            started: false,
        });
        Ok(index)
    }

    /// Adds a scanner at `address`; returns its device index.
    pub fn add_scanner(&mut self, address: Address) -> usize {
        let channel = self.link.add_device(address);
        let index = self.devices.len();
        self.devices.push(SceneDevice {
            channel,
            role: SceneRole::Scanner(Vec::new()),
            started: false,
        });
        index
    }

    /// Adds a central at `address` that connects to and discovers the peripheral
    /// at `target`; returns its device index.
    pub fn add_central(&mut self, address: Address, target: Address) -> usize {
        let channel = self.link.add_device(address);
        let index = self.devices.len();
        self.devices.push(SceneDevice {
            channel,
            role: SceneRole::Central(Box::new(CentralDevice::new(target))),
            started: false,
        });
        index
    }

    /// The number of devices in the scene.
    pub fn device_count(&self) -> usize {
        self.devices.len()
    }

    /// Advances the whole scene one step at simulated time `t_seconds`: queues
    /// each device's bring-up on its first tick, lets peripherals run their
    /// scripts and emit notifications, routes advertising and data across the
    /// shared [`Link`], then delivers the results back to each device.
    pub fn tick(&mut self, t_seconds: f64) {
        for device in &mut self.devices {
            if !device.started {
                let _ = match &device.role {
                    SceneRole::Peripheral(p) => p.queue_start(&device.channel),
                    SceneRole::Scanner(_) => queue_scanner_start(&device.channel),
                    SceneRole::Central(_) => Ok(()),
                };
                device.started = true;
            }
        }
        // Devices produce (peripherals: script tick + notifications; centrals:
        // the connection request and discovery flow).
        for device in &mut self.devices {
            match &mut device.role {
                SceneRole::Peripheral(p) => {
                    if let Err(e) = p.tick(&device.channel, t_seconds) {
                        p.record_error(e.to_string());
                    }
                }
                SceneRole::Central(c) => c.produce(&device.channel),
                SceneRole::Scanner(_) => {}
            }
        }
        // Route across the shared medium.
        self.link.tick();
        // Consume the delivered events.
        for device in &mut self.devices {
            match &mut device.role {
                SceneRole::Peripheral(p) => {
                    while let Some(pkt) = device.channel.poll_controller_packet() {
                        if let Err(e) = p.handle_packet(&device.channel, &pkt) {
                            p.record_error(e.to_string());
                        }
                    }
                }
                SceneRole::Scanner(reports) => {
                    while let Some(pkt) = device.channel.poll_controller_packet() {
                        reports.extend(parse_scan_reports(&pkt));
                    }
                }
                SceneRole::Central(c) => {
                    while let Some(pkt) = device.channel.poll_controller_packet() {
                        c.consume(&device.channel, &pkt);
                    }
                }
            }
        }
    }

    /// The GATT status JSON of peripheral `index` (see
    /// [`ScriptedPeripheral::status_json`]), or `None` if it isn't a peripheral.
    pub fn peripheral_status_json(&self, index: usize) -> Option<String> {
        match self.devices.get(index)?.role {
            SceneRole::Peripheral(ref p) => Some(p.status_json()),
            SceneRole::Scanner(_) | SceneRole::Central(_) => None,
        }
    }

    /// The discovered-GATT JSON of central `index`, or `None` if it isn't a
    /// central.
    pub fn central_status_json(&self, index: usize) -> Option<String> {
        match self.devices.get(index)?.role {
            SceneRole::Central(ref c) => Some(c.status_json()),
            SceneRole::Peripheral(_) | SceneRole::Scanner(_) => None,
        }
    }

    /// Queue a read of `value_handle` on central `index`.
    pub fn central_read(&mut self, index: usize, value_handle: u16) {
        if let Some(SceneRole::Central(c)) = self.devices.get_mut(index).map(|d| &mut d.role) {
            c.queue_read(value_handle);
        }
    }

    /// Queue a write of `value` to `value_handle` on central `index`.
    pub fn central_write(&mut self, index: usize, value_handle: u16, value: Vec<u8>) {
        if let Some(SceneRole::Central(c)) = self.devices.get_mut(index).map(|d| &mut d.role) {
            c.queue_write(value_handle, value);
        }
    }

    /// Streams one isochronous SDU from central `index` to the peripheral it
    /// is connected to — the media plane a real LE Audio source drives.
    /// Returns false if the central has no connection yet.
    pub fn central_send_audio(&mut self, index: usize, sdu: &[u8]) -> bool {
        let Some(SceneRole::Central(central)) = self.devices.get(index).map(|d| &d.role) else {
            return false;
        };
        let handle = central.client.connection_handle;
        if handle == 0 {
            return false;
        }
        let sequence = central.audio_tx_sequence;
        if let Some(SceneRole::Central(central)) = self.devices.get_mut(index).map(|d| &mut d.role)
        {
            central.audio_tx_sequence = sequence.wrapping_add(1);
        }
        let packet = crate::packets::build_iso_packet(handle, sequence, sdu);
        let _ = self.devices[index].channel.inject_host_packet(packet);
        true
    }

    /// Drains the SDUs peripheral `index` has received, oldest first.
    pub fn peripheral_take_audio(&mut self, index: usize) -> Vec<Vec<u8>> {
        match self.devices.get_mut(index).map(|d| &mut d.role) {
            Some(SceneRole::Peripheral(p)) => p.take_audio(),
            _ => Vec::new(),
        }
    }

    /// Queue enabling notifications on `value_handle` for central `index`.
    pub fn central_subscribe(&mut self, index: usize, value_handle: u16) {
        if let Some(SceneRole::Central(c)) = self.devices.get_mut(index).map(|d| &mut d.role) {
            c.queue_subscribe(value_handle);
        }
    }

    /// Host-writes `value` into characteristic `uuid` of peripheral `index`
    /// (the in-page equivalent of `WebPeripheral::set_value`): updates the live
    /// GATT database and notifies any subscribed central.
    pub fn peripheral_set_value(
        &mut self,
        index: usize,
        uuid: &str,
        value: &[u8],
    ) -> Result<(), String> {
        match self.devices.get_mut(index).map(|d| &mut d.role) {
            Some(SceneRole::Peripheral(p)) => p.set_characteristic_value(uuid, value),
            _ => Err("not a peripheral".to_string()),
        }
    }

    /// The scan reports scanner `index` has collected as a JSON array, draining
    /// them so each call returns only what's new.
    pub fn scanner_reports_json(&mut self, index: usize) -> String {
        match self.devices.get_mut(index).map(|d| &mut d.role) {
            Some(SceneRole::Scanner(reports)) => {
                let json = serde_json::to_string(&reports).unwrap_or_else(|_| "[]".to_string());
                reports.clear();
                json
            }
            _ => "[]".to_string(),
        }
    }
}

#[cfg(test)]
mod scene_tests {
    use super::*;

    #[test]
    fn test_ws_url_carries_the_wire_byte_order() {
        // netsim reads the URL address LSB-first, so a page writing display
        // order would advertise the address reversed and be unreachable.
        let url = "ws://localhost:7681/v1/websocket/bt?name=web-speaker&address=CC:1E:57:00:00:06";
        assert_eq!(
            ws_url_with_wire_address(url),
            "ws://localhost:7681/v1/websocket/bt?name=web-speaker&address=06:00:00:57:1E:CC"
        );
        // The identity stays the address the page asked for.
        assert_eq!(
            address_from_ws_url(url),
            Some("CC:1E:57:00:00:06".parse().unwrap())
        );
        // A URL without an address is passed through untouched.
        let plain = "ws://localhost:7681/v1/websocket/bt?name=x";
        assert_eq!(ws_url_with_wire_address(plain), plain);
    }

    #[test]
    fn test_address_is_parsed_from_the_netsim_url() {
        // The browser path stamps identity from the WebSocket URL; without it
        // a page-hosted device pairs with the script engine's placeholder
        // address and a real stack rejects the pairing.
        let url = "ws://localhost:7681/v1/websocket/bt?name=web-speaker&address=CC:1E:57:00:00:06";
        assert_eq!(
            address_from_ws_url(url),
            Some("CC:1E:57:00:00:06".parse().unwrap())
        );
        // Order-independent, and absent means absent.
        assert_eq!(
            address_from_ws_url("ws://h/p?address=AA:BB:CC:00:00:01&name=x"),
            Some("AA:BB:CC:00:00:01".parse().unwrap())
        );
        assert_eq!(address_from_ws_url("ws://h/p?name=x"), None);
    }

    #[test]
    fn test_scene_stamps_on_air_identity_onto_the_device() {
        // The script engine allocates placeholder addresses (every session
        // starts at :01), but SMP computes with device.address — the scene
        // must overwrite it with the address actually on the air.
        let mut scene = SceneEngine::new();
        let script = r#"
            let server = android::BluetoothGattServer("A");
            let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
            bas.add_characteristic(android::BluetoothGattCharacteristic(
                uuid::BATTERY_LEVEL, android::PROPERTY_READ, android::PERMISSION_READ));
            server.add_service(bas);
        "#;
        scene
            .add_peripheral("AA:BB:CC:00:00:07".parse().unwrap(), script)
            .unwrap();
        let status = scene.peripheral_status_json(0).unwrap();
        assert!(
            status.contains("AA:BB:CC:00:00:07"),
            "device should carry its on-air address: {status}"
        );
    }

    #[test]
    fn test_connection_complete_records_peer_address_type() {
        // Byte 8 of LE Connection Complete is the peer address type; SMP
        // mixes it into pairing crypto, so it must reach the connection.
        let mut scene = SceneEngine::new();
        let script = r#"
            let server = android::BluetoothGattServer("B");
            let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
            bas.add_characteristic(android::BluetoothGattCharacteristic(
                uuid::BATTERY_LEVEL, android::PROPERTY_READ, android::PERMISSION_READ));
            server.add_service(bas);
        "#;
        let index = scene
            .add_peripheral("AA:BB:CC:00:00:08".parse().unwrap(), script)
            .unwrap();
        scene.tick(0.1); // bring-up

        // LE Connection Complete with peer type 0x00 (public).
        let mut event = vec![
            0x04, 0x3E, 0x13, 0x01, 0x00, 0x40, 0x00, 0x01, 0x00,
        ];
        event.extend_from_slice(&[0xB9, 0x62, 0xF7, 0xD6, 0x79, 0x7C]); // peer LE
        event.extend_from_slice(&[0x18, 0x00, 0x00, 0x00, 0x48, 0x00, 0x00]);
        let channel = scene.devices[index].channel.clone();
        let SceneRole::Peripheral(p) = &mut scene.devices[index].role else {
            panic!("expected peripheral");
        };
        p.handle_packet(&channel, &event).unwrap();
        p.primary().with_server(|s| {
            let conn = s.device.connections.get(&0x0040).expect("connected");
            assert_eq!(conn.peer_address_type, AddressType::Public);
        });
    }

    #[test]
    fn test_ltk_request_event_gets_a_reply_with_the_session_key() {
        // Encryption start over a real controller: the LE Long Term Key
        // Request event must be answered with the key SMP recorded on the
        // connection, or pairing fails on the peer.
        let mut scene = SceneEngine::new();
        let script = r#"
            let server = android::BluetoothGattServer("C");
            let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
            bas.add_characteristic(android::BluetoothGattCharacteristic(
                uuid::BATTERY_LEVEL, android::PROPERTY_READ, android::PERMISSION_READ));
            server.add_service(bas);
        "#;
        let index = scene
            .add_peripheral("AA:BB:CC:00:00:09".parse().unwrap(), script)
            .unwrap();
        scene.tick(0.1); // bring-up

        let channel = scene.devices[index].channel.clone();
        while channel.poll_host_packet().is_some() {} // drain bring-up

        let SceneRole::Peripheral(p) = &mut scene.devices[index].role else {
            panic!("expected peripheral");
        };
        // Connect, then plant the key SMP would have recorded.
        let mut connect = vec![0x04, 0x3E, 0x13, 0x01, 0x00, 0x40, 0x00, 0x01, 0x01];
        connect.extend_from_slice(&[0x11; 6]);
        connect.extend_from_slice(&[0x18, 0x00, 0x00, 0x00, 0x48, 0x00, 0x00]);
        p.handle_packet(&channel, &connect).unwrap();
        let key = [0xAB; 16];
        p.primary().with_server(|s| {
            s.device.connections.get_mut(&0x0040).unwrap().ltk = Some(key);
        });

        // LE Long Term Key Request: subevent 0x05, handle, rand(8), ediv(2).
        let mut event = vec![0x04, 0x3E, 0x0D, 0x05, 0x40, 0x00];
        event.extend_from_slice(&[0x00; 10]);
        p.handle_packet(&channel, &event).unwrap();

        let reply = channel.poll_host_packet().expect("a reply must be queued");
        assert_eq!(&reply[..3], &[0x01, 0x1A, 0x20], "LTK Request Reply");
        assert_eq!(&reply[6..22], &key, "carrying the session key");
    }

    #[test]
    fn test_scanner_hears_script_staged_advertising_extras() {
        // The beacon idiom: a script stages service data + manufacturer data
        // and the on-air advertisement must carry both to a scanner.
        let mut scene = SceneEngine::new();
        let script = r#"
            let server = android::BluetoothGattServer("Beacon");
            let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
            let level = android::BluetoothGattCharacteristic(uuid::BATTERY_LEVEL,
                android::PROPERTY_READ, android::PERMISSION_READ);
            level.set_value([88]);
            bas.add_characteristic(level);
            server.add_service(bas);
            server.advertise_service_data(0xFE2C, [0x00, 0x11, 0x22]);
            server.advertise_manufacturer_data(0x00E0, [0x01]);
        "#;
        scene
            .add_peripheral("AA:BB:CC:00:00:01".parse().unwrap(), script)
            .unwrap();
        let scanner = scene.add_scanner("AA:BB:CC:00:00:02".parse().unwrap());
        for _ in 0..3 {
            scene.tick(0.1);
        }

        let reports = scene.scanner_reports_json(scanner);
        assert!(reports.contains("fe2c") || reports.contains("FE2C"), "{reports}");
        assert!(
            reports.contains("001122") || reports.contains("[0,17,34]"),
            "service data bytes should be on the air: {reports}"
        );
        assert!(
            reports.contains("00E0"),
            "manufacturer company id should be on the air: {reports}"
        );
    }

    /// `add_pacs` / `add_ascs` call the Rust profile registrars, which write
    /// into the GATT database rather than the script's service list — so
    /// nothing in the script surface proves they landed. Read them back.
    /// `push_event` + `on_event` are the host→script event path; the
    /// dispatch had no test, so a script could stop receiving events
    /// without anything noticing.
    #[test]
    fn test_pushed_events_reach_the_script_handler() {
        let script = r#"
            let server = android::BluetoothGattServer("evt");
            let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
            let level = android::BluetoothGattCharacteristic(uuid::BATTERY_LEVEL,
                android::PROPERTY_READ, android::PERMISSION_READ);
            level.set_value([100]);
            bas.add_characteristic(level);
            server.add_service(bas);
            // `this` is the persistent state map bound by the runtime.
            fn on_event(server, event) {
                if event.event == "ui" && event.action == "drain" {
                    let level = server.value(uuid::BATTERY_LEVEL)[0];
                    server.update_value(uuid::BATTERY_LEVEL, [level - event.amount]);
                    server.emit("drained", #{ to: level - event.amount });
                }
            }
        "#;
        let mut peripheral = ScriptedPeripheral::run_script(script).unwrap();
        let channel = HciChannel::new();

        peripheral.push_event("ui", r#"{"action":"drain","amount":7}"#);
        peripheral.tick(&channel, 0.1).unwrap();

        let level = peripheral
            .primary()
            .with_server(|s| s.device.gatt_db.value_handle_for_uuid(crate::types::Uuid::Uuid16(0x2A19))
                .and_then(|h| s.device.gatt_db.value(h).map(|v| v.to_vec())))
            .unwrap();
        assert_eq!(level, vec![93], "the handler ran and applied the payload");

        let emitted = peripheral.take_emitted();
        assert_eq!(emitted.len(), 1, "the script's emit reached the host");
        assert!(
            emitted[0].contains("\"drained\"") && emitted[0].contains("93"),
            "emitted payload: {}",
            emitted[0]
        );
        assert!(peripheral.take_emitted().is_empty(), "draining is destructive");
    }

    #[test]
    fn test_profile_registrar_bindings_build_real_services() {
        let mut scene = SceneEngine::new();
        let script = r#"
            let server = android::BluetoothGattServer("LEA");
            server.add_pacs(0x03, 0x00);
            server.add_ascs([0x01], []);
            server.add_ras();
        "#;
        let index = scene
            .add_peripheral("AA:BB:CC:00:00:51".parse().unwrap(), script)
            .unwrap();
        let status = scene.peripheral_status_json(index).unwrap();
        // The registrars write to the database; check there, not in status.
        let handles: Vec<u16> = [0x2BC9, 0x2BC4, 0x2BC6, 0x2B70]
            .iter()
            .map(|&uuid| {
                let SceneRole::Peripheral(p) = &scene.devices[index].role else {
                    panic!("expected a peripheral");
                };
                p.primary().with_server(|s| {
                    s.device
                        .gatt_db
                        .value_handle_for_uuid(crate::types::Uuid::Uuid16(uuid))
                        .unwrap_or_else(|| panic!("characteristic {uuid:#06X} missing"))
                })
            })
            .collect();
        assert_eq!(handles.len(), 4, "sink PAC, sink ASE, ASE CP, ranging data");
        assert!(!status.is_empty());
    }

    /// `send_audio` and `take_audio` are the script's half of the media
    /// plane; neither had a test.
    #[test]
    fn test_script_can_send_and_receive_audio_sdus() {
        let mut scene = SceneEngine::new();
        let sink_script = r#"
            let server = android::BluetoothGattServer("sink");
            let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
            bas.add_characteristic(android::BluetoothGattCharacteristic(
                uuid::BATTERY_LEVEL, android::PROPERTY_READ, android::PERMISSION_READ));
            server.add_service(bas);
            // Drain what arrived and record how much, so a test can see it.
            fn tick(server, t) {
                let frames = server.take_audio();
                if frames.len() > 0 {
                    server.update_value(uuid::BATTERY_LEVEL, [frames.len()]);
                }
            }
        "#;
        let sink = scene
            .add_peripheral("AA:BB:CC:00:00:52".parse().unwrap(), sink_script)
            .unwrap();
        let source = scene.add_central(
            "AA:BB:CC:00:00:53".parse().unwrap(),
            "AA:BB:CC:00:00:52".parse().unwrap(),
        );
        for _ in 0..40 {
            scene.tick(0.02);
        }
        for _ in 0..3 {
            assert!(scene.central_send_audio(source, &[0xAA; 8]));
            scene.tick(0.02);
        }
        scene.tick(0.02);
        let status = scene.peripheral_status_json(sink).unwrap();
        // The script drained the SDUs and wrote the count into the battery
        // level, so a non-zero value proves take_audio saw them.
        assert!(
            !status.contains("\"value\": \"00\""),
            "the script's take_audio should have seen SDUs: {status}"
        );
    }

    #[test]
    fn test_script_staged_service_uuids_reach_the_advertisement() {
        // A service built by a Rust profile registrar exists only in the
        // GATT database, so the script stages its UUID explicitly. That
        // staging used to be dropped, leaving the device advertising no
        // services — invisible to a scanner filtering on one.
        let mut scene = SceneEngine::new();
        let script = r#"
            let server = android::BluetoothGattServer("Ranger");
            server.add_ras();
            server.advertise_service_uuid(0x185B);
        "#;
        scene
            .add_peripheral("AA:BB:CC:00:00:41".parse().unwrap(), script)
            .unwrap();
        let scanner = scene.add_scanner("AA:BB:CC:00:00:42".parse().unwrap());
        for _ in 0..3 {
            scene.tick(0.1);
        }
        let reports = scene.scanner_reports_json(scanner);
        assert!(
            reports.contains("185B"),
            "the staged service UUID must be on the air: {reports}"
        );
    }

    #[test]
    fn test_beacon_advertises_non_connectable() {
        // advertise_connectable(false) must put ADV_NONCONN_IND (0x03) in the
        // LE Set Advertising Parameters and send no scan-response command.
        let mut scene = SceneEngine::new();
        let script = r#"
            let server = android::BluetoothGattServer("Beacon");
            let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
            bas.add_characteristic(android::BluetoothGattCharacteristic(
                uuid::BATTERY_LEVEL, android::PROPERTY_READ, android::PERMISSION_READ));
            server.add_service(bas);
            server.advertise_manufacturer_data(0x004C, [0x02, 0x15]);
            server.advertise_connectable(false);
        "#;
        let _ = &mut scene;
        // Drive queue_start on a standalone channel so the bring-up commands
        // are inspectable (a live scene's link would consume them).
        let mut p = ScriptedPeripheral::run_script(script).unwrap();
        p.set_identity("AA:BB:CC:00:00:0A".parse().unwrap());
        let channel = HciChannel::new();
        p.queue_start(&channel).unwrap();
        let mut adv_type = None;
        let mut saw_scan_rsp = false;
        while let Some(pkt) = channel.poll_host_packet() {
            // H4 command (0x01), opcode LE Set Advertising Parameters 0x2006;
            // params start at index 3, advertising type is the 5th param byte.
            if pkt.len() >= 9 && pkt[0] == 0x01 && pkt[1] == 0x06 && pkt[2] == 0x20 {
                // [0..4]=H4/opcode/len, [4..8]=interval min+max, [8]=adv type.
                adv_type = Some(pkt[8]);
            }
            if pkt.len() >= 3 && pkt[0] == 0x01 && pkt[1] == 0x09 && pkt[2] == 0x20 {
                saw_scan_rsp = true;
            }
        }
        assert_eq!(adv_type, Some(0x03), "should be ADV_NONCONN_IND");
        assert!(!saw_scan_rsp, "a non-connectable beacon sends no scan response");
    }

    #[test]
    fn test_large_service_data_drops_name_to_fit() {
        // A 24-byte service-data payload (Quick Share nudge) fills the packet;
        // the builder must drop the name entirely, not emit a stub, and not
        // reject the advertisement.
        let mut extras = AdvertisingData::new();
        extras.service_data_16.push((0xFE2C, vec![0xAB; 24]));
        let payload = build_adv_payload_with_extras("QuickShare", &[], Some(&extras))
            .expect("nameless beacon must fit in 31 bytes");
        assert!(payload.len() <= 31, "len {}", payload.len());
        // Service data present…
        assert!(payload.windows(2).any(|w| w == [0x2C, 0xFE]));
        // …and no Complete Local Name AD type (0x09).
        assert!(!payload.contains(&0x09), "name should be absent: {payload:?}");
    }

    #[test]
    fn test_indicate_subscription_sends_indications_not_notifications() {
        // CCCD bit 1 is Indicate. The flush path used to send a notification
        // whatever the client asked for and to ignore bit 1 entirely, so an
        // Indicate-only characteristic (several SIG profiles mandate one)
        // delivered nothing at all.
        let script = r#"
            let server = android::BluetoothGattServer("ind");
            let svc = android::BluetoothGattService(uuid::from_u16(0x181D), android::SERVICE_TYPE_PRIMARY);
            let ch = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A9D),
                android::PROPERTY_READ | android::PROPERTY_INDICATE, android::PERMISSION_READ);
            ch.set_value([0x00, 0x01]);
            ch.add_descriptor(android::BluetoothGattDescriptor(
                uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
                android::PERMISSION_READ | android::PERMISSION_WRITE));
            svc.add_characteristic(ch);
            server.add_service(svc);
            fn tick(server, t) {
                server.update_value(uuid::from_u16(0x2A9D), [0x00, 2 + t.to_int() % 5]);
            }
        "#;
        let mut peripheral = ScriptedPeripheral::run_script(script).unwrap();
        let channel = HciChannel::new();
        let drain = |channel: &HciChannel| {
            let mut out = Vec::new();
            while let Some(p) = channel.poll_host_packet() {
                out.push(p);
            }
            out
        };
        peripheral.queue_start(&channel).unwrap();
        let _ = drain(&channel);

        let mut connect = vec![0x04, 0x3E, 0x13, 0x01, 0x00, 0x40, 0x00, 0x01, 0x00];
        connect.extend_from_slice(&[0x11; 6]);
        connect.extend_from_slice(&[0x18, 0x00, 0x00, 0x00, 0x48, 0x00, 0x00]);
        peripheral.handle_packet(&channel, &connect).unwrap();
        let _ = drain(&channel);

        // Subscribe for indications (CCCD = 0x0002), not notifications.
        let watch = peripheral.watched[0].clone();
        let cccd = watch.cccd_handle.expect("characteristic declares a CCCD");
        peripheral.primary().with_server(|s| {
            let _ = s.device.gatt_db.set_value(cccd, &[0x02, 0x00]);
        });
        assert_eq!(
            peripheral.cccd_subscription(&watch),
            CccdSubscription::Indicate
        );

        peripheral.tick(&channel, 1.0).unwrap();
        let packets = drain(&channel);
        let att_opcodes: Vec<u8> = packets
            .iter()
            .filter(|p| p.len() > 9)
            .map(|p| p[9])
            .collect();
        assert!(
            att_opcodes.contains(&0x1D),
            "an Indicate subscriber must get ATT Handle Value Indication (0x1D), got {att_opcodes:02X?}"
        );
        assert!(
            !att_opcodes.contains(&0x1B),
            "and not a notification (0x1B)"
        );
    }

    #[test]
    fn test_central_starts_discovery_on_either_connection_complete() {
        // The central used to test raw bytes for subevent 0x01 only, so a
        // controller reporting the Enhanced variant (0x0A) — what an
        // address-resolving stack sends — never started discovery. Both
        // must work now that it shares the peripheral's typed parser.
        for subevent in [0x01u8, 0x0A] {
            let mut central = CentralDevice::new("AA:BB:CC:00:00:31".parse().unwrap());
            let channel = HciChannel::new();
            let mut event = vec![0x04, 0x3E, 0x13, subevent, 0x00, 0x40, 0x00, 0x01, 0x00];
            event.extend_from_slice(&[0x11; 6]);
            event.extend_from_slice(&[0x18, 0x00, 0x00, 0x00, 0x48, 0x00, 0x00]);
            central.consume(&channel, &event);
            assert_eq!(
                central.phase,
                CentralPhase::ExchangingMtu,
                "subevent {subevent:#04X} should start discovery"
            );
            assert!(
                channel.poll_host_packet().is_some(),
                "an MTU exchange request must go out"
            );
        }

        // A failed connection starts nothing.
        let mut central = CentralDevice::new("AA:BB:CC:00:00:31".parse().unwrap());
        let channel = HciChannel::new();
        let mut failed = vec![0x04, 0x3E, 0x13, 0x01, 0x02, 0x40, 0x00, 0x01, 0x00];
        failed.extend_from_slice(&[0x11; 6]);
        failed.extend_from_slice(&[0x18, 0x00, 0x00, 0x00, 0x48, 0x00, 0x00]);
        central.consume(&channel, &failed);
        assert_ne!(central.phase, CentralPhase::ExchangingMtu);
    }

    #[test]
    fn test_audio_streams_from_central_to_peripheral() {
        // The media plane end to end: a central streams isochronous SDUs over
        // the simulated radio and the peripheral receives them in order.
        let mut scene = SceneEngine::new();
        let script = r#"
            let server = android::BluetoothGattServer("sink");
            let vcs = android::BluetoothGattService(uuid::VOLUME_CONTROL_SERVICE, android::SERVICE_TYPE_PRIMARY);
            vcs.add_characteristic(android::BluetoothGattCharacteristic(
                uuid::VOLUME_STATE, android::PROPERTY_READ, android::PERMISSION_READ));
            server.add_service(vcs);
        "#;
        let sink = scene
            .add_peripheral("AA:BB:CC:00:00:20".parse().unwrap(), script)
            .unwrap();
        let source = scene.add_central(
            "AA:BB:CC:00:00:21".parse().unwrap(),
            "AA:BB:CC:00:00:20".parse().unwrap(),
        );
        // Let the connection come up.
        for _ in 0..40 {
            scene.tick(0.02);
        }

        // Three "codec frames" worth of audio.
        let frames: Vec<Vec<u8>> = (0u8..3).map(|i| vec![i; 8]).collect();
        for frame in &frames {
            assert!(
                scene.central_send_audio(source, frame),
                "the central must have a connection to stream over"
            );
            scene.tick(0.02);
        }
        scene.tick(0.02);

        let received = scene.peripheral_take_audio(sink);
        assert_eq!(received, frames, "every SDU arrives, in order");
        // Draining is destructive: a second take sees nothing new.
        assert!(scene.peripheral_take_audio(sink).is_empty());
    }

    #[test]
    fn test_scene_scanner_sees_scripted_peripheral() {
        let mut scene = SceneEngine::new();
        let script = r#"
            let server = android::BluetoothGattServer("SceneHRM");
            let hrs = android::BluetoothGattService(uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY);
            let hr = android::BluetoothGattCharacteristic(
                uuid::HEART_RATE_MEASUREMENT,
                android::PROPERTY_READ | android::PROPERTY_NOTIFY,
                android::PERMISSION_READ,
            );
            hr.set_value([0x00, 72]);
            hrs.add_characteristic(hr);
            server.add_service(hrs);
        "#;
        scene
            .add_peripheral("AA:BB:CC:00:00:01".parse().unwrap(), script)
            .unwrap();
        let scanner = scene.add_scanner("AA:BB:CC:00:00:02".parse().unwrap());
        assert_eq!(scene.device_count(), 2);

        // A few ticks: bring-up, advertise, route.
        for _ in 0..3 {
            scene.tick(0.1);
        }

        let reports = scene.scanner_reports_json(scanner);
        assert!(
            reports.contains("SceneHRM"),
            "scanner should have seen the peripheral by name; got {reports}"
        );
        // The peripheral's own GATT status is available for a server view.
        assert!(
            scene
                .peripheral_status_json(0)
                .unwrap()
                .contains("SceneHRM")
        );
    }

    #[test]
    fn test_central_connects_and_discovers_peripheral() {
        let mut scene = SceneEngine::new();
        let script = r#"
            let server = android::BluetoothGattServer("SceneHRM");
            let hrs = android::BluetoothGattService(uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY);
            let hr = android::BluetoothGattCharacteristic(
                uuid::HEART_RATE_MEASUREMENT,
                android::PROPERTY_READ | android::PROPERTY_NOTIFY,
                android::PERMISSION_READ,
            );
            hr.set_value([0x00, 72]);
            hrs.add_characteristic(hr);
            server.add_service(hrs);
        "#;
        let peripheral_addr = "AA:BB:CC:00:00:01".parse().unwrap();
        scene.add_peripheral(peripheral_addr, script).unwrap();
        let central = scene.add_central("AA:BB:CC:00:00:02".parse().unwrap(), peripheral_addr);

        // Connect + MTU + service/characteristic discovery is a handful of
        // round-trips; each takes a couple of ticks through the Link.
        for _ in 0..40 {
            scene.tick(0.1);
        }

        let json = scene.central_status_json(central).unwrap();
        // The central discovered the Heart Rate service and its measurement
        // characteristic on the peer — two real devices, connected in-process.
        assert!(
            json.contains("\"connected\":true"),
            "central should be connected; got {json}"
        );
        assert!(
            json.contains("180D"),
            "should discover Heart Rate service; got {json}"
        );
        assert!(
            json.contains("2A37"),
            "should discover HR Measurement char; got {json}"
        );
    }

    #[test]
    fn test_run_test_script_pass_fail_and_compile_error() {
        // A passing assertion.
        assert!(
            run_test_script(
                "let s = android::BluetoothGattServer(\"t\"); assert(s.name == \"t\", \"name\");"
            )
            .is_ok()
        );
        // A failing assertion surfaces its message.
        let err = run_test_script("assert(1 == 2, \"one is not two\");").unwrap_err();
        assert!(
            err.contains("one is not two") || err.to_lowercase().contains("assert"),
            "got {err}"
        );
        // A compile error is reported as such.
        assert!(run_test_script("@@ not rhai @@").is_err());
    }
}

#[cfg(target_arch = "wasm32")]
mod web {
    //! The browser half: `web_sys::WebSocket` pumping and the wasm-bindgen
    //! types the demo pages instantiate. Each `tick()` call from the page's
    //! JS interval pumps both directions once and returns render-ready JSON.

    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;
    use std::sync::Once;

    use wasm_bindgen::JsCast;
    use wasm_bindgen::prelude::*;
    use web_sys::{BinaryType, MessageEvent, WebSocket};

    use super::super::hci_adapter::HciChannel;
    use super::{
        DEFAULT_HEART_RATE_SCRIPT, ScriptedPeripheral, address_from_ws_url, parse_scan_reports,
        queue_advertiser_start, queue_scanner_start, ws_url_with_wire_address,
    };
    use crate::types::Address;

    /// Panics otherwise vanish into an opaque `unreachable` trap; route them
    /// to the browser console instead.
    fn install_panic_hook() {
        static HOOK: Once = Once::new();
        HOOK.call_once(|| {
            std::panic::set_hook(Box::new(|info| {
                web_sys::console::error_1(&JsValue::from_str(&info.to_string()));
            }));
        });
    }

    fn js_error(message: impl std::fmt::Display) -> JsValue {
        JsValue::from_str(&message.to_string())
    }

    /// The wasm sibling of `NetsimTransport`: same pump shape (drain
    /// `HciChannel` host packets to the socket, drain received messages into
    /// the channel), but the browser owns all WebSocket framing, and receipt
    /// is event-driven — `onmessage` queues packets for the next pump.
    struct WasmWsTransport {
        ws: WebSocket,
        inbound: Rc<RefCell<VecDeque<Vec<u8>>>>,
        _on_message: Closure<dyn FnMut(MessageEvent)>,
    }

    impl WasmWsTransport {
        fn connect(url: &str) -> Result<Self, JsValue> {
            let ws = WebSocket::new(url)?;
            ws.set_binary_type(BinaryType::Arraybuffer);
            let inbound: Rc<RefCell<VecDeque<Vec<u8>>>> = Rc::default();
            let queue = inbound.clone();
            let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
                if let Ok(buffer) = event.data().dyn_into::<js_sys::ArrayBuffer>() {
                    queue
                        .borrow_mut()
                        .push_back(js_sys::Uint8Array::new(&buffer).to_vec());
                }
            });
            ws.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
            Ok(Self {
                ws,
                inbound,
                _on_message: on_message,
            })
        }

        fn ready_state(&self) -> u16 {
            self.ws.ready_state()
        }

        fn is_open(&self) -> bool {
            self.ready_state() == WebSocket::OPEN
        }

        fn pump(&self, channel: &HciChannel) -> Result<(), JsValue> {
            if self.is_open() {
                while let Some(packet) = channel.poll_host_packet() {
                    self.ws.send_with_u8_array(&packet)?;
                }
            }
            loop {
                let next = self.inbound.borrow_mut().pop_front();
                match next {
                    Some(packet) => channel.receive_from_controller(packet).map_err(js_error)?,
                    None => break,
                }
            }
            Ok(())
        }
    }

    impl Drop for WasmWsTransport {
        fn drop(&mut self) {
            self.ws.set_onmessage(None);
            let _ = self.ws.close();
        }
    }

    /// The scanner page's engine: joins netsim as a scanning device and
    /// returns decoded advertising reports as JSON on every tick.
    #[wasm_bindgen]
    pub struct WebScanner {
        transport: WasmWsTransport,
        channel: HciChannel,
        started: bool,
    }

    #[wasm_bindgen]
    impl WebScanner {
        #[wasm_bindgen(constructor)]
        pub fn new(url: &str) -> Result<WebScanner, JsValue> {
            install_panic_hook();
            Ok(Self {
                transport: WasmWsTransport::connect(url)?,
                channel: HciChannel::new(),
                started: false,
            })
        }

        /// 0 = connecting, 1 = open, 2 = closing, 3 = closed — the page's
        /// connection-failure UX keys off this.
        /// Returns the underlying WebSocket ready state (per the WebSocket API).
        pub fn ready_state(&self) -> u16 {
            self.transport.ready_state()
        }

        /// One pump: returns a JSON array of decoded advertising reports
        /// (possibly empty).
        pub fn tick(&mut self) -> Result<String, JsValue> {
            self.transport.pump(&self.channel)?;
            if !self.started && self.transport.is_open() {
                queue_scanner_start(&self.channel).map_err(js_error)?;
                self.started = true;
                self.transport.pump(&self.channel)?;
            }
            let mut reports = Vec::new();
            while let Some(packet) = self.channel.poll_controller_packet() {
                reports.extend(parse_scan_reports(&packet));
            }
            serde_json::to_string(&reports).map_err(js_error)
        }
    }

    /// The **in-page controller** backend: a whole scene of scripted devices
    /// sharing one in-process [`Link`](crate::controller::sim::Link), with no
    /// WebSocket and no netsim. Add peripherals and scanners, `tick()` on a
    /// LC3 for the demo pages: encode PCM into the frames a source puts on
    /// the air, decode the frames a sink received so the page can play
    /// them. Behind the `lc3` feature — the pages build enables it, the
    /// `simble mcp` binary does not need it (SDUs are opaque to the
    /// protocol layers).
    #[cfg(feature = "lc3")]
    #[wasm_bindgen]
    pub struct WebLc3 {
        // The stream configuration lives inside these two now: both are
        // stateful across frames and are built for one configuration.
        encoder: crate::audio::lc3::Lc3Encode,
        decoder: crate::audio::lc3::Lc3Stream,
    }

    #[cfg(feature = "lc3")]
    #[wasm_bindgen]
    impl WebLc3 {
        /// Creates a codec for one stream's configuration — the same values
        /// the ASE was configured with (16 kHz / 10 ms is what simble's PAC
        /// record advertises).
        #[wasm_bindgen(constructor)]
        pub fn new(sample_rate_hz: u32, frame_duration_us: u32) -> Result<WebLc3, JsValue> {
            Ok(Self {
                encoder: crate::audio::lc3::Lc3Encode::new(sample_rate_hz, frame_duration_us)
                    .map_err(js_error)?,
                decoder: crate::audio::lc3::Lc3Stream::new(sample_rate_hz, frame_duration_us)
                    .map_err(js_error)?,
            })
        }

        /// PCM samples per frame, so the page knows how much audio to hand
        /// over and how much to expect back.
        pub fn samples_per_frame(&self) -> usize {
            self.encoder.samples_per_frame()
        }

        /// Encodes one frame of 16-bit PCM into `frame_bytes` of LC3.
        pub fn encode(&mut self, samples: Vec<i16>, frame_bytes: usize) -> Result<Vec<u8>, JsValue> {
            self.encoder
                .encode(&samples, frame_bytes)
                .map_err(js_error)
        }

        /// Decodes one LC3 frame back to 16-bit PCM.
        pub fn decode(&mut self, frame: Vec<u8>) -> Result<Vec<i16>, JsValue> {
            self.decoder
                .decode(&frame)
                .map_err(js_error)
        }
    }

    /// timer, and read each device's state — the browser pages use this when
    /// the backend selector is set to "in-page". Wraps [`SceneEngine`].
    #[wasm_bindgen]
    pub struct WebLink {
        scene: super::SceneEngine,
    }

    #[wasm_bindgen]
    impl WebLink {
        /// Creates an empty in-page scene.
        #[wasm_bindgen(constructor)]
        pub fn new() -> WebLink {
            install_panic_hook();
            Self {
                scene: super::SceneEngine::new(),
            }
        }

        /// Adds a scripted peripheral at `address` (e.g. `"AA:BB:CC:00:00:01"`);
        /// returns its device index, or the script/address error.
        pub fn add_peripheral(&mut self, address: &str, script: &str) -> Result<usize, JsValue> {
            let address = address.parse().map_err(js_error)?;
            self.scene.add_peripheral(address, script).map_err(js_error)
        }

        /// Adds a scanner at `address`; returns its device index.
        pub fn add_scanner(&mut self, address: &str) -> Result<usize, JsValue> {
            let address = address.parse().map_err(js_error)?;
            Ok(self.scene.add_scanner(address))
        }

        /// Adds a central at `address` that connects to and discovers the
        /// peripheral at `target`; returns its device index.
        pub fn add_central(&mut self, address: &str, target: &str) -> Result<usize, JsValue> {
            let address = address.parse().map_err(js_error)?;
            let target = target.parse().map_err(js_error)?;
            Ok(self.scene.add_central(address, target))
        }

        /// The discovered-GATT JSON of central `index` (`undefined` if not a
        /// central).
        pub fn central_status_json(&self, index: usize) -> Option<String> {
            self.scene.central_status_json(index)
        }

        /// Queue a read of `value_handle` on central `index`.
        pub fn central_read(&mut self, index: usize, value_handle: u16) {
            self.scene.central_read(index, value_handle);
        }

        /// Queue a write of `value` to `value_handle` on central `index`.
        pub fn central_write(&mut self, index: usize, value_handle: u16, value: Vec<u8>) {
            self.scene.central_write(index, value_handle, value);
        }

    /// Queue enabling notifications on `value_handle` for central `index`.
        pub fn central_subscribe(&mut self, index: usize, value_handle: u16) {
            self.scene.central_subscribe(index, value_handle);
        }

        /// Host-write `value` into characteristic `uuid` of peripheral `index`
        /// (updates the live GATT database and notifies subscribers).
        pub fn peripheral_set_value(
            &mut self,
            index: usize,
            uuid: &str,
            value: Vec<u8>,
        ) -> Result<(), JsValue> {
            self.scene
                .peripheral_set_value(index, uuid, &value)
                .map_err(js_error)
        }

        /// The number of devices in the scene.
        /// Streams one isochronous SDU (a codec frame's worth of audio) from
        /// central `index` to the peripheral it is connected to.
        pub fn central_send_audio(&mut self, index: usize, sdu: Vec<u8>) -> bool {
            self.scene.central_send_audio(index, &sdu)
        }

        /// Drains the SDUs peripheral `index` has received, as an array of
        /// byte arrays — what the page feeds to its audio output.
        pub fn peripheral_take_audio(&mut self, index: usize) -> js_sys::Array {
            let out = js_sys::Array::new();
            for sdu in self.scene.peripheral_take_audio(index) {
                out.push(&js_sys::Uint8Array::from(&sdu[..]).into());
            }
            out
        }

        pub fn device_count(&self) -> usize {
            self.scene.device_count()
        }

        /// Advances the whole scene one step at simulated time `t_seconds`.
        pub fn tick(&mut self, t_seconds: f64) {
            self.scene.tick(t_seconds);
        }

        /// The GATT status JSON of peripheral `index`, or `undefined` if that
        /// device isn't a peripheral.
        pub fn peripheral_status_json(&self, index: usize) -> Option<String> {
            self.scene.peripheral_status_json(index)
        }

        /// New scan reports for scanner `index` as a JSON array (drained on read).
        pub fn scanner_reports_json(&mut self, index: usize) -> String {
            self.scene.scanner_reports_json(index)
        }
    }

    /// A lightweight, advertise-only device the scanner page spins up to
    /// populate an otherwise-empty netsim scene (no GATT server, no script —
    /// just a name, an optional 16-bit service UUID, and optional manufacturer
    /// data on the air). Several of these run on their own sockets so the
    /// scanner demos something on first open.
    #[wasm_bindgen]
    pub struct WebAdvertiser {
        transport: WasmWsTransport,
        channel: HciChannel,
        started: bool,
        name: String,
        service_uuid: u16,
        mfg_company: u16,
        mfg_data: Vec<u8>,
    }

    #[wasm_bindgen]
    impl WebAdvertiser {
        /// `service_uuid` of 0 means "no service UUID"; an empty `mfg_data`
        /// means "no manufacturer data".
        #[wasm_bindgen(constructor)]
        pub fn new(
            url: &str,
            name: &str,
            service_uuid: u16,
            mfg_company: u16,
            mfg_data: Vec<u8>,
        ) -> Result<WebAdvertiser, JsValue> {
            install_panic_hook();
            Ok(Self {
                transport: WasmWsTransport::connect(url)?,
                channel: HciChannel::new(),
                started: false,
                name: name.to_string(),
                service_uuid,
                mfg_company,
                mfg_data,
            })
        }

        /// Returns the underlying WebSocket ready state (per the WebSocket API).
        pub fn ready_state(&self) -> u16 {
            self.transport.ready_state()
        }

        /// One pump: on first open, issues the advertising bring-up; then keeps
        /// the socket drained. Advertise-only, so any inbound controller
        /// packets (a central probing) are discarded.
        pub fn tick(&mut self) -> Result<(), JsValue> {
            self.transport.pump(&self.channel)?;
            if !self.started && self.transport.is_open() {
                queue_advertiser_start(
                    &self.channel,
                    &self.name,
                    self.service_uuid,
                    self.mfg_company,
                    &self.mfg_data,
                )
                .map_err(js_error)?;
                self.started = true;
            }
            while self.channel.poll_controller_packet().is_some() {}
            self.transport.pump(&self.channel)?;
            Ok(())
        }
    }

    /// The HRM page's engine: a running Simble whose device is defined by an
    /// editable Rhai script (see [`ScriptedPeripheral`]).
    #[wasm_bindgen]
    pub struct WebPeripheral {
        transport: WasmWsTransport,
        channel: HciChannel,
        peripheral: ScriptedPeripheral,
        started: bool,
        /// The on-air address netsim advertises for this device, kept so a
        /// script rebuild re-stamps the identity SMP computes with.
        address: Option<Address>,
    }

    #[wasm_bindgen]
    impl WebPeripheral {
        #[wasm_bindgen(constructor)]
        pub fn new(url: &str, script: &str) -> Result<WebPeripheral, JsValue> {
            install_panic_hook();
            let mut peripheral = ScriptedPeripheral::run_script(script).map_err(js_error)?;
            // The device must own the address netsim advertises for it, not
            // the script engine's placeholder — SMP mixes it into the pairing
            // crypto, and a real controller drives key distribution off the
            // Encryption Change event.
            if let Some(address) = address_from_ws_url(url) {
                peripheral.set_identity(address);
            }

            let address = address_from_ws_url(url);
            // netsim reads the URL address LSB-first; connect with the wire
            // form so the device lands on the air where the page says it is.
            let url = ws_url_with_wire_address(url);
            Ok(Self {
                transport: WasmWsTransport::connect(&url)?,
                channel: HciChannel::new(),
                peripheral,
                started: false,
                address,
            })
        }

        /// Tears down the current scripted device and rebuilds it from
        /// `script` on the same socket (Run/Restart button). Errors are the
        /// script's compile/runtime message; on error the old device keeps
        /// running.
        pub fn run_script(&mut self, script: &str) -> Result<(), JsValue> {
            let mut peripheral = ScriptedPeripheral::run_script(script).map_err(js_error)?;
            if let Some(address) = self.address {
                peripheral.set_identity(address);
            }
            self.peripheral = peripheral;
            self.channel = HciChannel::new();
            self.started = false;
            Ok(())
        }

        /// Returns the underlying WebSocket ready state (per the WebSocket API).
        pub fn ready_state(&self) -> u16 {
            self.transport.ready_state()
        }

        /// Drains the isochronous SDUs this device has received, as an array
        /// of byte arrays — the netsim counterpart of
        /// `WebLink.peripheral_take_audio`, so a page-hosted sink can play
        /// audio streamed to it by a real peer rather than only by an
        /// in-page source.
        pub fn take_audio(&mut self) -> js_sys::Array {
            let out = js_sys::Array::new();
            for sdu in self.peripheral.take_audio() {
                out.push(&js_sys::Uint8Array::from(&sdu[..]).into());
            }
            out
        }

        /// Writes a characteristic's value from the page (the lightbulb's
        /// colour picker). `uuid` is the string form; on the next tick a
        /// subscribed central is notified of the change.
        pub fn set_value(&mut self, uuid: &str, value: Vec<u8>) -> Result<(), JsValue> {
            self.peripheral
                .set_characteristic_value(uuid, &value)
                .map_err(js_error)
        }

        /// One pump + script tick; `t_seconds` is seconds since the current
        /// script was Run. Returns the peripheral status JSON.
        pub fn tick(&mut self, t_seconds: f64) -> Result<String, JsValue> {
            self.transport.pump(&self.channel)?;
            if !self.started && self.transport.is_open() {
                self.peripheral
                    .queue_start(&self.channel)
                    .map_err(js_error)?;
                self.started = true;
            }
            while let Some(packet) = self.channel.poll_controller_packet() {
                if let Err(e) = self.peripheral.handle_packet(&self.channel, &packet) {
                    self.peripheral.record_error(e.to_string());
                }
            }
            if let Err(e) = self.peripheral.tick(&self.channel, t_seconds) {
                self.peripheral.record_error(e.to_string());
            }
            self.transport.pump(&self.channel)?;
            Ok(self.peripheral.status_json())
        }
    }

    /// The API Explorer's engine: a live [`ScriptedPeripheral`] session built
    /// one Rhai statement at a time. Each Execute in the page calls
    /// [`WebSession::eval_line`] with the single generated line; the session
    /// scope persists across calls (so `svc1`, `chr1`, … stay usable), and the
    /// device is hosted on netsim as soon as a server exists. netsim is
    /// optional — building and inspecting a device works fully offline; the
    /// socket only carries advertising/notifications when it's reachable.
    #[wasm_bindgen]
    pub struct WebSession {
        transport: WasmWsTransport,
        channel: HciChannel,
        peripheral: ScriptedPeripheral,
        started: bool,
        adv_signature: String,
    }

    #[wasm_bindgen]
    impl WebSession {
        #[wasm_bindgen(constructor)]
        pub fn new(url: &str) -> Result<WebSession, JsValue> {
            install_panic_hook();
            Ok(Self {
                transport: WasmWsTransport::connect(url)?,
                channel: HciChannel::new(),
                peripheral: ScriptedPeripheral::new_session(),
                started: false,
                adv_signature: String::new(),
            })
        }

        /// Returns the underlying WebSocket ready state (per the WebSocket API).
        pub fn ready_state(&self) -> u16 {
            self.transport.ready_state()
        }

        /// Evaluates one Rhai line in the persistent session scope and returns
        /// the JSON result (`ok`, `value`, `error`, `events`). Works whether or
        /// not netsim is connected — this only touches the in-page engine.
        pub fn eval_line(&mut self, line: &str) -> String {
            self.peripheral.eval_line_json(line)
        }

        /// One pump + host tick. Once a server exists it advertises (re-issuing
        /// the bring-up whenever the built device's name/services change),
        /// handles connections, and flushes value-change notifications, exactly
        /// like [`WebPeripheral`]. Returns the peripheral status JSON.
        pub fn tick(&mut self, t_seconds: f64) -> Result<String, JsValue> {
            self.transport.pump(&self.channel)?;
            if self.peripheral.has_server() {
                let signature = self.peripheral.adv_signature();
                if signature != self.adv_signature {
                    // The built device changed — restart advertising so the new
                    // name/services go on the air (mirrors run_script's reset).
                    self.adv_signature = signature;
                    self.channel = HciChannel::new();
                    self.started = false;
                }
                if !self.started && self.transport.is_open() {
                    self.peripheral
                        .queue_start(&self.channel)
                        .map_err(js_error)?;
                    self.started = true;
                }
                while let Some(packet) = self.channel.poll_controller_packet() {
                    if let Err(e) = self.peripheral.handle_packet(&self.channel, &packet) {
                        self.peripheral.record_error(e.to_string());
                    }
                }
                if let Err(e) = self.peripheral.tick(&self.channel, t_seconds) {
                    self.peripheral.record_error(e.to_string());
                }
                self.transport.pump(&self.channel)?;
            }
            Ok(self.peripheral.status_json())
        }

        /// The current session's device status JSON (for the live viewer)
        /// without pumping the socket — used right after an Execute so the
        /// viewer reflects the new object immediately.
        pub fn status_json(&self) -> String {
            self.peripheral.status_json()
        }
    }

    /// The default HRM script, so the page needs no separate fetch.
    #[wasm_bindgen]
    pub fn default_heart_rate_script() -> String {
        DEFAULT_HEART_RATE_SCRIPT.to_string()
    }

    /// Runs a Rhai test script (device-building + `assert(...)`) and returns
    /// `{"ok":true}` if every assertion passed, or `{"ok":false,"error":"…"}`
    /// with the failure message.
    #[wasm_bindgen]
    pub fn run_test(script: &str) -> String {
        match super::run_test_script(script) {
            Ok(()) => "{\"ok\":true,\"error\":\"\"}".to_string(),
            Err(e) => {
                let msg = serde_json::to_string(&e).unwrap_or_else(|_| "\"error\"".to_string());
                format!("{{\"ok\":false,\"error\":{msg}}}")
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub use web::{
    WebAdvertiser, WebPeripheral, WebScanner, WebSession, default_heart_rate_script, run_test,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::l2cap::AclPacketBoundary;
    use zerocopy::IntoBytes;
    use crate::att::opcode;
    use crate::l2cap::{L2capHeader, cid};

    fn drain_host_packets(channel: &HciChannel) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        while let Some(packet) = channel.poll_host_packet() {
            out.push(packet);
        }
        out
    }

    fn opcode_of(command: &[u8]) -> u16 {
        assert_eq!(command[0], h4_type::HCI_COMMAND);
        u16::from_le_bytes([command[1], command[2]])
    }

    #[test]
    fn test_scanner_start_queues_reset_masks_params_enable() {
        let channel = HciChannel::new();
        queue_scanner_start(&channel).unwrap();
        let commands = drain_host_packets(&channel);
        let opcodes: Vec<u16> = commands.iter().map(|c| opcode_of(c)).collect();
        assert_eq!(opcodes, vec![0x0C03, 0x0C01, 0x2001, 0x200B, 0x200C]);
        // Both event masks fully open (LE Meta Events are masked by default).
        assert_eq!(&commands[1][4..12], &[0xFF; 8]);
        assert_eq!(&commands[2][4..12], &[0xFF; 8]);
        // Active scanning (scan-type byte 0x01) so advertisers' scan-response
        // data (names) is solicited via SCAN_REQ, not just passively observed.
        assert_eq!(commands[3][4], 0x01);
        // Scan enable with duplicate filtering off.
        assert_eq!(&commands[4][4..], &[0x01, 0x00]);
    }

    /// Builds one LE Advertising Report event around `data`.
    fn adv_report_event(event_type: u8, data: &[u8], rssi: i8) -> Vec<u8> {
        let mut packet = vec![
            h4_type::HCI_EVENT,
            hci_event_code::LE_META,
            (12 + data.len()) as u8, // subevent, count, 9-byte report header, data, RSSI
            le_subevent::ADVERTISING_REPORT,
            0x01,       // one report
            event_type, // ADV_IND etc.
            0x00,       // public address
            0x01,
            0x02,
            0x03,
            0x04,
            0x05,
            0x06, // address (little-endian)
            data.len() as u8,
        ];
        packet.extend_from_slice(data);
        packet.push(rssi as u8);
        packet
    }

    /// Everything a device can put in an advertisement must survive being
    /// encoded and decoded again. simble owns both halves — `AdvertisingData`
    /// builds and `parse_scan_reports` decodes — so this round trip is the
    /// cheapest guard there is against a field being silently dropped on the
    /// way out. `advertise_service_uuid` shipped broken precisely because
    /// nothing checked the encoder against the decoder.
    #[test]
    fn test_every_advertised_field_survives_the_round_trip() {
        let mut extras = AdvertisingData::new();
        extras.service_uuids_16.push(0x185B); // staged by advertise_service_uuid
        extras.service_data_16.push((0xFE2C, vec![0x00, 0x11, 0x22]));
        extras = extras.with_manufacturer_data(0x00E0, &[0xAB]);

        let payload = build_adv_payload_with_extras("Ranger", &[0x180F], Some(&extras))
            .expect("fits in 31 bytes");
        let reports = parse_scan_reports(&adv_report_event(0x00, &payload, -55));
        assert_eq!(reports.len(), 1);
        let report = &reports[0];

        assert_eq!(report.name.as_deref(), Some("Ranger"), "name survives");
        assert!(
            report.service_uuids.iter().any(|u| u == "180F"),
            "the device's own service survives: {:?}",
            report.service_uuids
        );
        assert!(
            report.service_uuids.iter().any(|u| u == "185B"),
            "a staged service UUID survives: {:?}",
            report.service_uuids
        );
        assert!(
            report
                .service_data
                .iter()
                .any(|d| d.tag == "FE2C" && d.data == "001122"),
            "service data survives: {:?}",
            report.service_data
        );
        let mfg = report.manufacturer_data.as_ref().expect("manufacturer data");
        assert_eq!((mfg.tag.as_str(), mfg.data.as_str()), ("00E0", "AB"));
        assert!(report.flags.is_some(), "flags survive");
    }

    #[test]
    fn test_parse_scan_reports_decodes_ad_structures() {
        let mut data = vec![0x02, ad_type::FLAGS, 0x06];
        data.extend_from_slice(&[0x08, ad_type::COMPLETE_LOCAL_NAME]);
        data.extend_from_slice(b"web-hrm");
        data.extend_from_slice(&[0x03, ad_type::COMPLETE_16BIT_UUIDS, 0x0D, 0x18]);
        data.extend_from_slice(&[
            0x05,
            ad_type::MANUFACTURER_SPECIFIC_DATA,
            0xE0,
            0x00,
            0xAB,
            0xCD,
        ]);
        let packet = adv_report_event(0x00, &data, -42);

        let reports = parse_scan_reports(&packet);
        assert_eq!(reports.len(), 1);
        let report = &reports[0];
        assert_eq!(report.address, "06:05:04:03:02:01");
        assert_eq!(report.address_type, "public");
        assert!(report.connectable);
        assert!(!report.scan_response);
        assert_eq!(report.rssi, -42);
        assert_eq!(report.name.as_deref(), Some("web-hrm"));
        assert_eq!(report.flags, Some(0x06));
        assert_eq!(report.service_uuids, vec!["180D".to_string()]);
        let manufacturer = report.manufacturer_data.as_ref().unwrap();
        assert_eq!(manufacturer.tag, "00E0");
        assert_eq!(manufacturer.data, "ABCD");
    }

    #[test]
    fn test_parse_scan_reports_ignores_other_packets() {
        assert!(
            parse_scan_reports(&[h4_type::HCI_EVENT, 0x0E, 0x04, 0x01, 0x03, 0x0C, 0x00])
                .is_empty()
        );
        assert!(parse_scan_reports(&[h4_type::HCI_ACL_DATA, 0x40, 0x00, 0x00, 0x00]).is_empty());
        // Truncated report body: no panic, no report.
        assert!(
            parse_scan_reports(&[
                h4_type::HCI_EVENT,
                hci_event_code::LE_META,
                0x04,
                le_subevent::ADVERTISING_REPORT,
                0x01,
                0x00,
                0x00
            ])
            .is_empty()
        );
    }

    #[test]
    fn test_build_adv_payload_fits_31_bytes_and_keeps_name() {
        let payload = build_adv_payload("web-hrm", &[0x180D]);
        assert!(payload.len() <= 31);
        assert!(payload.windows(7).any(|w| w == b"web-hrm"));
        assert!(payload.windows(2).any(|w| w == [0x0D, 0x18]));

        // An oversized name drops the UUID list and trims, never overflows.
        let long = "a-device-name-well-past-the-thirty-one-byte-advertising-limit";
        let payload = build_adv_payload(long, &[0x180D, 0x180F, 0x1812]);
        assert!(payload.len() <= 31);
        assert!(!payload.windows(2).any(|w| w == [0x0D, 0x18]));
        assert!(payload.windows(9).any(|w| w == b"a-device-"));
    }

    fn le_connection_complete(handle: u16, peer_le: [u8; 6]) -> Vec<u8> {
        let mut packet = vec![
            h4_type::HCI_EVENT,
            hci_event_code::LE_META,
            19,
            le_subevent::CONNECTION_COMPLETE,
            0x00, // status
        ];
        packet.extend_from_slice(&handle.to_le_bytes());
        packet.push(0x01); // role: peripheral
        packet.push(0x00); // peer address type
        packet.extend_from_slice(&peer_le);
        packet.extend_from_slice(&[0x28, 0x00, 0x00, 0x00, 0x48, 0x00, 0x00]); // interval etc.
        packet
    }

    fn acl_packet(handle: u16, l2cap: &[u8]) -> Vec<u8> {
        let mut packet = vec![h4_type::HCI_ACL_DATA];
        packet.extend_from_slice(
            HciAclHeader::new(
                handle,
                AclPacketBoundary::FirstAutoFlushable,
                l2cap.len() as u16,
            )
            .as_bytes(),
        );
        packet.extend_from_slice(l2cap);
        packet
    }

    /// Runs the shipped default script and walks the full peripheral life
    /// cycle a browser session would: start (advertising bring-up), connect,
    /// subscribe via a real CCCD write, script tick driving a real
    /// notification, then disconnect and re-advertise.
    #[test]
    fn test_default_script_full_lifecycle() {
        let mut peripheral =
            ScriptedPeripheral::run_script(DEFAULT_HEART_RATE_SCRIPT).expect("default script runs");
        assert_eq!(peripheral.device_name(), "web-thermometer");
        assert!(peripheral.tick_defined);

        let channel = HciChannel::new();
        peripheral.queue_start(&channel).unwrap();
        let commands = drain_host_packets(&channel);
        let opcodes: Vec<u16> = commands.iter().map(|c| opcode_of(c)).collect();
        // 0x2074 (LE Set Host Feature) declares CIS host support, without
        // which a controller refuses to open an isochronous stream.
        assert_eq!(
            opcodes,
            vec![0x0C03, 0x0C01, 0x2001, 0x2074, 0x2006, 0x2008, 0x2009, 0x200A]
        );
        // Advertising data carries the script device's name and the
        // Environmental Sensing service UUID (0x181A) the script declared.
        let adv_data = &commands[5];
        assert!(adv_data.windows(15).any(|w| w == b"web-thermometer"));
        assert!(adv_data.windows(2).any(|w| w == [0x1A, 0x18]));

        // Central connects.
        let peer = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
        peripheral
            .handle_packet(&channel, &le_connection_complete(0x0040, peer))
            .unwrap();
        let status = peripheral.status_json();
        assert!(status.contains("\"connected\":true"));

        // Central subscribes: real ATT write to the CCCD the script added.
        let watch = peripheral.watched[0].clone();
        let cccd = watch.cccd_handle.expect("script added a CCCD");
        let mut write = vec![opcode::WRITE_REQ];
        write.extend_from_slice(&cccd.to_le_bytes());
        write.extend_from_slice(&[0x01, 0x00]);
        peripheral
            .handle_packet(
                &channel,
                &acl_packet(0x0040, &L2capHeader::serialize(cid::ATT, &write)),
            )
            .unwrap();
        // The Write Response went out as ACL data.
        let responses = drain_host_packets(&channel);
        assert!(
            responses
                .iter()
                .any(|p| p[0] == h4_type::HCI_ACL_DATA && p.ends_with(&[opcode::WRITE_RSP]))
        );
        assert_eq!(
            peripheral.cccd_subscription(&watch),
            CccdSubscription::Notify
        );

        // Script tick updates the temperature, which becomes a notification.
        peripheral.tick(&channel, 2.0).unwrap();
        assert!(
            peripheral.last_error.is_none(),
            "{:?}",
            peripheral.last_error
        );
        let packets = drain_host_packets(&channel);
        let notification = packets
            .iter()
            .find(|p| p[0] == h4_type::HCI_ACL_DATA && p.contains(&opcode::HANDLE_VALUE_NTF))
            .expect("tick produced a notification");
        // Temperature (0x2A6E): a signed 16-bit little-endian value in
        // hundredths of a degree C — the last two bytes of the notification.
        let value = &notification[notification.len() - 2..];
        let centi = i16::from_le_bytes([value[0], value[1]]);
        assert!((2000..=2300).contains(&centi), "centi {centi}");

        // Same value on the next tick at the same t: no duplicate notification.
        peripheral.tick(&channel, 2.0).unwrap();
        assert!(drain_host_packets(&channel).is_empty());

        // Disconnect: state clears and advertising is re-enabled.
        let disconnect = vec![
            h4_type::HCI_EVENT,
            hci_event_code::DISCONNECTION_COMPLETE,
            4,
            0x00,
            0x40,
            0x00,
            0x13,
        ];
        peripheral.handle_packet(&channel, &disconnect).unwrap();
        assert!(peripheral.status_json().contains("\"connected\":false"));
        let commands = drain_host_packets(&channel);
        assert_eq!(commands.len(), 1);
        assert_eq!(opcode_of(&commands[0]), 0x200A);
        assert_eq!(commands[0][4], 0x01);
    }

    #[test]
    fn test_script_errors_surface_as_strings() {
        let compile_error = ScriptedPeripheral::run_script("let x = ;")
            .err()
            .expect("syntax error");
        assert!(!compile_error.is_empty());

        let no_server = ScriptedPeripheral::run_script("let x = 1 + 1;")
            .err()
            .expect("no server created");
        assert!(no_server.contains("BluetoothGattServer"));

        // A broken tick doesn't kill the device — the error is recorded.
        let script = r#"
            let server = android::BluetoothGattServer("web-hrm");
            fn tick(server, t) { nonexistent_function(); }
        "#;
        let mut peripheral = ScriptedPeripheral::run_script(script).unwrap();
        let channel = HciChannel::new();
        peripheral.tick(&channel, 0.1).unwrap();
        assert!(peripheral.last_error.is_some());
        assert!(peripheral.status_json().contains("last_error"));
    }

    #[test]
    fn test_update_value_extension_writes_the_real_database() {
        let script = r#"
            let server = android::BluetoothGattServer("dev");
            let svc = android::BluetoothGattService(uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY);
            let chr = android::BluetoothGattCharacteristic(
                uuid::HEART_RATE_MEASUREMENT,
                android::PROPERTY_READ | android::PROPERTY_NOTIFY,
                android::PERMISSION_READ,
            );
            chr.set_value([0x00, 60]);
            svc.add_characteristic(chr);
            server.add_service(svc);
            server.update_value(uuid::HEART_RATE_MEASUREMENT, [0x00, 99]);
        "#;
        let peripheral = ScriptedPeripheral::run_script(script).unwrap();
        let watch = &peripheral.watched[0];
        assert_eq!(peripheral.attribute_value(watch).unwrap(), vec![0x00, 99]);
    }

    #[test]
    fn test_status_json_reports_characteristic_properties() {
        // The generic viewer renders R/W/N/I chips from the raw property
        // bitmask, so the status snapshot must expose it per characteristic.
        let peripheral =
            ScriptedPeripheral::run_script(DEFAULT_HEART_RATE_SCRIPT).expect("default script runs");
        let status = peripheral.status_json();
        assert!(status.contains("\"properties\":"), "{status}");
        // The Temperature characteristic is READ (0x02) | NOTIFY (0x10) = 0x12.
        let expected = (BluetoothGattCharacteristic::PROPERTY_READ
            | BluetoothGattCharacteristic::PROPERTY_NOTIFY) as i64;
        assert!(
            status.contains(&format!("\"properties\":{expected}")),
            "{status}"
        );
    }

    #[test]
    fn test_session_builds_device_incrementally() {
        // The API Explorer's model: one Rhai statement per Execute, with
        // `let`-bound objects (svc1, chr1, …) persisting in the shared scope
        // and usable by later Executes.
        let mut session = ScriptedPeripheral::new_session();
        assert!(!session.has_server());
        assert!(session.status_json().contains("\"services\":[]"));

        // A `let` binding returns unit and produces no events.
        let outcome = session
            .eval_line(r#"let server = android::BluetoothGattServer("explorer");"#)
            .unwrap();
        assert_eq!(outcome.value, "()");
        assert!(outcome.events.is_empty());
        assert!(session.has_server());
        assert_eq!(session.device_name(), "explorer");

        session
            .eval_line(
                r#"let svc1 = android::BluetoothGattService(uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY);"#,
            )
            .unwrap();
        session
            .eval_line(
                r#"let chr1 = android::BluetoothGattCharacteristic(uuid::HEART_RATE_MEASUREMENT, android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);"#,
            )
            .unwrap();
        // svc1 and chr1 survive from earlier Executes in the shared scope.
        session.eval_line("svc1.add_characteristic(chr1);").unwrap();
        // add_service fires the on_service_added callback -> a session event.
        let added = session.eval_line("server.add_service(svc1);").unwrap();
        assert!(
            added.events.iter().any(|e| e.contains("service_added")),
            "events: {:?}",
            added.events
        );

        let status = session.status_json();
        assert!(status.contains("180D"), "{status}"); // Heart Rate service
        assert!(status.contains("2A37"), "{status}"); // HR Measurement char

        // A returned expression renders its value (get_service -> a service).
        let got = session
            .eval_line("server.get_service(uuid::HEART_RATE_SERVICE)")
            .unwrap();
        assert!(!got.value.is_empty());

        // The built device is hostable: advertising bring-up carries its name.
        assert!(session.has_server());
        let channel = HciChannel::new();
        session.queue_start(&channel).unwrap();
        let commands = drain_host_packets(&channel);
        assert!(commands[5].windows(8).any(|w| w == b"explorer"));
    }

    #[test]
    fn test_set_characteristic_value_host_write() {
        // The lightbulb page's colour picker writes a custom 128-bit "colour"
        // characteristic from the host side; the write must land in the live
        // database (and thus be visible to the viewer and notifiable).
        let script = r#"
            let server = android::BluetoothGattServer("web-lightbulb");
            let svc = android::BluetoothGattService(
                uuid::of("f0ff0001-1234-5678-90ab-cdef01234567"),
                android::SERVICE_TYPE_PRIMARY,
            );
            let color = android::BluetoothGattCharacteristic(
                uuid::of("f0ff0002-1234-5678-90ab-cdef01234567"),
                android::PROPERTY_READ | android::PROPERTY_WRITE | android::PROPERTY_NOTIFY,
                android::PERMISSION_READ | android::PERMISSION_WRITE,
            );
            color.set_value([0x33, 0xcc, 0xff]);
            let cccd = android::BluetoothGattDescriptor(
                uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
                android::PERMISSION_READ | android::PERMISSION_WRITE,
            );
            color.add_descriptor(cccd);
            svc.add_characteristic(color);
            server.add_service(svc);
        "#;
        let mut peripheral = ScriptedPeripheral::run_script(script).unwrap();
        peripheral
            .set_characteristic_value("f0ff0002-1234-5678-90ab-cdef01234567", &[0xff, 0x00, 0x00])
            .unwrap();
        let status = peripheral.status_json();
        assert!(status.contains("FF0000"), "{status}");
        // A bad UUID is a clean error, not a panic.
        assert!(peripheral.set_characteristic_value("nope", &[0]).is_err());
        // A UUID with no matching characteristic errors too.
        assert!(peripheral.set_characteristic_value("2A19", &[0]).is_err());
    }

    #[test]
    fn test_session_eval_error_surfaces_as_json() {
        let mut session = ScriptedPeripheral::new_session();
        // A runtime error comes back as ok:false with the message, not a panic.
        let json = session.eval_line_json("nonexistent_function(1, 2)");
        assert!(json.contains("\"ok\":false"), "{json}");
        assert!(json.contains("\"error\":"), "{json}");
        // The session is still usable afterwards.
        let json = session.eval_line_json(r#"let server = android::BluetoothGattServer("ok");"#);
        assert!(json.contains("\"ok\":true"), "{json}");
    }

    #[test]
    fn test_demo_advertiser_bring_up() {
        // The scanner page's self-spun demo devices advertise via this path;
        // verify the HCI sequence and that the payload carries the name,
        // service UUID, and manufacturer data.
        let channel = HciChannel::new();
        queue_advertiser_start(
            &channel,
            "Simble Beacon",
            0x180F,
            0x0059,
            &[0x01, 0x02, 0x03, 0x04],
        )
        .unwrap();
        let commands = drain_host_packets(&channel);
        let opcodes: Vec<u16> = commands.iter().map(|c| opcode_of(c)).collect();
        // 0x2074 (LE Set Host Feature) declares CIS host support, without
        // which a controller refuses to open an isochronous stream.
        assert_eq!(
            opcodes,
            vec![0x0C03, 0x0C01, 0x2001, 0x2006, 0x2008, 0x2009, 0x200A]
        );
        let adv_data = &commands[4];
        assert!(adv_data.windows(13).any(|w| w == b"Simble Beacon"));
        assert!(adv_data.windows(2).any(|w| w == [0x0F, 0x18])); // service 0x180F
        assert!(adv_data.windows(4).any(|w| w == [0x01, 0x02, 0x03, 0x04])); // mfg data
        // Enable advertising is the last command, with the enable flag set.
        assert_eq!(commands[6][4], 0x01);
    }

    #[test]
    fn test_demo_adv_payload_trims_to_limit() {
        // A very long name still yields a legal (<= 31-byte) payload.
        let long = "a-demo-advertiser-name-well-past-the-legacy-advertising-limit";
        let payload = build_demo_adv_payload(long, 0x181A, 0, &[]);
        assert!(payload.len() <= 31, "payload {} bytes", payload.len());
    }
}
