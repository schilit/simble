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
pub const DEFAULT_HEART_RATE_SCRIPT: &str = include_str!("../../catalog/devices/heart_rate.rhai");

use crate::device::LeHost;
use crate::packets::hci_events::{
    HciEvent, advertising_reports, event_code as hci_event_code, le_subevent,
};
// Advertising payload builders live with `AdvertisingData` in `gap`; kept in
// this module's public surface for the browser bindings and existing callers.
use crate::gap::advertising::fit_within_legacy_limit;
pub use crate::gap::advertising::{build_adv_payload, build_adv_payload_with_extras};
use crate::l2cap::{AclReassembler, HciAclHeader, L2capHeader};
use crate::packets::att::{AttExchangeMtuRsp, AttHandleValueHeader, opcode as att_op};
use crate::scripting::bindings::{dynamic_to_bytes, runtime_error};
use crate::scripting::{ScriptBroadcastSource, ScriptGattServer, ScriptedCentral, new_engine};
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
    for packet in crate::device::host::init_commands().into_iter().take(3) {
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
    /// The CSIP Resolvable Set Identifier (AD type 0x2E) as hex, if present:
    /// six octets, `hash || prand` (CSIS Section 4.9). A coordinator turns this
    /// into "member of the set I already have" by recomputing the hash with
    /// each SIRK it holds — see [`crate::profiles::csip::rsi_matches`].
    pub resolvable_set_identifier: Option<String>,
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
            // Exactly six octets or it is not an RSI. A shorter or longer
            // structure is dropped rather than resolved against, because `sih`
            // over the wrong octets still returns a confident three bytes.
            ad_type::RESOLVABLE_SET_IDENTIFIER if payload.len() == 6 => {
                report.resolvable_set_identifier = Some(hex(payload));
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
    if code != hci_event_code::LE_META
        || parameters.first() != Some(&le_subevent::ADVERTISING_REPORT)
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
                resolvable_set_identifier: None,
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
/// name is the demo's identity, so it survives longest. Shares the single
/// `fit_within_legacy_limit` loop with the other two builders, so an
/// overflow is an error here too rather than a payload that never transmits.
pub fn build_demo_adv_payload(
    name: &str,
    service_uuid: u16,
    mfg_company: u16,
    mfg_data: &[u8],
) -> Result<Vec<u8>, SimbleError> {
    fit_within_legacy_limit(name, |name, complete, keep_extras| {
        let mut ad = AdvertisingData::new()
            .with_flags(flags::LE_GENERAL_DISCOVERABLE | flags::BR_EDR_NOT_SUPPORTED);
        if keep_extras {
            if service_uuid != 0 {
                ad = ad.with_service_uuid_16(service_uuid);
            }
            if !mfg_data.is_empty() {
                ad = ad.with_manufacturer_data(mfg_company, mfg_data);
            }
        }
        ad.with_name_on_air(name, complete).to_bytes()
    })
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
        )?),
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

/// Narrows a Rhai integer to a `u8`, so a script that passes 300 for a volume gets a
/// runtime error naming the parameter instead of a silent truncation. Rhai has one integer
/// type, and every profile field below is a byte.
fn to_u8(name: &str, value: i64) -> Result<u8, Box<EvalAltResult>> {
    u8::try_from(value).map_err(|_| runtime_error(format!("{name}: not a byte (0..=255): {value}")))
}

/// Reads a 16-byte key (a CSIP SIRK) out of a script value.
fn to_key_128(name: &str, value: Dynamic) -> Result<[u8; 16], Box<EvalAltResult>> {
    let bytes = dynamic_to_bytes(value)?;
    <[u8; 16]>::try_from(bytes.as_slice())
        .map_err(|_| runtime_error(format!("{name}: expected 16 bytes, got {}", bytes.len())))
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
    // ---- Volume Control: Android's `BluetoothVolumeControl` -------------
    //
    // Three services, because that is how the spec splits them: VCS is the
    // device's own volume, and a device includes one VOCS per audio output
    // and one AICS per audio input. `BluetoothVolumeControl` is the single
    // Android proxy over all three (`setVolume`, `setVolumeOffset`,
    // `setDeviceVolume`), so all three carry its name.
    //
    // Each control point is a state machine with a change counter, and a
    // rejected write becomes a real ATT Error Response. That is the part a
    // script cannot hand-build: Rhai has no way to fail an ATT write, so a
    // hand-rolled VCS silently accepts a stale change counter that a real
    // one rejects with 0x80.
    engine.register_fn(
        "add_vcs",
        |server: &mut ScriptGattServer,
         initial_volume: i64,
         initial_mute: i64|
         -> Result<(), Box<EvalAltResult>> {
            let volume = to_u8("add_vcs initial_volume", initial_volume)?;
            let mute = to_u8("add_vcs initial_mute", initial_mute)?;
            server.with_server(|s| {
                crate::profiles::VolumeControlService::register(
                    &mut s.device.gatt_db,
                    volume,
                    mute,
                );
            });
            Ok(())
        },
    );
    // One audio output's offset trim (VOCS). `audio_location` is a Bluetooth
    // Audio Location bitmap — 0x01 front left, 0x02 front right.
    engine.register_fn(
        "add_vocs",
        |server: &mut ScriptGattServer,
         audio_location: i64,
         description: &str|
         -> Result<(), Box<EvalAltResult>> {
            let location = u32::try_from(audio_location).map_err(|_| {
                runtime_error(format!(
                    "add_vocs audio_location: not a bitmap: {audio_location}"
                ))
            })?;
            server.with_server(|s| {
                crate::profiles::vocs::VolumeOffsetControlService::register(
                    &mut s.device.gatt_db,
                    location,
                    description,
                );
            });
            Ok(())
        },
    );
    // One audio input's gain (AICS). The input is typed `Bluetooth` and
    // `Active`, the case an LE Audio sink actually has; the gain range is the
    // script's, because that is the part that differs per device.
    engine.register_fn(
        "add_aics",
        |server: &mut ScriptGattServer,
         gain_minimum: i64,
         gain_maximum: i64,
         description: &str|
         -> Result<(), Box<EvalAltResult>> {
            let gain_settings_minimum = to_u8("add_aics gain_minimum", gain_minimum)?;
            let gain_settings_maximum = to_u8("add_aics gain_maximum", gain_maximum)?;
            if gain_settings_minimum > gain_settings_maximum {
                return Err(runtime_error(format!(
                    "add_aics: gain_minimum {gain_settings_minimum} exceeds gain_maximum {gain_settings_maximum}"
                )));
            }
            server.with_server(|s| {
                crate::profiles::aics::AudioInputControlService::register(
                    &mut s.device.gatt_db,
                    crate::profiles::aics::GainSettingsProperties {
                        gain_settings_units: 1,
                        gain_settings_minimum,
                        gain_settings_maximum,
                    },
                    crate::profiles::aics::AudioInputType::Bluetooth,
                    crate::profiles::aics::AudioInputStatus::Active,
                    description,
                );
            });
            Ok(())
        },
    );

    // ---- Coordinated sets: Android's `BluetoothCsipSetCoordinator` -------
    //
    // The half a script cannot build is the crypto: a set member is found by
    // resolving the Resolvable Set Identifier in its advertisement against a
    // SIRK, which is AES-CMAC and AES-128 (`csip::sih`). Rhai has neither.
    engine.register_fn(
        "add_csis",
        |server: &mut ScriptGattServer,
         sirk: Dynamic,
         set_size: i64,
         rank: i64|
         -> Result<(), Box<EvalAltResult>> {
            let sirk = to_key_128("add_csis sirk", sirk)?;
            let set_size = to_u8("add_csis set_size", set_size)?;
            let rank = to_u8("add_csis rank", rank)?;
            if rank == 0 || rank > set_size {
                return Err(runtime_error(format!(
                    "add_csis: rank {rank} is outside 1..={set_size} (CSIP Section 3.3: ranks are 1-based and unique within the set)"
                )));
            }
            server.with_server(|s| {
                crate::profiles::CoordinatedSetIdentificationService::register(
                    &mut s.device.gatt_db,
                    sirk,
                    set_size,
                    rank,
                );
            });
            Ok(())
        },
    );
    // Stages the Resolvable Set Identifier AD structure (0x2E) this member
    // advertises, so a coordinator holding the SIRK can recognise it. Without
    // this the device is discoverable but not identifiable as a set member,
    // which is the whole point of CSIP.
    engine.register_fn(
        "advertise_set_identity",
        |server: &mut ScriptGattServer,
         sirk: Dynamic,
         prand: Dynamic|
         -> Result<(), Box<EvalAltResult>> {
            let sirk = to_key_128("advertise_set_identity sirk", sirk)?;
            let prand_bytes = dynamic_to_bytes(prand)?;
            let prand = <[u8; 3]>::try_from(prand_bytes.as_slice()).map_err(|_| {
                runtime_error(format!(
                    "advertise_set_identity prand: expected 3 bytes, got {}",
                    prand_bytes.len()
                ))
            })?;
            let rsi = crate::profiles::csip::rsi(&sirk, &prand);
            server.with_server(|s| {
                s.device
                    .advertising_data
                    .get_or_insert_with(crate::gap::AdvertisingData::new)
                    .resolvable_set_identifier = Some(rsi.to_vec());
            });
            Ok(())
        },
    );

    // ---- Hearing Access: Android's `BluetoothHapClient` ------------------
    //
    // Presets are a list a client pages through with Read Presets Request and
    // switches with Set Active Preset, all over one control point that
    // *indicates* its responses. `presets` is an array of maps:
    //   #{ index: 1, name: "Universal", writable: true, available: true }
    engine.register_fn(
        "add_has",
        |server: &mut ScriptGattServer,
         features: i64,
         presets: Array|
         -> Result<(), Box<EvalAltResult>> {
            let features = to_u8("add_has features", features)?;
            let features = crate::profiles::hap::HearingAidFeatures::from_byte(features)
                .ok_or_else(|| {
                    runtime_error(format!(
                        "add_has features: 0x{features:02X} has no valid hearing-aid type in bits 0-1"
                    ))
                })?;
            // `HearingAccessService::register` panics on an empty list or an
            // over-long name. A panic in a script binding takes down the whole
            // engine, so the same conditions are checked here and returned as
            // script errors instead.
            if presets.is_empty() {
                return Err(runtime_error(
                    "add_has: needs at least one preset (HAS Section 2.8)".to_string(),
                ));
            }
            let mut records = Vec::with_capacity(presets.len());
            for (n, preset) in presets.into_iter().enumerate() {
                let map = preset.try_cast::<Map>().ok_or_else(|| {
                    runtime_error(format!("add_has preset {n}: expected a map"))
                })?;
                let get_int = |key: &str| -> Option<i64> {
                    map.get(key).and_then(|v| v.as_int().ok())
                };
                let index = to_u8(
                    &format!("add_has preset {n} index"),
                    get_int("index").ok_or_else(|| {
                        runtime_error(format!("add_has preset {n}: missing `index`"))
                    })?,
                )?;
                let name = map
                    .get("name")
                    .and_then(|v| v.clone().into_string().ok())
                    .ok_or_else(|| {
                        runtime_error(format!("add_has preset {n}: missing `name`"))
                    })?;
                if name.is_empty() || name.len() > 40 {
                    return Err(runtime_error(format!(
                        "add_has preset {n}: name is 1..=40 bytes (HAS Section 2.8), got {}",
                        name.len()
                    )));
                }
                let flag = |key: &str, default: bool| {
                    map.get(key).and_then(|v| v.as_bool().ok()).unwrap_or(default)
                };
                records.push(crate::profiles::hap::PresetRecord {
                    index,
                    writable: flag("writable", true),
                    available: flag("available", true),
                    name,
                });
            }
            server.with_server(|s| {
                crate::profiles::HearingAccessService::register(
                    &mut s.device.gatt_db,
                    features,
                    &records,
                );
            });
            Ok(())
        },
    );

    // ---- Media Control --------------------------------------------------
    //
    // Android has NO Bluetooth profile proxy for this. An app controls media
    // through `MediaSession`/`MediaController` and the Bluetooth stack bridges
    // to MCS internally, so there is no `BluetoothMediaControl` to mirror and
    // this binding is named for the service instead.
    //
    // `add_gmcs` registers the Generic Media Control Service (the device-wide
    // player a client finds without knowing any app); `add_mcs` registers a
    // per-player instance, identified by its Content Control ID.
    engine.register_fn(
        "add_gmcs",
        |server: &mut ScriptGattServer,
         player_name: &str,
         ccid: i64|
         -> Result<(), Box<EvalAltResult>> {
            let ccid = to_u8("add_gmcs ccid", ccid)?;
            server.with_server(|s| {
                crate::profiles::MediaControlService::register_generic(
                    &mut s.device.gatt_db,
                    player_name,
                    ccid,
                );
            });
            Ok(())
        },
    );
    engine.register_fn(
        "add_mcs",
        |server: &mut ScriptGattServer,
         player_name: &str,
         ccid: i64|
         -> Result<(), Box<EvalAltResult>> {
            let ccid = to_u8("add_mcs ccid", ccid)?;
            server.with_server(|s| {
                crate::profiles::MediaControlService::register(
                    &mut s.device.gatt_db,
                    player_name,
                    ccid,
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
                runtime_error(format!(
                    "advertise_service_uuid: not a 16-bit uuid: {uuid16}"
                ))
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
    // `catalog::*` and `assert_over`, which build on the bindings above; see
    // `crate::scripting::register_host_extensions`.
    crate::scripting::register_host_extensions(engine);
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
    /// Auracast broadcast sources the script built
    /// (`android::BluetoothLeBroadcast`).
    ///
    /// A source is not a GATT server and shares nothing with one: it drives an
    /// extended advertising set, a periodic train and a BIG, all of which the
    /// controller tracks separately from the legacy advertising this
    /// peripheral's own bring-up uses. So the two coexist on one device — as
    /// they do on a real Auracast TV, which is a connectable GATT peripheral
    /// *and* a broadcast source.
    sources: Vec<ScriptBroadcastSource>,
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

        let sources: Vec<ScriptBroadcastSource> = scope
            .iter()
            .filter_map(|(_, _, value)| value.try_cast::<ScriptBroadcastSource>())
            .collect();

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
            sources,
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
            sources: Vec::new(),
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

    /// The first `android::BluetoothGattServer` the script built, or `None` if
    /// it built none. Paired with [`ScriptGattServer::with_server`] this lets a
    /// Rust test drive real ATT traffic at a device a script composed — the
    /// only way to check that a profile binding wired up a live state machine
    /// rather than an inert set of attributes.
    pub fn primary_server(&self) -> Option<&ScriptGattServer> {
        self.servers.first()
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
    /// Narrows the `LE_Event_Mask` this peripheral's bring-up asks its
    /// controller for — see
    /// [`LeHost::set_le_event_mask`](crate::device::host::LeHost::set_le_event_mask).
    ///
    /// Only a real controller cares. Simble's own controller and rootcanal
    /// accept any mask, so a dongle-backed scene is the only caller.
    pub fn set_le_event_mask(&mut self, mask: [u8; 8]) {
        self.host.set_le_event_mask(mask);
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
        // A broadcast source is addressed by the same identity: its
        // announcement is what a receiver filters on, and the metadata an
        // Assistant hands to a Scan Delegator names this address.
        for source in &self.sources {
            source.set_address(address);
        }
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
    ///
    /// When the characteristic has an `AttributeHandler` — i.e. it is a control
    /// point owned by a Rust profile — the write is routed through
    /// `GattDatabase::write` so the state machine runs, exactly as it would for
    /// a connected peer's ATT write. `set_value` would store the opcode and
    /// execute nothing, so the Audio page's volume buttons wrote a command that
    /// was never applied. Everything without a handler keeps the direct path,
    /// where bypassing dispatch is the point.
    pub fn set_characteristic_value(&mut self, uuid: &str, bytes: &[u8]) -> Result<(), String> {
        let uuid = uuid.parse::<Uuid>().map_err(|e| e.to_string())?;
        let handle = find_value_handle(self.primary(), uuid)
            .ok_or_else(|| format!("no characteristic with UUID {uuid}"))?;
        self.primary()
            .with_server(|s| {
                if s.device.gatt_db.has_handler(handle) {
                    s.device.gatt_db.write(handle, bytes)
                } else {
                    s.device.gatt_db.set_value(handle, bytes)
                }
            })
            .map_err(|status| format!("set_value failed: ATT error {status}"))
    }

    /// Host-writes a characteristic and notifies it even if the bytes did not
    /// change.
    ///
    /// The value-diff in `flush_value_notifications` is right for a
    /// characteristic that holds *state* — a battery level that has not moved
    /// is not news. It is wrong for one that reports *change*: two identical
    /// HID mouse reports mean the pointer moved twice by the same amount, and
    /// suppressing the second stalls the pointer for anyone dragging at a
    /// steady speed. The same applies to a keystroke repeated by auto-repeat.
    pub fn notify_characteristic_value(&mut self, uuid: &str, bytes: &[u8]) -> Result<(), String> {
        self.set_characteristic_value(uuid, bytes)?;
        let uuid = uuid.parse::<Uuid>().map_err(|e| e.to_string())?;
        if let Some(handle) = find_value_handle(self.primary(), uuid) {
            // Forgetting the memo is what makes the next flush treat this
            // value as new.
            self.last_values.remove(&handle);
        }
        Ok(())
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
        self.flush_broadcast_sources(channel)?;
        Ok(())
    }

    /// Sends whatever the script's broadcast sources have queued — the setup
    /// ladder `start_broadcast` began, a teardown, or an SDU.
    fn flush_broadcast_sources(&self, channel: &HciChannel) -> Result<(), SimbleError> {
        for source in &self.sources {
            for packet in source.take_outbox() {
                channel.inject_host_packet(packet)?;
            }
        }
        Ok(())
    }

    /// Turns broadcast-source state transitions into the script's
    /// `on_broadcast_*` / `on_playback_*` callbacks.
    fn dispatch_broadcast_callbacks(&mut self) {
        for source in self.sources.clone() {
            let receiver = Dynamic::from(source.clone());
            for (name, args) in source.take_callbacks() {
                if !crate::scripting::broadcast::defines(&self.ast, name, args.len() + 1) {
                    continue;
                }
                let mut all = vec![receiver.clone()];
                all.extend(args);
                let options = CallFnOptions::new()
                    .eval_ast(false)
                    .bind_this_ptr(&mut self.state);
                let result = self.engine.call_fn_with_options::<Dynamic>(
                    options,
                    &mut self.scope,
                    &self.ast,
                    name,
                    all,
                );
                match result {
                    Ok(_) => self.last_error = None,
                    Err(e) => self.last_error = Some(e.to_string()),
                }
            }
        }
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
    /// CCCD attribute in the database before the following declaration.
    ///
    /// Two passes, because there are two ways a characteristic gets into a
    /// device. `getServices()` returns only what went through the Android
    /// layer — what the *script* built. A Rust profile registrar (`add_bass`,
    /// `add_ascs`, `add_pacs`) writes straight into the `GattDatabase` and
    /// never appears there, so for as long as this had only the first pass,
    /// **no profile-registered characteristic could ever notify**: BASS's
    /// Broadcast Receive State, ASCS's ASEs and control point, all
    /// mandatory-notify, all silent. The second pass walks the database
    /// itself and picks up whatever the first missed.
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
            let already: Vec<u16> = self.watched.iter().map(|w| w.value_handle).collect();
            let from_database = server.with_server(|s| notifying_in_database(&s.device.gatt_db));
            for (value_handle, cccd_handle) in from_database {
                if already.contains(&value_handle) {
                    continue;
                }
                self.watched.push(WatchedCharacteristic {
                    server_index,
                    value_handle,
                    cccd_handle,
                });
            }
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
        // The broadcast ladder is command/event driven and shares the same
        // controller: each source sees the whole stream and answers only the
        // events it asked for.
        for source in &self.sources {
            for out in source.on_packet(packet) {
                channel.inject_host_packet(out)?;
            }
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
            //
            // bind_this_ptr: `state` is documented as bound for `tick` *and*
            // `on_event`, and only `on_event` ever got it — so a peripheral's
            // `fn tick` could not remember anything between calls, while a
            // central's could. `'this' not bound` is what a script saw.
            let options = CallFnOptions::new()
                .eval_ast(false)
                .bind_this_ptr(&mut self.state);
            let result = self.engine.call_fn_with_options::<Dynamic>(
                options,
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
        // Broadcast callbacks after the script's own tick, so a `fn tick` that
        // called `start_broadcast` sees `on_broadcast_started` in the same
        // pass rather than one tick later.
        self.dispatch_broadcast_callbacks();
        self.flush_broadcast_sources(channel)?;
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
        // Append anything a Rust profile registrar put straight into the
        // database — see `database_only_services`.
        let services = self.primary().with_server(|s| {
            let mut services: Vec<ServiceStatus> = services;
            let known: Vec<String> = services.iter().map(|x| x.uuid.clone()).collect();
            services.extend(database_only_services(
                &s.device.gatt_db,
                &known,
                &subscribed_handles,
            ));
            services
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

/// Every notify/indicate-capable characteristic in `db`, as
/// `(value_handle, cccd_handle)`, read from the Characteristic Declarations
/// themselves rather than from the Android service list.
///
/// This is what reaches profile registrars: `add_bass` and friends write into
/// the database and never touch `BluetoothGattServer::get_services`, so a
/// declaration is the only record that a Broadcast Receive State exists at
/// all. The declaration's value is `[properties, value_handle(2), uuid]`
/// (Vol 3, Part G, Section 3.3.1).
fn notifying_in_database(db: &crate::gatt::GattDatabase) -> Vec<(u16, Option<u16>)> {
    const NOTIFYING: u8 = crate::gatt::CharacteristicProperties::NOTIFY
        | crate::gatt::CharacteristicProperties::INDICATE;
    db.attributes
        .values()
        .filter(|attribute| attribute.uuid == Uuid::CHARACTERISTIC)
        .filter_map(|attribute| {
            let [properties, low, high, ..] = attribute.value[..] else {
                return None;
            };
            if properties & NOTIFYING == 0 {
                return None;
            }
            let value_handle = u16::from_le_bytes([low, high]);
            Some((value_handle, find_cccd_after(db, value_handle)))
        })
        .collect()
}

/// Walks the live GATT database and reports the services it holds that the
/// script's own service list does not know about.
///
/// A service registered by a Rust profile registrar (`add_vcs`, `add_pacs`,
/// `add_ascs`, `add_ras`, …) exists only in the `GattDatabase`: it never passed
/// through `server.add_service`, which is what `ScriptGattServer::get_services`
/// enumerates. So a device composed entirely of Rust profiles reported
/// `"services": []` to every consumer — the page's device view, the Explorer,
/// and the MCP status tool — while being a perfectly real device on the air.
/// The Audio page showed "No services yet." for a sink with PACS, ASCS and VCS
/// in its database.
///
/// This is deliberately *additive*: the script's own list is emitted first and
/// unchanged, so every existing page renders exactly what it rendered before,
/// and profile-registered services are appended rather than replacing it.
fn database_only_services(
    db: &crate::gatt::GattDatabase,
    known: &[String],
    subscribed_handles: &[u16],
) -> Vec<ServiceStatus> {
    let mut services: Vec<ServiceStatus> = Vec::new();
    for attribute in db.attributes.values() {
        match attribute.uuid {
            Uuid::PRIMARY_SERVICE | Uuid::SECONDARY_SERVICE => {
                let Some(uuid) = Uuid::from_bytes(&attribute.value) else {
                    continue;
                };
                services.push(ServiceStatus {
                    uuid: uuid.to_string(),
                    characteristics: Vec::new(),
                });
            }
            Uuid::CHARACTERISTIC => {
                // Characteristic declaration value (Core Vol 3, Part G,
                // Section 3.3.1): [properties(1), value_handle(2), uuid].
                let Some(service) = services.last_mut() else {
                    continue;
                };
                let value = &attribute.value;
                if value.len() < 5 {
                    continue;
                }
                let value_handle = u16::from_le_bytes([value[1], value[2]]);
                let Some(uuid) = Uuid::from_bytes(&value[3..]) else {
                    continue;
                };
                service.characteristics.push(CharacteristicStatus {
                    uuid: uuid.to_string(),
                    properties: value[0] as i64,
                    value: db.value(value_handle).map(hex).unwrap_or_default(),
                    subscribed: subscribed_handles.contains(&value_handle),
                });
            }
            _ => {}
        }
    }
    services.retain(|service| !known.contains(&service.uuid));
    services
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
    /// Decodes HID input reports as they are notified. Inert unless
    /// `hid_start` has found a HID service on this peer.
    hid: crate::device::HidHost,
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
            hid: crate::device::HidHost::new(),
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

    /// True once the connection is up and its GATT has been discovered.
    ///
    /// These four accessors exist for [`WebSource`](web::WebSource), which is
    /// browser-only, so they are gated with it rather than carried as dead
    /// code in native builds.
    #[cfg(target_arch = "wasm32")]
    fn is_ready(&self) -> bool {
        self.phase == CentralPhase::Ready
    }

    /// True when every queued operation has been sent *and* answered — the
    /// only safe moment to act on the result of a sequence of writes, since
    /// a queued operation has not reached the peer yet.
    #[cfg(target_arch = "wasm32")]
    fn is_idle(&self) -> bool {
        self.pending_ops.is_empty() && self.in_flight.is_none()
    }

    /// The ACL connection handle, which a CIS is opened against.
    #[cfg(target_arch = "wasm32")]
    fn connection_handle(&self) -> u16 {
        self.client.connection_handle
    }

    /// The value handle of a discovered characteristic, by UUID.
    #[cfg(target_arch = "wasm32")]
    fn characteristic_handle(&self, uuid: crate::types::Uuid) -> Option<u16> {
        self.client
            .find_characteristic(uuid)
            .map(|characteristic| characteristic.value_handle)
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
    ///
    /// LE Create Connection takes 25 parameter bytes (Vol 4, Part E, Section
    /// 7.8.12). This used to send only the first 12 — everything from
    /// Own_Address_Type onwards was missing. The in-process controller reads
    /// the fields it needs at their fixed offsets and connected regardless,
    /// so every scene test passed; a real controller rejects the command
    /// outright and the central then sits in "connecting" having never
    /// transmitted.
    fn produce(&mut self, channel: &HciChannel) {
        if !self.connect_requested && self.phase == CentralPhase::Connecting {
            // A Zephyr/nRF controller has no public address in ROM; connecting
            // as own=public from 00:00:00:00:00:00 makes the peer drop the link
            // during negotiate (HCI status 0x3E). Set a static random address
            // (two MSBs = 0b11) and initiate as own=random, matching LeCentral.
            const RANDOM_ADDRESS: [u8; 6] = [0xC1, 0x00, 0x00, 0xC0, 0xDE, 0xF0];
            let _ = queue_command(channel, [0x05, 0x20], &RANDOM_ADDRESS); // LE Set Random Address
            let mut params = Vec::with_capacity(25);
            // Scan window == interval: initiate with a continuous scan so the
            // peer's connectable advert is never missed.
            params.extend_from_slice(&0x0060u16.to_le_bytes()); // scan interval, 60 ms
            params.extend_from_slice(&0x0060u16.to_le_bytes()); // scan window, 60 ms (continuous)
            params.push(0x00); // initiator filter policy: use the peer address
            params.push(0x00); // peer address type: public
            let mut peer = self.target.to_be_bytes();
            peer.reverse(); // little-endian on the wire
            params.extend_from_slice(&peer);
            params.push(0x01); // own address type: random (the address set above)
            params.extend_from_slice(&0x0018u16.to_le_bytes()); // min interval, 30 ms
            params.extend_from_slice(&0x0028u16.to_le_bytes()); // max interval, 50 ms
            params.extend_from_slice(&0x0000u16.to_le_bytes()); // max latency
            params.extend_from_slice(&0x0190u16.to_le_bytes()); // supervision timeout, 4 s
            params.extend_from_slice(&0x0000u16.to_le_bytes()); // min CE length
            params.extend_from_slice(&0x0000u16.to_le_bytes()); // max CE length
            debug_assert_eq!(params.len(), 25, "LE Create Connection is 25 bytes");
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
        // The typed header splits handle from value, so the value slice cannot
        // drift out of step with the offset the handle was read from.
        if op == att_op::HANDLE_VALUE_NTF
            && let Some((header, value)) = AttHandleValueHeader::parse(att)
        {
            let value_handle = header.handle.get();
            self.values.insert(value_handle, value.to_vec());
            // Decoded here rather than by polling `values`: consecutive input
            // reports overwrite each other in that map, and a HID host that
            // missed one would lose a keystroke or a click edge.
            self.hid.on_notification(value_handle, value);
            return;
        }

        match self.phase {
            CentralPhase::ExchangingMtu => {
                if op == att_op::EXCHANGE_MTU_RSP
                    && let Some((rsp, _)) = AttExchangeMtuRsp::parse(att)
                {
                    self.client
                        .on_exchange_mtu_response(rsp.server_rx_mtu.get(), 517);
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
                            self.hid.on_read(h, &att[1..]);
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

    // --- HID host ----------------------------------------------------------
    // The HOGP client half: a real computer's response to discovering a
    // keyboard. Kept here rather than in a second central because everything
    // it needs — the discovered GATT, the read/write queue, the notification
    // path — already exists on this type; only the decoding is new, and that
    // lives in [`crate::device::HidHost`].

    /// Starts driving the peer as a HID device: reads its Report Map and
    /// subscribes to every input Report. Returns false if discovery has not
    /// finished or the peer exposes no HID service.
    fn hid_start(&mut self) -> bool {
        if self.phase != CentralPhase::Ready {
            return false;
        }
        let plan = self.hid.plan(&self.client.services);
        if plan.is_empty() {
            return false;
        }
        // The Report Map is read first so the reports that follow can be
        // decoded; the operation queue preserves that order.
        for handle in plan.read {
            self.queue_read(handle);
        }
        for handle in plan.subscribe {
            self.queue_subscribe(handle);
        }
        true
    }

    /// The input decoded since the last call, plus what the host has learned
    /// about the device: `{kind, ready, report_map, report, report_handle,
    /// events:[…]}`. Draining, so a page polling each frame sees every event
    /// exactly once. `report` is the raw bytes those events were decoded
    /// from, so a caller can show the wire beside its meaning.
    fn hid_events_json(&mut self) -> String {
        #[derive(serde::Serialize)]
        struct View {
            kind: &'static str,
            ready: bool,
            /// The Report Map as uppercase hex, once read.
            report_map: Option<String>,
            /// The most recent input report, space-separated hex.
            report: Option<String>,
            /// The value handle that report arrived on.
            report_handle: Option<u16>,
            events: Vec<crate::device::HidEvent>,
        }
        let last = self.hid.last_report();
        let view = View {
            kind: self.hid.kind().label(),
            ready: self.hid.is_ready(),
            report_map: self
                .hid
                .report_map()
                .map(|m| m.iter().map(|b| format!("{b:02X}")).collect()),
            report: last.map(|(_, bytes)| {
                bytes
                    .iter()
                    .map(|b| format!("{b:02X}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            }),
            report_handle: last.map(|(handle, _)| handle),
            events: self.hid.drain_events(),
        };
        serde_json::to_string(&view).unwrap_or_else(|_| "{}".to_string())
    }
}

// ---------------------------------------------------------------------------
// BR/EDR devices in a scene
// ---------------------------------------------------------------------------

use crate::classic::rfcomm::RFCOMM_PSM;
use crate::classic::sdp::{SDP_PSM, SdpServer, SdpUuid, Service};
use crate::device::classic_host::{self, spp_service_record};
use crate::device::{
    ClassicHost, DiscoveredDevice, RfcommHandler, SdpHandler, SdpQueryHandler, SharedRfcommPort,
    SharedSdpQueryResults,
};

/// The Serial Port Profile service class (Assigned Numbers) — what an SPP
/// record advertises itself as, and what a client searches for.
const SERIAL_PORT_SERVICE_CLASS: SdpUuid = SdpUuid::Uuid16(0x1101);

/// How far a classic client has got through its plan.
///
/// The phases are the real BR/EDR connection sequence, in order, and each
/// one is entered only when the previous one's *event* arrived. That is the
/// point of naming them: a client stuck in `Paging` has not been refused, it
/// has been left without a Connection Complete — which is the failure this
/// whole layer exists to make visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassicPhase {
    /// Bring-up commands have not been queued yet.
    Starting,
    /// HCI Inquiry is running; waiting for Inquiry Complete.
    Inquiring,
    /// Resolving the names of what the inquiry found.
    ResolvingNames,
    /// Paging the target; waiting for Connection Complete.
    Paging,
    /// Opening the SDP channel and asking what the peer offers.
    QueryingSdp,
    /// Opening RFCOMM on the server channel SDP advertised.
    OpeningRfcomm,
    /// The data link is open and bytes are moving.
    Exchanging,
    /// Everything asked for was done and the link was torn down.
    Done,
    /// The plan could not continue; see `ClassicDevice::error`.
    Failed,
    /// This device answers rather than initiates, so it has no plan.
    Accepting,
}

impl ClassicPhase {
    /// Stable identifier for a UI or a status document.
    pub fn name(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Inquiring => "inquiring",
            Self::ResolvingNames => "resolving-names",
            Self::Paging => "paging",
            Self::QueryingSdp => "querying-sdp",
            Self::OpeningRfcomm => "opening-rfcomm",
            Self::Exchanging => "exchanging",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Accepting => "accepting",
        }
    }
}

/// One BR/EDR device in a [`SceneEngine`]: a [`ClassicHost`], the plan it is
/// following, and the handles a test or a page reads its progress from.
///
/// A device with no `target` is the *acceptor*: it makes itself discoverable
/// and connectable, serves SDP and RFCOMM, and waits. A device with a target
/// is the *initiator*, and runs the sequence a phone runs — inquire, resolve
/// the name, page, query SDP, open the advertised RFCOMM channel, exchange
/// data, disconnect.
pub struct ClassicDevice {
    host: ClassicHost,
    /// Who to connect to. `None` makes this an acceptor.
    target: Option<Address>,
    /// The Scan Enable this device brings up with. An acceptor that is not
    /// discoverable is a legitimate thing to want to test.
    scan_enable: u8,
    phase: ClassicPhase,
    /// The RFCOMM service class the client looks for in the peer's SDP.
    wanted_service: SdpUuid,
    sdp_results: Option<SharedSdpQueryResults>,
    port: Option<SharedRfcommPort>,
    /// What the client writes once the DLC opens.
    to_send: Vec<u8>,
    /// What came back over the serial port.
    received: Vec<Vec<u8>>,
    /// When set, the plan stops at [`ClassicPhase::Exchanging`] and stays
    /// there: the link is a *seam* for someone else — a profile holding the
    /// port — rather than a errand to run and finish. Without it the plan
    /// disconnects as soon as one payload comes back, which is right for the
    /// send-one-thing demo it was written for and fatal for a conversation.
    hold_open: bool,
    /// Set by [`Self::request_sco`]: open the audio connection as soon as
    /// there is an ACL to hang it off. Held as a *request* rather than acted
    /// on immediately because a profile decides there is audio long before
    /// this device's plan reaches a point where it can send HCI.
    sco_requested: bool,
    /// Whether *this* device opened the audio connection.
    ///
    /// Only the end that opened it may hang it up. Without this, a device
    /// that merely *accepted* an inbound synchronous request sees a link it
    /// never asked for and disconnects it — inside the same tick it came up,
    /// so from the outside the audio never appears at all and no layer
    /// reports an error.
    sco_opened_here: bool,
    /// Call audio queued for the synchronous link, waiting for it to exist.
    sco_to_send: Vec<Vec<u8>>,
    error: Option<String>,
}

impl ClassicDevice {
    /// An acceptor: discoverable, connectable, serving SDP plus an echoing
    /// RFCOMM port on `rfcomm_channel`, advertised in its SDP record under
    /// the Serial Port service class.
    pub fn acceptor(name: &str, class_of_device: [u8; 3], rfcomm_channel: u8) -> Self {
        let (rfcomm, port) = RfcommHandler::echoing(rfcomm_channel);
        Self::accepting(
            name,
            class_of_device,
            vec![(
                0x00010001,
                spp_service_record(0x00010001, rfcomm_channel, name),
            )],
            rfcomm,
            port,
        )
    }

    /// An acceptor that serves the SDP `records` it is given and an RFCOMM
    /// responder on `rfcomm_channel`, handing back the port so the *caller's*
    /// profile drives the serial connection.
    ///
    /// [`Self::acceptor`] is this with an SPP record and a port that echoes.
    /// A profile whose answer is not "the bytes you just sent" — HFP's Audio
    /// Gateway, for one — needs its own record and its own hand on the port,
    /// and that is the whole difference.
    pub fn serving(
        name: &str,
        class_of_device: [u8; 3],
        rfcomm_channel: u8,
        records: Vec<(u32, Service)>,
    ) -> (Self, SharedRfcommPort) {
        let port: SharedRfcommPort =
            std::sync::Arc::new(std::sync::Mutex::new(crate::device::RfcommPort::default()));
        let rfcomm = RfcommHandler::new(rfcomm_channel, port.clone());
        let mut device = Self::accepting(name, class_of_device, records, rfcomm, port.clone());
        device.hold_open = true;
        (device, port)
    }

    /// The shared body of [`Self::acceptor`] and [`Self::serving`].
    fn accepting(
        name: &str,
        class_of_device: [u8; 3],
        records: Vec<(u32, Service)>,
        rfcomm: RfcommHandler,
        port: SharedRfcommPort,
    ) -> Self {
        let mut host = ClassicHost::new(name, class_of_device);
        let mut sdp = SdpHandler::new(SdpServer::new());
        for (handle, record) in records {
            sdp.server_mut().service_records.insert(handle, record);
        }
        let _ = host.register_handler(Box::new(sdp));
        let _ = host.register_handler(Box::new(rfcomm));
        Self {
            host,
            target: None,
            scan_enable: classic_host::scan_enable::INQUIRY_AND_PAGE,
            phase: ClassicPhase::Accepting,
            wanted_service: SERIAL_PORT_SERVICE_CLASS,
            sdp_results: None,
            port: Some(port),
            to_send: Vec::new(),
            received: Vec::new(),
            hold_open: false,
            sco_requested: false,
            sco_opened_here: false,
            sco_to_send: Vec::new(),
            error: None,
        }
    }

    /// An initiator that discovers `target`, opens its Serial Port service
    /// and sends `payload` over it.
    pub fn initiator(
        name: &str,
        class_of_device: [u8; 3],
        target: Address,
        payload: impl Into<Vec<u8>>,
    ) -> Self {
        let mut device = Self::seeking(name, class_of_device, target, SERIAL_PORT_SERVICE_CLASS);
        device.to_send = payload.into();
        device
    }

    /// An initiator that discovers `target`, searches its SDP server for
    /// `service`, opens the RFCOMM channel that record advertises — and then
    /// **stops**, holding the data link open for the caller to drive through
    /// the port it returns.
    ///
    /// [`Self::initiator`] is a whole errand: find a serial port, say one
    /// thing on it, hang up. This is the same machinery with the errand
    /// removed, which is what a profile above RFCOMM needs — the conversation
    /// belongs to the profile, and the link must outlive the first payload
    /// rather than being torn down by it.
    pub fn client(
        name: &str,
        class_of_device: [u8; 3],
        target: Address,
        service: SdpUuid,
    ) -> (Self, SharedRfcommPort) {
        let port: SharedRfcommPort =
            std::sync::Arc::new(std::sync::Mutex::new(crate::device::RfcommPort::default()));
        let mut device = Self::seeking(name, class_of_device, target, service);
        device.hold_open = true;
        // The port is created here rather than in `advance_sdp`, so the
        // profile above can start writing into it before SDP has answered —
        // there is nowhere else to put bytes it produces while connecting.
        device.port = Some(port.clone());
        (device, port)
    }

    /// The shared body of [`Self::initiator`] and [`Self::client`]: a device
    /// that inquires for `target` and searches its SDP for `service`.
    fn seeking(name: &str, class_of_device: [u8; 3], target: Address, service: SdpUuid) -> Self {
        let mut host = ClassicHost::new(name, class_of_device);
        let (sdp, results) = SdpQueryHandler::searching_with_profile_version(service);
        let _ = host.register_handler(Box::new(sdp));
        Self {
            host,
            target: Some(target),
            // A client need be neither discoverable nor connectable: it is
            // the one doing the finding.
            scan_enable: classic_host::scan_enable::NONE,
            phase: ClassicPhase::Starting,
            wanted_service: service,
            sdp_results: Some(results),
            port: None,
            to_send: Vec::new(),
            received: Vec::new(),
            hold_open: false,
            sco_requested: false,
            sco_opened_here: false,
            sco_to_send: Vec::new(),
            error: None,
        }
    }

    /// This device's SDP query results, for a caller that wants to report
    /// what the search actually cost and found.
    pub fn sdp_results(&self) -> Option<&SharedSdpQueryResults> {
        self.sdp_results.as_ref()
    }

    /// The serial port this device's RFCOMM handler serves, once there is one.
    pub fn port(&self) -> Option<&SharedRfcommPort> {
        self.port.as_ref()
    }

    // --- the audio connection (SCO / eSCO) ---------------------------------

    /// Asks for the audio connection to be opened over this device's ACL.
    ///
    /// Only one end may do this — HFP gives the job to the Audio Gateway —
    /// and it is a request, not a command: the setup goes out on the next
    /// step at which there is an ACL to hang it off.
    pub fn request_sco(&mut self) {
        self.sco_requested = true;
        self.sco_opened_here = true;
    }

    /// Hangs up the audio, leaving the ACL and everything on it alone.
    pub fn release_sco(&mut self) {
        self.sco_requested = false;
        self.sco_to_send.clear();
    }

    /// The Voice Setting and packet types the next setup asks for — the
    /// codec seam (`AudioCodec::voice_setting`/`esco_packet_type`).
    pub fn set_sco_parameters(&mut self, voice_setting: u16, packet_type: u16) {
        self.host.set_sco_parameters(voice_setting, packet_type);
    }

    /// What this device answers an inbound synchronous Connection Request
    /// with.
    pub fn set_sco_policy(&mut self, policy: crate::device::ScoPolicy) {
        self.host.set_sco_policy(policy);
    }

    /// The audio connection, if one is up.
    pub fn sco(&self) -> Option<crate::device::ScoConnection> {
        self.host.sco()
    }

    /// Why the last audio setup failed, if the far end refused it.
    pub fn sco_failure(&self) -> Option<u8> {
        self.host.sco_failure()
    }

    /// Queues one payload for the synchronous link. It waits if the link is
    /// not up yet, rather than being dropped: a profile that starts talking
    /// the instant it decides there is audio is right to, and the frames it
    /// produced while the setup was in flight are still the call.
    pub fn send_sco(&mut self, payload: impl Into<Vec<u8>>) {
        self.sco_to_send.push(payload.into());
    }

    /// Takes the call audio that has arrived on the synchronous link.
    pub fn take_sco_received(&mut self) -> Vec<Vec<u8>> {
        self.host.take_sco_received()
    }

    /// Makes this device's Scan Enable `value` — used to build a device that
    /// is deliberately not discoverable.
    pub fn with_scan_enable(mut self, value: u8) -> Self {
        self.scan_enable = value;
        self
    }

    /// How far the plan has got.
    pub fn phase(&self) -> ClassicPhase {
        self.phase
    }

    /// Why the plan stopped, if it did.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The devices this device's inquiry turned up.
    pub fn discovered(&self) -> &[DiscoveredDevice] {
        self.host.discovered()
    }

    /// The underlying host, for assertions a plan does not cover.
    pub fn host(&self) -> &ClassicHost {
        &self.host
    }

    /// What arrived over the serial port.
    pub fn received(&self) -> &[Vec<u8>] {
        &self.received
    }

    /// Queues this device's bring-up on its channel.
    fn queue_start(&mut self, channel: &HciChannel) {
        for packet in self.host.start_commands() {
            let _ = channel.inject_host_packet(packet);
        }
        for packet in self.host.set_scan_enable(self.scan_enable) {
            let _ = channel.inject_host_packet(packet);
        }
        self.phase = match self.target {
            Some(_) => ClassicPhase::Starting,
            None => ClassicPhase::Accepting,
        };
    }

    fn fail(&mut self, reason: impl Into<String>) {
        self.error = Some(reason.into());
        self.phase = ClassicPhase::Failed;
    }

    /// Advance the plan one step, emitting whatever HCI it asks for.
    ///
    /// Every transition is gated on something the *controller* said, never
    /// on a tick count: that is what makes a stalled step visible as a phase
    /// that stops moving rather than as a scene that silently drifts on.
    fn produce(&mut self, channel: &HciChannel) {
        // The audio connection is orthogonal to the plan below: it hangs off
        // whatever ACL exists, and either role may be the one holding it, so
        // it runs before the acceptor's early return rather than inside the
        // initiator's state machine.
        self.produce_sco(channel);

        let Some(target) = self.target else {
            // An acceptor still has to drain what its profiles want to send.
            for packet in self.host.poll() {
                let _ = channel.inject_host_packet(packet);
            }
            return;
        };

        let packets = match self.phase {
            ClassicPhase::Starting => {
                self.phase = ClassicPhase::Inquiring;
                self.host.start_inquiry(1)
            }
            ClassicPhase::Inquiring => {
                if !self.host.inquiry_finished() {
                    Vec::new()
                } else if self.host.discovered().iter().any(|d| d.address == target) {
                    self.phase = ClassicPhase::ResolvingNames;
                    self.host.request_remote_name(target)
                } else {
                    self.fail(format!("inquiry did not find {target}"));
                    Vec::new()
                }
            }
            ClassicPhase::ResolvingNames => {
                if self.host.name_of(target).is_none() {
                    Vec::new()
                } else {
                    self.phase = ClassicPhase::Paging;
                    self.host.create_connection(target)
                }
            }
            ClassicPhase::Paging => {
                if self.host.connection().is_none() {
                    Vec::new()
                } else {
                    self.phase = ClassicPhase::QueryingSdp;
                    // The SDP query itself leaves on its own, from the
                    // handler's `poll_output`, once this channel opens.
                    self.host.open_channel(SDP_PSM).unwrap_or_default()
                }
            }
            ClassicPhase::QueryingSdp => self.advance_sdp(),
            ClassicPhase::OpeningRfcomm => {
                let open = self
                    .port
                    .as_ref()
                    .and_then(|port| port.lock().ok().map(|port| port.is_open()))
                    .unwrap_or(false);
                if !open {
                    Vec::new()
                } else {
                    self.phase = ClassicPhase::Exchanging;
                    let payload = std::mem::take(&mut self.to_send);
                    // An empty write is a zero-length UIH frame the peer has
                    // to make sense of; a client with nothing to say of its
                    // own should say nothing.
                    if !payload.is_empty()
                        && let Some(port) = self.port.as_ref()
                        && let Ok(mut port) = port.lock()
                    {
                        port.write(payload);
                    }
                    Vec::new()
                }
            }
            ClassicPhase::Exchanging => {
                self.drain_port();
                if self.hold_open || self.received.is_empty() {
                    Vec::new()
                } else {
                    self.phase = ClassicPhase::Done;
                    self.host.disconnect()
                }
            }
            ClassicPhase::Done | ClassicPhase::Failed | ClassicPhase::Accepting => Vec::new(),
        };

        for packet in packets {
            let _ = channel.inject_host_packet(packet);
        }
        // Profiles speak unprompted too — the RFCOMM initiator's SABM leaves
        // this way, not from the plan above.
        for packet in self.host.poll() {
            let _ = channel.inject_host_packet(packet);
        }
    }

    /// The audio connection's step: open it when it has been asked for and
    /// there is an ACL under it, hang it up when the request is withdrawn,
    /// and drain whatever audio is queued for it.
    ///
    /// Every transition here is gated on what the *controller* said, like
    /// every other stage: `host.sco()` is `Some` only once a Synchronous
    /// Connection Complete has arrived, so a setup that is refused leaves
    /// this loop trying again rather than reporting audio nobody agreed to.
    fn produce_sco(&mut self, channel: &HciChannel) {
        let up = self.host.sco().is_some();
        if self.sco_requested && !up && self.host.sco_failure().is_some() {
            // The far end refused. Asking again every step would busy the
            // link and hide the refusal behind a setup that is always "in
            // flight"; the request is withdrawn and the reason stays
            // readable on the host.
            self.sco_requested = false;
            self.sco_opened_here = false;
            self.sco_to_send.clear();
        }
        let packets = if self.sco_requested && !up {
            self.host.setup_sco()
        } else if !self.sco_requested && up && self.sco_opened_here {
            self.sco_opened_here = false;
            self.host.disconnect_sco()
        } else {
            Vec::new()
        };
        for packet in packets {
            let _ = channel.inject_host_packet(packet);
        }
        if self.host.sco().is_some() {
            for payload in std::mem::take(&mut self.sco_to_send) {
                for packet in self.host.send_sco(&payload) {
                    let _ = channel.inject_host_packet(packet);
                }
            }
        }
    }

    /// The SDP stage: wait for the answer, then register an RFCOMM initiator
    /// on the server channel the peer advertised and open its L2CAP channel.
    ///
    /// The handler cannot be registered any earlier: which channel to open a
    /// DLC on is precisely what the SDP query is for, and guessing it is how
    /// a client ends up with a DLC refused by DM.
    fn advance_sdp(&mut self) -> Vec<Vec<u8>> {
        let Some(results) = self.sdp_results.as_ref() else {
            self.fail("no SDP results handle");
            return Vec::new();
        };
        let Ok(results) = results.lock() else {
            return Vec::new();
        };
        if !results.answered {
            return Vec::new();
        }
        if let Some(code) = results.error {
            drop(results);
            self.fail(format!("peer's SDP server returned error {code:#06x}"));
            return Vec::new();
        }
        let Some(rfcomm_channel) = results.channel_for(self.wanted_service) else {
            drop(results);
            self.fail("peer advertises no Serial Port service".to_string());
            return Vec::new();
        };
        drop(results);

        // Reuse the port the caller was already given, if there is one: a
        // profile holding a clone of it must not be handed a *different*
        // port once SDP answers, or everything it queued goes nowhere.
        let port = self.port.clone().unwrap_or_else(|| {
            std::sync::Arc::new(std::sync::Mutex::new(crate::device::RfcommPort::default()))
        });
        let rfcomm = RfcommHandler::initiating(rfcomm_channel, port.clone());
        if let Err(e) = self.host.register_handler(Box::new(rfcomm)) {
            self.fail(e.to_string());
            return Vec::new();
        }
        self.port = Some(port);
        self.phase = ClassicPhase::OpeningRfcomm;
        match self.host.open_channel(RFCOMM_PSM) {
            Ok(packets) => packets,
            Err(e) => {
                self.fail(e.to_string());
                Vec::new()
            }
        }
    }

    /// Move anything the serial port received into this device's record of it.
    ///
    /// A device holding the link open for a profile above it must *not* do
    /// this: the port is the seam, and taking from it here would swallow the
    /// bytes the profile is waiting for. That is not hypothetical — it is
    /// what a plan written for "send one payload and read the echo" does to
    /// its second consumer, silently, with the link up and every phase green.
    fn drain_port(&mut self) {
        if self.hold_open {
            return;
        }
        if let Some(port) = self.port.as_ref()
            && let Ok(mut port) = port.lock()
        {
            self.received.extend(port.take_received());
        }
    }

    /// Everything a page renders about this device's BR/EDR link, as JSON:
    /// the phase, what the inquiry turned up, the ACL connection, and the
    /// state of the serial port on top of it.
    ///
    /// A BR/EDR link has no equivalent of an advertising report to look at,
    /// so a page that shows nothing but "connected" cannot say *how* it
    /// connected — which stage it is stuck in, or whether the peer was even
    /// found. That is what this exists to make visible.
    pub fn status_json(&self) -> String {
        #[derive(serde::Serialize)]
        struct FoundJson {
            address: String,
            class_of_device: String,
            name: Option<String>,
        }
        #[derive(serde::Serialize)]
        struct DlcJson {
            dlci: u8,
            tx_max_frame_size: u16,
            rx_max_frame_size: u16,
            rx_initial_credits: u8,
            credits_out: u8,
            credits_in: u8,
        }
        #[derive(serde::Serialize)]
        struct ClassicJson {
            phase: &'static str,
            error: Option<String>,
            name: String,
            discovered: Vec<FoundJson>,
            acl_handle: Option<u16>,
            peer: Option<String>,
            sdp_channel: Option<u8>,
            sdp_profile_version: Option<u16>,
            sdp_request_bytes: usize,
            sdp_response_bytes: usize,
            dlc: Option<DlcJson>,
            received: usize,
        }

        let connection = self.host.connection();
        let results = self.sdp_results.as_ref().and_then(|r| r.lock().ok());
        let port = self.port.as_ref().and_then(|p| p.lock().ok());
        let status = ClassicJson {
            phase: self.phase.name(),
            error: self.error.clone(),
            name: self.host.name().to_string(),
            discovered: self
                .host
                .discovered()
                .iter()
                .map(|d| FoundJson {
                    address: d.address.to_string(),
                    class_of_device: format!(
                        "{:02X}{:02X}{:02X}",
                        d.class_of_device[2], d.class_of_device[1], d.class_of_device[0]
                    ),
                    name: d.name.clone(),
                })
                .collect(),
            acl_handle: connection.map(|(handle, _)| handle),
            peer: connection.map(|(_, address)| address.to_string()),
            sdp_channel: results
                .as_ref()
                .and_then(|r| r.channel_for(self.wanted_service)),
            sdp_profile_version: results.as_ref().and_then(|r| r.profile_version),
            sdp_request_bytes: results.as_ref().map(|r| r.request_bytes).unwrap_or(0),
            sdp_response_bytes: results.as_ref().map(|r| r.response_bytes).unwrap_or(0),
            dlc: port.as_ref().and_then(|p| p.window()).map(|w| DlcJson {
                dlci: w.dlci,
                tx_max_frame_size: w.tx_max_frame_size,
                rx_max_frame_size: w.rx_max_frame_size,
                rx_initial_credits: w.rx_initial_credits,
                credits_out: w.tx_credits,
                credits_in: w.rx_credits,
            }),
            received: port.as_ref().map(|p| p.received_count()).unwrap_or(0),
        };
        serde_json::to_string(&status).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
    }

    /// Feed one controller packet to the host and send back what it answers.
    fn consume(&mut self, channel: &HciChannel, packet: &[u8]) {
        match self.host.handle_packet(packet) {
            Ok(out) => {
                for reply in out {
                    let _ = channel.inject_host_packet(reply);
                }
            }
            Err(e) => self.fail(e.to_string()),
        }
        self.drain_port();
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
    /// A central whose behaviour is a Rhai script (`android::BluetoothGatt`).
    /// Boxed for the same reason the peripheral is: it carries an engine.
    ScriptedCentral(Box<ScriptedCentral>),
    /// A BR/EDR device — the fifth thing a scene can host, and the only one
    /// that is not LE. Boxed: it carries a whole `ClassicHost`.
    Classic(Box<ClassicDevice>),
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

    /// Adds a *scripted* central at `address`: a Rhai script that builds an
    /// `android::BluetoothGatt`, connects it and reacts in callbacks. Returns
    /// its device index, or the script error.
    ///
    /// The script names its own target with `client.connect("AA:BB:…")`, so
    /// unlike [`Self::add_central`] the scene does not supply one — the
    /// script is the whole behaviour.
    pub fn add_scripted_central(
        &mut self,
        address: Address,
        script: &str,
    ) -> Result<usize, String> {
        let central = ScriptedCentral::run_script(script)?;
        let channel = self.link.add_device(address);
        let index = self.devices.len();
        self.devices.push(SceneDevice {
            channel,
            role: SceneRole::ScriptedCentral(Box::new(central)),
            started: false,
        });
        Ok(index)
    }

    /// Adds a **BR/EDR** device at `address`; returns its device index.
    ///
    /// This is the fifth thing a scene can host, beside the four LE roles
    /// above, and the first that speaks Bluetooth Classic. Build the device
    /// with [`ClassicDevice::acceptor`] (discoverable, connectable, serving
    /// SDP and an echoing RFCOMM port) or [`ClassicDevice::initiator`]
    /// (inquires, pages, queries SDP, opens the advertised serial port).
    ///
    /// Nothing about it is LE: it shares the [`Link`] with the LE devices
    /// because they share a simulated room and an ACL router, not because
    /// they share a transport.
    pub fn add_classic_device(&mut self, address: Address, device: ClassicDevice) -> usize {
        let channel = self.link.add_device(address);
        let index = self.devices.len();
        self.devices.push(SceneDevice {
            channel,
            role: SceneRole::Classic(Box::new(device)),
            started: false,
        });
        index
    }

    /// The classic device at `index`, or `None` if that device is something
    /// else — the handle a test needs for its phase, what its inquiry found,
    /// and what came back over its serial port.
    pub fn classic_device(&self, index: usize) -> Option<&ClassicDevice> {
        match self.devices.get(index)?.role {
            SceneRole::Classic(ref d) => Some(d),
            _ => None,
        }
    }

    /// Mutable access to the BR/EDR device at `index` — what a profile above
    /// the link needs in order to ask for an audio connection or put audio
    /// on it.
    pub fn classic_device_mut(&mut self, index: usize) -> Option<&mut ClassicDevice> {
        match self.devices.get_mut(index)?.role {
            SceneRole::Classic(ref mut d) => Some(d),
            _ => None,
        }
    }

    /// The BR/EDR status JSON of classic device `index` (see
    /// [`ClassicDevice::status_json`]), or `None` if it isn't one.
    pub fn classic_status_json(&self, index: usize) -> Option<String> {
        Some(self.classic_device(index)?.status_json())
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
                let _ = match &mut device.role {
                    SceneRole::Peripheral(p) => p.queue_start(&device.channel),
                    SceneRole::Scanner(_) => queue_scanner_start(&device.channel),
                    // Both centrals queue their own bring-up: the scene one
                    // in `produce`, the scripted one when its script called
                    // `connect`.
                    SceneRole::Central(_) | SceneRole::ScriptedCentral(_) => Ok(()),
                    SceneRole::Classic(c) => {
                        c.queue_start(&device.channel);
                        Ok(())
                    }
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
                SceneRole::ScriptedCentral(c) => {
                    for packet in c.tick(t_seconds) {
                        let _ = device.channel.inject_host_packet(packet);
                    }
                }
                SceneRole::Classic(c) => c.produce(&device.channel),
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
                SceneRole::ScriptedCentral(c) => {
                    while let Some(pkt) = device.channel.poll_controller_packet() {
                        for out in c.on_packet(&pkt) {
                            let _ = device.channel.inject_host_packet(out);
                        }
                    }
                }
                SceneRole::Classic(c) => {
                    while let Some(pkt) = device.channel.poll_controller_packet() {
                        c.consume(&device.channel, &pkt);
                    }
                }
            }
        }
    }

    /// The scripted central at `index`, or `None` if that device is something
    /// else — the handle a host needs for its status, its emitted messages
    /// and whether one of its `assert`s failed.
    pub fn scripted_central(&self, index: usize) -> Option<&ScriptedCentral> {
        match self.devices.get(index)?.role {
            SceneRole::ScriptedCentral(ref c) => Some(c),
            _ => None,
        }
    }

    /// Mutable access to the scripted central at `index` (draining emitted
    /// messages needs it).
    pub fn scripted_central_mut(&mut self, index: usize) -> Option<&mut ScriptedCentral> {
        match self.devices.get_mut(index)?.role {
            SceneRole::ScriptedCentral(ref mut c) => Some(c),
            _ => None,
        }
    }

    /// The GATT status JSON of peripheral `index` (see
    /// [`ScriptedPeripheral::status_json`]), or `None` if it isn't a peripheral.
    pub fn peripheral_status_json(&self, index: usize) -> Option<String> {
        match self.devices.get(index)?.role {
            SceneRole::Peripheral(ref p) => Some(p.status_json()),
            SceneRole::Scanner(_)
            | SceneRole::Central(_)
            | SceneRole::ScriptedCentral(_)
            | SceneRole::Classic(_) => None,
        }
    }

    /// The discovered-GATT JSON of central `index`, or `None` if it isn't a
    /// central.
    pub fn central_status_json(&self, index: usize) -> Option<String> {
        match self.devices.get(index)?.role {
            SceneRole::Central(ref c) => Some(c.status_json()),
            SceneRole::ScriptedCentral(ref c) => Some(c.status_json()),
            SceneRole::Peripheral(_) | SceneRole::Scanner(_) | SceneRole::Classic(_) => None,
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

    /// Host-writes `value` into characteristic `uuid` of peripheral `index`
    /// and notifies it even when the bytes are unchanged — see
    /// [`ScriptedPeripheral::notify_characteristic_value`].
    pub fn peripheral_notify_value(
        &mut self,
        index: usize,
        uuid: &str,
        value: &[u8],
    ) -> Result<(), String> {
        match self.devices.get_mut(index).map(|d| &mut d.role) {
            Some(SceneRole::Peripheral(p)) => p.notify_characteristic_value(uuid, value),
            _ => Err("not a peripheral".to_string()),
        }
    }

    /// Drives central `index` as a HID host: reads the peer's Report Map and
    /// subscribes to its input Reports. Returns false until the central has
    /// finished discovery, so a caller polls this once per tick until it
    /// takes.
    pub fn central_start_hid(&mut self, index: usize) -> bool {
        match self.devices.get_mut(index).map(|d| &mut d.role) {
            Some(SceneRole::Central(c)) => c.hid_start(),
            _ => false,
        }
    }

    /// The HID input central `index` has decoded since the last call (see
    /// `CentralDevice::hid_events_json`).
    pub fn central_hid_events_json(&mut self, index: usize) -> String {
        match self.devices.get_mut(index).map(|d| &mut d.role) {
            Some(SceneRole::Central(c)) => c.hid_events_json(),
            _ => "{}".to_string(),
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
#[path = "wasm_ws_scene_tests.rs"]
mod scene_tests;

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
        pub fn encode(
            &mut self,
            samples: Vec<i16>,
            frame_bytes: usize,
        ) -> Result<Vec<u8>, JsValue> {
            self.encoder.encode(&samples, frame_bytes).map_err(js_error)
        }

        /// Decodes one LC3 frame back to 16-bit PCM.
        pub fn decode(&mut self, frame: Vec<u8>) -> Result<Vec<i16>, JsValue> {
            self.decoder.decode(&frame).map_err(js_error)
        }
    }

    /// timer, and read each device's state — the browser pages use this when
    /// the backend selector is set to "in-page". Wraps [`SceneEngine`].
    /// The ranging demo's scene: a tag and a locator on one simulated
    /// medium, measured both by RSSI and by Channel Sounding.
    ///
    /// The page owns one timer and calls [`WebRanging::tick`]; everything
    /// else it shows comes out of [`WebRanging::status_json`], which reports
    /// the ground truth alongside both estimates and the raw measurements
    /// behind them. See [`crate::device::ranging_scene`].
    #[wasm_bindgen]
    pub struct WebRanging {
        scene: crate::device::RangingScene,
    }

    #[wasm_bindgen]
    impl WebRanging {
        /// Creates the scene with a tag and a locator at the given addresses.
        #[wasm_bindgen(constructor)]
        pub fn new(tag: &str, locator: &str) -> Result<WebRanging, JsValue> {
            install_panic_hook();
            Ok(Self {
                scene: crate::device::RangingScene::new(
                    tag.parse().map_err(js_error)?,
                    locator.parse().map_err(js_error)?,
                ),
            })
        }

        /// Advances both devices one step.
        pub fn tick(&mut self) {
            self.scene.tick();
        }

        /// Moves the tag to `(x, y)` metres on the floor plan.
        pub fn set_tag_position(&mut self, x: f64, y: f64) {
            self.scene
                .set_tag_position(crate::controller::propagation::Position::new(x, y));
        }

        /// Sets the room the radio propagates through: the transmit power in
        /// dBm, the path-loss exponent, and the shadowing standard deviation
        /// in dB. These are the *truth*; the locator does not learn them.
        pub fn set_room(&mut self, tx_power_dbm: f64, path_loss_exponent: f64, shadowing_db: f64) {
            let mut model = self.scene.path_loss();
            model.tx_power_dbm = tx_power_dbm;
            model.path_loss_exponent = path_loss_exponent;
            model.shadowing_sigma_db = shadowing_db;
            self.scene.set_path_loss(model);
        }

        /// Sets what the locator's RSSI estimator *assumes*: the calibrated
        /// one-metre RSSI and the path-loss exponent. Changing these
        /// re-derives the estimate from samples already collected.
        pub fn set_rssi_assumptions(&mut self, reference_dbm: f64, path_loss_exponent: f64) {
            self.scene
                .set_rssi_assumptions(crate::cs::RssiRangingParams {
                    reference_rssi_dbm: reference_dbm,
                    path_loss_exponent,
                });
        }

        /// Reseeds the medium's noise, so a run repeats exactly.
        pub fn set_noise_seed(&mut self, seed: f64) {
            self.scene.set_noise_seed(seed as u64);
        }

        /// The whole scene as JSON: truth, room, link state, and both
        /// methods' inputs, estimates, and errors.
        pub fn status_json(&self) -> String {
            self.scene.status_json()
        }
    }

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

        /// Adds a **BR/EDR** device at `address` — the fifth thing a scene can
        /// host, and the only one that is not LE.
        ///
        /// `role` is `"acceptor"` for a device that makes itself discoverable
        /// and connectable and serves an echoing serial port on
        /// `rfcomm_channel`, or `"initiator"` for one that inquires for
        /// `target`, resolves its name, pages it, queries its SDP, opens the
        /// serial port that record advertises and sends `payload` over it.
        /// `target` is ignored for an acceptor, and `rfcomm_channel` for an
        /// initiator — which channel to open is exactly what SDP is asked.
        ///
        /// Read its progress back with
        /// [`Self::classic_status_json`]; a BR/EDR link has no advertising
        /// report to watch, so the phase list is the only view of it there is.
        pub fn add_classic_device(
            &mut self,
            address: &str,
            role: &str,
            name: &str,
            rfcomm_channel: u8,
            target: &str,
            payload: &str,
        ) -> Result<usize, JsValue> {
            let address = address.parse().map_err(js_error)?;
            let device = match role {
                "acceptor" => super::ClassicDevice::acceptor(
                    name,
                    // Rendering / audio-video, wearable headset: what a
                    // simble peripheral has always claimed to be.
                    [0x04, 0x04, 0x24],
                    rfcomm_channel,
                ),
                "initiator" => super::ClassicDevice::initiator(
                    name,
                    [0x0C, 0x02, 0x5A], // smartphone
                    target.parse().map_err(js_error)?,
                    payload.as_bytes().to_vec(),
                ),
                other => {
                    return Err(js_error(format!(
                        "unknown classic role {other:?}: expected \"acceptor\" or \"initiator\""
                    )));
                }
            };
            Ok(self.scene.add_classic_device(address, device))
        }

        /// The BR/EDR status of classic device `index`: its phase, what its
        /// inquiry found, the ACL connection, what SDP answered, and the
        /// RFCOMM data link's credit window. `undefined` if that device is
        /// not a classic one.
        pub fn classic_status_json(&self, index: usize) -> Option<String> {
            self.scene.classic_status_json(index)
        }

        /// Adds a *scripted* central at `address` — a Rhai script that builds
        /// an `android::BluetoothGatt`, connects it, and reacts in callbacks.
        /// Returns its device index, or the script/address error, so a page
        /// can show a compile failure on the line that caused it.
        pub fn add_scripted_central(
            &mut self,
            address: &str,
            script: &str,
        ) -> Result<usize, JsValue> {
            let address = address.parse().map_err(js_error)?;
            self.scene
                .add_scripted_central(address, script)
                .map_err(js_error)
        }

        /// Points scripted central `index` at `target`, overriding the address
        /// its script named — for a page that allocates addresses itself.
        pub fn scripted_central_set_target(
            &mut self,
            index: usize,
            target: &str,
        ) -> Result<(), JsValue> {
            let target = target.parse().map_err(js_error)?;
            match self.scene.scripted_central_mut(index) {
                Some(central) => {
                    central.set_target(target);
                    Ok(())
                }
                None => Err(js_error("not a scripted central")),
            }
        }

        /// Drains what scripted central `index` emitted with
        /// `client.emit(kind, payload)` — the script's channel to the page.
        pub fn scripted_central_emitted(&mut self, index: usize) -> js_sys::Array {
            let out = js_sys::Array::new();
            if let Some(central) = self.scene.scripted_central_mut(index) {
                for message in central.take_emitted() {
                    out.push(&JsValue::from_str(&message));
                }
            }
            out
        }

        /// Queues a read on scripted central `index`, naming the
        /// characteristic by UUID string (`"2A37"` or a full 128-bit UUID) —
        /// what the discovered tree a page renders already holds.
        pub fn scripted_central_read(&mut self, index: usize, uuid: &str) -> Result<(), JsValue> {
            let uuid: crate::types::Uuid = uuid.parse().map_err(js_error)?;
            match self.scene.scripted_central_mut(index) {
                Some(central) => {
                    central.read(uuid);
                    Ok(())
                }
                None => Err(js_error("not a scripted central")),
            }
        }

        /// Queues a write (Write Request) on scripted central `index`.
        pub fn scripted_central_write(
            &mut self,
            index: usize,
            uuid: &str,
            value: Vec<u8>,
        ) -> Result<(), JsValue> {
            let uuid: crate::types::Uuid = uuid.parse().map_err(js_error)?;
            match self.scene.scripted_central_mut(index) {
                Some(central) => {
                    central.write(uuid, value, true);
                    Ok(())
                }
                None => Err(js_error("not a scripted central")),
            }
        }

        /// Queues enabling (or disabling) notifications on scripted central
        /// `index`.
        pub fn scripted_central_subscribe(
            &mut self,
            index: usize,
            uuid: &str,
            enable: bool,
        ) -> Result<(), JsValue> {
            let uuid: crate::types::Uuid = uuid.parse().map_err(js_error)?;
            match self.scene.scripted_central_mut(index) {
                Some(central) => {
                    central.subscribe(uuid, enable);
                    Ok(())
                }
                None => Err(js_error("not a scripted central")),
            }
        }

        /// The first error a scripted central's callbacks raised — a failed
        /// `assert`, or an operation naming a characteristic the peer does not
        /// have. `undefined` while the script is behaving.
        pub fn scripted_central_failure(&self, index: usize) -> Option<String> {
            self.scene
                .scripted_central(index)
                .and_then(|c| c.failure().map(str::to_string))
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

        /// Host-write `value` into characteristic `uuid` of peripheral
        /// `index` and notify it even if the bytes are unchanged — what a
        /// report of *change* (a relative mouse report, a repeated key)
        /// needs.
        pub fn peripheral_notify_value(
            &mut self,
            index: usize,
            uuid: &str,
            value: Vec<u8>,
        ) -> Result<(), JsValue> {
            self.scene
                .peripheral_notify_value(index, uuid, &value)
                .map_err(js_error)
        }

        /// Drive central `index` as a HID host — read the peer's Report Map
        /// and subscribe to its input Reports. False until discovery is done,
        /// so a page calls it each tick until it takes.
        pub fn central_start_hid(&mut self, index: usize) -> bool {
            self.scene.central_start_hid(index)
        }

        /// The HID input central `index` has decoded since the last call:
        /// `{kind, ready, report_map, events:[…]}`. Draining.
        pub fn central_hid_events_json(&mut self, index: usize) -> String {
            self.scene.central_hid_events_json(index)
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

        /// Writes a characteristic and notifies it even when the bytes are
        /// unchanged — the netsim counterpart of
        /// `WebLink::peripheral_notify_value`, and the reason the HID domain
        /// can run here at all. See
        /// [`ScriptedPeripheral::notify_characteristic_value`]: a HID input
        /// report describes *change*, so two identical reports are two
        /// events, and the value-diff that is right for a battery level would
        /// swallow the second of them.
        pub fn notify_value(&mut self, uuid: &str, value: Vec<u8>) -> Result<(), JsValue> {
            self.peripheral
                .notify_characteristic_value(uuid, &value)
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

    /// An LE Audio **source** hosted in the page and running on netsim: the
    /// central that connects to a sink, configures its endpoint, opens a real
    /// CIS, and streams SDUs to it.
    ///
    /// [`WebPeripheral`] is the sink half. Until this existed a foreign stack
    /// had to be the source for any LE Audio test, because simble could
    /// accept a CIS but never open one. The pieces it drives —
    /// [`CisCentral`](crate::device::CisCentral) for the media plane and
    /// [`AseConfig`](crate::profiles::ascs_client::AseConfig) for the control
    /// plane — live in the library, so this type is only the browser's
    /// WebSocket and a running order.
    #[wasm_bindgen]
    pub struct WebSource {
        transport: WasmWsTransport,
        channel: HciChannel,
        central: super::CentralDevice,
        cis: crate::device::CisCentral,
        ase: crate::profiles::ascs_client::AseConfig,
        started: bool,
        /// Whether the three ASE Control Point writes have been queued.
        ase_requested: bool,
        /// Whether CIS establishment has been kicked off.
        cis_requested: bool,
        /// SDUs handed over before the stream was ready, so audio can be
        /// queued the moment a file is loaded rather than only after the
        /// handshake finishes.
        pending_audio: VecDeque<Vec<u8>>,
        /// SDUs discarded while waiting for the stream to open.
        dropped: u32,
        error: Option<String>,
    }

    #[wasm_bindgen]
    impl WebSource {
        /// Connects a source to netsim at `url` and aims it at `target`
        /// (e.g. "CC:1E:57:00:00:06").
        #[wasm_bindgen(constructor)]
        pub fn new(url: &str, target: &str) -> Result<WebSource, JsValue> {
            install_panic_hook();
            let target: Address = target
                .parse()
                .map_err(|_| JsValue::from_str("target is not a Bluetooth address"))?;
            let url = ws_url_with_wire_address(url);
            Ok(Self {
                transport: WasmWsTransport::connect(&url)?,
                channel: HciChannel::new(),
                central: super::CentralDevice::new(target),
                cis: crate::device::CisCentral::new(crate::device::CisConfig::default()),
                ase: crate::profiles::ascs_client::AseConfig::default(),
                started: false,
                ase_requested: false,
                cis_requested: false,
                pending_audio: VecDeque::new(),
                dropped: 0,
                error: None,
            })
        }

        /// Returns the underlying WebSocket ready state (per the WebSocket API).
        pub fn ready_state(&self) -> u16 {
            self.transport.ready_state()
        }

        /// True once SDUs handed to [`send_audio`](Self::send_audio) will
        /// actually reach the sink.
        pub fn is_streaming(&self) -> bool {
            self.cis.is_streaming()
        }

        /// Hands one SDU to the stream.
        ///
        /// Once the stream is up this goes straight out. The queue exists
        /// only to bridge the gap before the CIS opens, so that a page can
        /// start feeding a decoded file the moment the user picks it — it is
        /// deliberately not in the streaming path. Putting it there cost
        /// real audio: a throttled browser tab wakes rarely and then hands
        /// over a large burst, which overran the bound and silently
        /// discarded the overflow.
        ///
        /// Pre-stream buffering is still bounded, because a handshake that
        /// never completes must not grow the queue without limit; what it
        /// drops is counted rather than lost quietly.
        pub fn send_audio(&mut self, sdu: Vec<u8>) {
            if self.cis.is_streaming() {
                if let Some(packet) = self.cis.send_sdu(&sdu) {
                    let _ = self.channel.inject_host_packet(packet);
                }
                return;
            }
            self.pending_audio.push_back(sdu);
            while self.pending_audio.len() > 200 {
                self.pending_audio.pop_front();
                self.dropped += 1;
            }
        }

        /// One pump: bring the controller up, advance the connection, the ASE
        /// configuration and the CIS, then drain queued audio onto the
        /// stream. Returns render-ready status JSON.
        pub fn tick(&mut self) -> Result<String, JsValue> {
            self.transport.pump(&self.channel)?;

            if !self.started && self.transport.is_open() {
                // Reset and both event masks, then the CIS host feature —
                // which must be declared before any connection exists, or LE
                // Create CIS is refused later for reasons that look unrelated.
                for packet in crate::device::host::init_commands().into_iter().take(3) {
                    self.channel.send_command(&packet[1..]).map_err(js_error)?;
                }
                for packet in crate::device::CisCentral::init_commands() {
                    self.channel.send_command(&packet[1..]).map_err(js_error)?;
                }
                self.started = true;
            }

            while let Some(packet) = self.channel.poll_controller_packet() {
                self.central.consume(&self.channel, &packet);
                for command in self.cis.on_packet(&packet) {
                    self.channel.send_command(&command[1..]).map_err(js_error)?;
                }
            }
            self.central.produce(&self.channel);

            self.advance_stream();
            self.drain_audio();
            self.transport.pump(&self.channel)?;
            Ok(self.status_json())
        }

        /// Drives the control plane: configure the endpoint once discovery
        /// finds it, then open the stream once those writes have landed.
        fn advance_stream(&mut self) {
            if !self.central.is_ready() {
                return;
            }
            if !self.ase_requested {
                let uuid = crate::profiles::ascs::ascs_uuid::ASE_CONTROL_POINT;
                let Some(control_point) = self.central.characteristic_handle(uuid) else {
                    self.error = Some(
                        "the peer has no ASE Control Point — it is not an LE Audio sink".into(),
                    );
                    self.ase_requested = true;
                    return;
                };
                // Queued together: the central sends one at a time and waits
                // for each response, so this is the ASCS order, not a burst.
                self.central
                    .queue_write(control_point, self.ase.config_codec());
                self.central
                    .queue_write(control_point, self.ase.config_qos());
                self.central.queue_write(control_point, self.ase.enable());
                self.ase_requested = true;
                return;
            }
            // The endpoint is Enabling once the writes have drained; only
            // then does opening a CIS mean anything.
            if !self.cis_requested && self.central.is_idle() && self.error.is_none() {
                let acl_handle = self.central.connection_handle();
                for command in self.cis.start(acl_handle) {
                    let _ = self.channel.send_command(&command[1..]);
                }
                self.cis_requested = true;
            }
        }

        /// Moves queued SDUs onto the stream once it will carry them.
        fn drain_audio(&mut self) {
            if !self.cis.is_streaming() {
                return;
            }
            while let Some(sdu) = self.pending_audio.pop_front() {
                match self.cis.send_sdu(&sdu) {
                    Some(packet) => {
                        let _ = self.channel.inject_host_packet(packet);
                    }
                    None => break,
                }
            }
        }

        /// What the page renders: where the handshake has got to, and why it
        /// stopped if it did.
        pub fn status_json(&self) -> String {
            let stage = if self.error.is_some() {
                "error"
            } else if self.cis.is_streaming() {
                "streaming"
            } else if self.cis_requested {
                "opening the stream"
            } else if self.ase_requested {
                "configuring the endpoint"
            } else if self.central.is_ready() {
                "discovered"
            } else if self.transport.is_open() {
                "connecting"
            } else {
                "offline"
            };
            format!(
                r#"{{"stage":"{}","streaming":{},"cis_handle":{},"queued":{},"dropped":{},"error":{}}}"#,
                stage,
                self.cis.is_streaming(),
                match self.cis.cis_handle() {
                    Some(handle) => handle.to_string(),
                    None => "null".to_string(),
                },
                self.pending_audio.len(),
                self.dropped,
                match &self.error {
                    Some(message) => format!("{message:?}"),
                    None => "null".to_string(),
                }
            )
        }
    }

    // -- Broadcast / Auracast ----------------------------------------------
    //
    // The connectionless media plane, both ends. Unlike every other pair on
    // this site these two devices never meet: there is no ACL, no GATT and no
    // pairing between them, so neither wrapper has a `target` and neither can
    // report anything about the other except what it heard on the air.
    //
    // netsim only. The in-page `Link` does not model periodic advertising or a
    // BIG, so there is nothing here for it to carry.

    /// Hex, space-separated — the form the pages already show wire bytes in.
    fn hex_bytes(bytes: &[u8]) -> String {
        bytes
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The names of the HCI statuses these two devices can actually produce.
    /// A page showing "0x1D" and nothing else makes the reader look it up; the
    /// interesting failures here (a source that is encrypted, a BIG that never
    /// established) deserve to say what they are.
    fn hci_status_name(status: u8) -> &'static str {
        match status {
            0x02 => "Unknown Connection Identifier",
            0x07 => "Memory Capacity Exceeded",
            0x0C => "Command Disallowed",
            0x11 => "Unsupported Feature or Parameter Value",
            0x12 => "Invalid HCI Command Parameters",
            // What a receiver is told when the source tears its BIG down: the
            // BIG Sync Lost event carries the terminating side's reason.
            0x13 => "Remote User Terminated Connection",
            0x14 => "Remote Device Terminated due to Low Resources",
            0x15 => "Remote Device Terminated due to Power Off",
            0x16 => "Connection Terminated by Local Host",
            0x1A => "Unsupported Remote Feature",
            0x1D => "Insufficient Security",
            0x1E => "Parameter Out of Mandatory Range",
            0x22 => "LMP/LL Response Timeout",
            0x3D => "Connection Terminated due to MIC Failure",
            0x3E => "Connection Failed to be Established",
            0x42 => "Unknown Advertising Identifier",
            0x43 => "Limit Reached",
            0x44 => "Operation Cancelled by Host",
            0x45 => "Packet Too Long",
            0xFF => "malformed event",
            _ => "unknown status",
        }
    }

    /// One BASE, rendered the same way whichever end of the broadcast is
    /// holding it. The Broadcast page puts the source's copy beside the
    /// receiver's and compares them field by field, which only means anything
    /// if both were serialized by the same code.
    fn base_json(base: &crate::profiles::bap::BasicAudioAnnouncement) -> serde_json::Value {
        use crate::profiles::bap;
        let subgroups: Vec<serde_json::Value> = base
            .subgroups
            .iter()
            .map(|subgroup| {
                let config = &subgroup.codec_specific_configuration;
                let bis: Vec<serde_json::Value> = subgroup
                    .bis
                    .iter()
                    .map(|bis| {
                        let location = bis.codec_specific_configuration.audio_channel_allocation;
                        serde_json::json!({
                            "index": bis.index,
                            "audio_location": location,
                            "location_name": location.map(bap::audio_location::describe),
                        })
                    })
                    .collect();
                let metadata: Vec<serde_json::Value> = bap::describe_metadata(&subgroup.metadata)
                    .into_iter()
                    .map(|(name, value)| serde_json::json!({ "name": name, "value": value }))
                    .collect();
                serde_json::json!({
                    "codec_id": hex_bytes(&subgroup.codec_id),
                    "codec_name": if subgroup.codec_id == bap::LC3_CODEC_ID {
                        "LC3"
                    } else {
                        "not LC3"
                    },
                    "sampling_frequency_hz": config.sampling_frequency.map(|f| f.hz()),
                    "frame_duration_us": config.frame_duration.map(|d| d.us()),
                    "octets_per_codec_frame": config.octets_per_codec_frame,
                    "codec_frames_per_sdu": config.codec_frames_per_sdu,
                    "metadata_hex": hex_bytes(&subgroup.metadata),
                    "metadata": metadata,
                    "bis": bis,
                })
            })
            .collect();
        serde_json::json!({
            "presentation_delay": base.presentation_delay,
            "subgroups": subgroups,
        })
    }

    /// A Broadcast_Code from what the page's text field holds: 16 octets,
    /// left-justified, zero-padded (BAP 3.7.1). Refused rather than truncated
    /// if it is too long — silently dropping the tail would produce a code
    /// that works nowhere and looks right everywhere.
    fn broadcast_code(code: Option<String>) -> Result<Option<[u8; 16]>, JsValue> {
        let Some(code) = code.filter(|c| !c.is_empty()) else {
            return Ok(None);
        };
        let bytes = code.as_bytes();
        if bytes.len() > 16 {
            return Err(js_error(format!(
                "a Broadcast Code is at most 16 octets; \"{code}\" is {}",
                bytes.len()
            )));
        }
        let mut padded = [0u8; 16];
        padded[..bytes.len()].copy_from_slice(bytes);
        Ok(Some(padded))
    }

    /// An **Auracast broadcast source** on netsim: an extended advertising set
    /// carrying the Broadcast Audio Announcement, a periodic train carrying the
    /// BASE, and a BIG whose BISes this page writes LC3 into.
    ///
    /// Wraps [`BigBroadcaster`](crate::device::BigBroadcaster), which is
    /// transport-free, so this type is only the browser's WebSocket, a running
    /// order, and the status a page renders. The interop scripts in
    /// `tests/interop/` drive the same device against Bumble.
    #[wasm_bindgen]
    pub struct WebBigBroadcaster {
        transport: WasmWsTransport,
        channel: HciChannel,
        broadcaster: crate::device::BigBroadcaster,
        started: bool,
        /// SDUs accepted per BIS — the count the page reports as "sent".
        sent: u64,
        /// SDUs refused because the BIG was not streaming yet.
        refused: u64,
    }

    #[wasm_bindgen]
    impl WebBigBroadcaster {
        /// Creates a source that will publish `broadcast_id` under
        /// `broadcast_name` on `num_bis` streams. A non-empty `code` encrypts
        /// the BISes, which a receiver then needs the same code to join.
        #[wasm_bindgen(constructor)]
        pub fn new(
            url: &str,
            broadcast_id: u32,
            broadcast_name: &str,
            num_bis: u8,
            code: Option<String>,
        ) -> Result<WebBigBroadcaster, JsValue> {
            install_panic_hook();
            if num_bis == 0 || num_bis > 4 {
                return Err(js_error("a BIG here carries between one and four BISes"));
            }
            let config = crate::device::BroadcastConfig {
                broadcast_id: broadcast_id & 0x00FF_FFFF,
                broadcast_name: broadcast_name.to_string(),
                num_bis,
                broadcast_code: broadcast_code(code)?,
                ..Default::default()
            };
            let url = ws_url_with_wire_address(url);
            Ok(Self {
                transport: WasmWsTransport::connect(&url)?,
                channel: HciChannel::new(),
                broadcaster: crate::device::BigBroadcaster::new(config),
                started: false,
                sent: 0,
                refused: 0,
            })
        }

        /// Returns the underlying WebSocket ready state (per the WebSocket API).
        pub fn ready_state(&self) -> u16 {
            self.transport.ready_state()
        }

        /// True once SDUs written here go out over the air.
        pub fn is_streaming(&self) -> bool {
            self.broadcaster.is_streaming()
        }

        /// Writes one SDU to BIS `bis_index` (1-based, as in the BASE).
        /// Returns whether it went out: before the data paths are open the
        /// controller would drop it, so it is refused and counted instead.
        ///
        /// There is deliberately no queue. A broadcaster has no peer to wait
        /// for — the BIG is up or it is not — and audio held back while a
        /// throttled tab catches up would be played late to every receiver at
        /// once.
        pub fn send_sdu(&mut self, bis_index: u8, sdu: Vec<u8>) -> bool {
            match self.broadcaster.send_sdu(bis_index, &sdu) {
                Some(packet) => {
                    let _ = self.channel.inject_host_packet(packet);
                    if bis_index == 1 {
                        self.sent += 1;
                    }
                    true
                }
                None => {
                    if bis_index == 1 {
                        self.refused += 1;
                    }
                    false
                }
            }
        }

        /// Tears the BIG down. The advertising set stays up until this device
        /// is dropped, which is also what stops the periodic train.
        pub fn terminate(&mut self) {
            let _ = self
                .channel
                .inject_host_packet(self.broadcaster.terminate());
            let _ = self.transport.pump(&self.channel);
        }

        /// One pump: bring the controller up, advance the setup sequence, and
        /// return render-ready status JSON.
        pub fn tick(&mut self) -> Result<String, JsValue> {
            self.transport.pump(&self.channel)?;

            if !self.started && self.transport.is_open() {
                // Reset and both event masks. The post-Reset default event mask
                // excludes LE Meta Events, so LE Create BIG Complete — the only
                // announcement of the BIS handles — would never arrive.
                for packet in crate::device::host::init_commands().into_iter().take(3) {
                    self.channel.send_command(&packet[1..]).map_err(js_error)?;
                }
                for packet in self.broadcaster.start() {
                    self.channel.inject_host_packet(packet).map_err(js_error)?;
                }
                self.started = true;
            }

            while let Some(packet) = self.channel.poll_controller_packet() {
                for command in self.broadcaster.on_packet(&packet) {
                    self.channel.inject_host_packet(command).map_err(js_error)?;
                }
            }
            self.transport.pump(&self.channel)?;
            Ok(self.status_json())
        }

        /// Everything the page renders, including the two payloads this source
        /// puts on the air: the advertising data a scanner sees and the BASE
        /// the periodic train carries.
        pub fn status_json(&self) -> String {
            use crate::device::BroadcastState;
            let state = self.broadcaster.state();
            let config = self.broadcaster.config();
            let (stage, failed) = match state {
                BroadcastState::Idle if !self.transport.is_open() => ("offline", None),
                BroadcastState::Idle => ("starting", None),
                BroadcastState::SettingAdvertisingParameters
                | BroadcastState::SettingAdvertisingData => ("advertising set", None),
                BroadcastState::SettingPeriodicParameters | BroadcastState::SettingPeriodicData => {
                    ("periodic train", None)
                }
                BroadcastState::EnablingAdvertising
                | BroadcastState::EnablingPeriodicAdvertising => ("on the air", None),
                BroadcastState::CreatingBig => ("creating the BIG", None),
                BroadcastState::OpeningDataPaths => ("opening data paths", None),
                BroadcastState::Streaming => ("streaming", None),
                BroadcastState::Terminated => ("terminated", None),
                BroadcastState::Failed(status) => ("failed", Some(status)),
            };
            let value = serde_json::json!({
                "stage": stage,
                "state": format!("{state:?}"),
                "streaming": self.broadcaster.is_streaming(),
                "failed": failed,
                "failed_name": failed.map(hci_status_name),
                "bis_handles": self.broadcaster.bis_handles(),
                "sent": self.sent,
                "refused": self.refused,
                "config": {
                    "broadcast_id": config.broadcast_id,
                    "broadcast_name": config.broadcast_name,
                    "advertising_sid": config.advertising_sid,
                    "num_bis": config.num_bis,
                    "max_sdu": config.max_sdu,
                    "sdu_interval_us": config.sdu_interval_us,
                    "rtn": config.rtn,
                    "max_transport_latency_ms": config.max_transport_latency_ms,
                    "phy": config.phy,
                    "encrypted": config.broadcast_code.is_some(),
                    "sampling_frequency_hz": config.sampling_frequency.hz(),
                    "frame_duration_us": config.frame_duration.us(),
                    "octets_per_codec_frame": config.octets_per_codec_frame,
                },
                // The two payloads, exactly as they go out. The BASE's octets
                // are what a receiver reassembles off the periodic train, so
                // the page can compare the two strings directly.
                "advertising_data": hex_bytes(&config.advertising_data()),
                "base_hex": hex_bytes(&config.base().to_bytes()),
                "base": base_json(&config.base()),
            });
            value.to_string()
        }
    }

    /// An **Auracast broadcast sink** on netsim: scans for a Broadcast Audio
    /// Announcement, syncs to the source's periodic train, reads the BASE and
    /// the BIGInfo off it, joins the BIG and collects SDUs per BIS.
    ///
    /// Wraps [`BigReceiver`](crate::device::BigReceiver). Several of these can
    /// exist at once against one source and none of them tells the source
    /// anything — that is what makes it a broadcast.
    #[wasm_bindgen]
    pub struct WebBigReceiver {
        transport: WasmWsTransport,
        channel: HciChannel,
        receiver: crate::device::BigReceiver,
        started: bool,
        /// Whether scanning has been turned off after joining. rootcanal keeps
        /// delivering every advertisement in the simulation otherwise, which on
        /// a page with several receivers is most of the traffic.
        scanning_stopped: bool,
        /// One queue of undelivered SDUs per BIS slot, in BIS index order.
        audio: Vec<VecDeque<Vec<u8>>>,
        /// SDUs received per BIS slot, counted before any bound is applied.
        counts: Vec<u64>,
        /// SDUs dropped because the page did not collect them in time.
        dropped: u64,
    }

    /// How much undelivered audio one BIS may hold — about two seconds at a
    /// 10 ms SDU interval. A hidden tab's timer runs at 1 Hz while the stream
    /// keeps arriving at 100 SDUs a second, so without a bound the queue is
    /// unbounded memory; with one, what it discards is counted rather than
    /// quietly lost.
    const RECEIVER_QUEUE_LIMIT: usize = 200;

    #[wasm_bindgen]
    impl WebBigReceiver {
        /// Creates a receiver. `broadcast_id` filters which source to join —
        /// omit it to take the first Auracast broadcast seen. `code` is the
        /// Broadcast Code for an encrypted source.
        #[wasm_bindgen(constructor)]
        pub fn new(
            url: &str,
            broadcast_id: Option<u32>,
            code: Option<String>,
        ) -> Result<WebBigReceiver, JsValue> {
            install_panic_hook();
            let config = crate::device::ReceiverConfig {
                broadcast_id: broadcast_id.map(|id| id & 0x00FF_FFFF),
                broadcast_code: broadcast_code(code)?,
                ..Default::default()
            };
            let url = ws_url_with_wire_address(url);
            Ok(Self {
                transport: WasmWsTransport::connect(&url)?,
                channel: HciChannel::new(),
                receiver: crate::device::BigReceiver::new(config),
                started: false,
                scanning_stopped: false,
                audio: Vec::new(),
                counts: Vec::new(),
                dropped: 0,
            })
        }

        /// Returns the underlying WebSocket ready state (per the WebSocket API).
        pub fn ready_state(&self) -> u16 {
            self.transport.ready_state()
        }

        /// True once SDUs are arriving.
        pub fn is_receiving(&self) -> bool {
            self.receiver.is_receiving()
        }

        /// Drains the SDUs received on BIS `bis_index` (1-based, as in the
        /// BASE), as an array of byte arrays.
        ///
        /// Per BIS, not merged: LC3 carries decoder state between frames, so
        /// two streams through one decoder corrupt both — and on this page the
        /// two BISes are the left and right of a stereo pair, which is only
        /// audible if they stay apart.
        pub fn take_audio(&mut self, bis_index: u8) -> js_sys::Array {
            let out = js_sys::Array::new();
            if bis_index == 0 {
                return out;
            }
            let Some(queue) = self.audio.get_mut(usize::from(bis_index - 1)) else {
                return out;
            };
            for sdu in queue.drain(..) {
                out.push(&js_sys::Uint8Array::from(&sdu[..]).into());
            }
            out
        }

        /// Leaves the BIG. The device stays on the air and keeps its periodic
        /// sync, so the page can show a receiver that has stopped listening
        /// without pretending it left the room.
        pub fn terminate(&mut self) {
            let _ = self.channel.inject_host_packet(self.receiver.terminate());
            let _ = self.transport.pump(&self.channel);
        }

        /// One pump: advance the synchronization, collect whatever audio
        /// arrived, and return render-ready status JSON.
        pub fn tick(&mut self) -> Result<String, JsValue> {
            self.transport.pump(&self.channel)?;

            if !self.started && self.transport.is_open() {
                for packet in crate::device::host::init_commands().into_iter().take(3) {
                    self.channel.send_command(&packet[1..]).map_err(js_error)?;
                }
                for packet in self.receiver.start() {
                    self.channel.inject_host_packet(packet).map_err(js_error)?;
                }
                self.started = true;
            }

            while let Some(packet) = self.channel.poll_controller_packet() {
                for command in self.receiver.on_packet(&packet) {
                    self.channel.inject_host_packet(command).map_err(js_error)?;
                }
            }

            // Once the BIG is joined there is nothing left to look for.
            if self.receiver.is_receiving() && !self.scanning_stopped {
                self.channel
                    .inject_host_packet(self.receiver.stop_scanning())
                    .map_err(js_error)?;
                self.scanning_stopped = true;
            }

            let slots = self.receiver.bis_handles().len();
            if self.audio.len() != slots {
                self.audio.resize_with(slots, VecDeque::new);
                self.counts.resize(slots, 0);
            }
            while let Some(sdu) = self.receiver.poll_sdu() {
                let Some(slot) = self
                    .receiver
                    .bis_handles()
                    .iter()
                    .position(|&handle| handle == sdu.handle)
                else {
                    continue;
                };
                self.counts[slot] += 1;
                let queue = &mut self.audio[slot];
                queue.push_back(sdu.payload);
                while queue.len() > RECEIVER_QUEUE_LIMIT {
                    queue.pop_front();
                    self.dropped += 1;
                }
            }

            self.transport.pump(&self.channel)?;
            Ok(self.status_json())
        }

        /// Everything the page renders: where synchronization has got to, the
        /// source it found, the BASE it read back, the BIGInfo the controller
        /// reported, and the per-BIS counts.
        pub fn status_json(&self) -> String {
            use crate::device::ReceiverState;
            let state = self.receiver.state();
            let (stage, failed) = match state {
                ReceiverState::Idle if !self.transport.is_open() => ("offline", None),
                ReceiverState::Idle | ReceiverState::SettingScanParameters => ("starting", None),
                ReceiverState::Scanning => ("scanning", None),
                ReceiverState::SyncingToPeriodicAdvertising => ("syncing to the train", None),
                ReceiverState::WaitingForAnnouncement => ("reading the announcement", None),
                ReceiverState::SyncingToBig => ("joining the BIG", None),
                ReceiverState::OpeningDataPaths => ("opening data paths", None),
                ReceiverState::Receiving => ("receiving", None),
                ReceiverState::Terminated => ("left the BIG", None),
                ReceiverState::Lost(reason) => ("lost", Some(reason)),
                ReceiverState::Failed(status) => ("failed", Some(status)),
            };
            let source = self.receiver.found().map(|found| {
                serde_json::json!({
                    "address": Address::new(found.address).to_string(),
                    "address_type": super::address_type_name(found.address_type),
                    "advertising_sid": found.advertising_sid,
                    "broadcast_id": found.broadcast_id,
                })
            });
            let big_info = self.receiver.big_info().map(|info| {
                serde_json::json!({
                    "num_bis": info.num_bis,
                    "nse": info.nse,
                    "iso_interval": info.iso_interval.get(),
                    "bn": info.bn,
                    "pto": info.pto,
                    "irc": info.irc,
                    "max_pdu": info.max_pdu.get(),
                    "sdu_interval_us": info.sdu_interval.get(),
                    "max_sdu": info.max_sdu.get(),
                    "phy": info.phy,
                    "framing": info.framing,
                    "encrypted": info.encryption != 0,
                })
            });
            let handles = self.receiver.bis_handles();
            let streams: Vec<serde_json::Value> = handles
                .iter()
                .enumerate()
                .map(|(slot, &handle)| {
                    serde_json::json!({
                        "index": slot + 1,
                        "handle": handle,
                        "sdus": self.counts.get(slot).copied().unwrap_or(0),
                        "queued": self.audio.get(slot).map(VecDeque::len).unwrap_or(0),
                    })
                })
                .collect();
            let value = serde_json::json!({
                "stage": stage,
                "state": format!("{state:?}"),
                "receiving": self.receiver.is_receiving(),
                "failed": failed,
                "failed_name": failed.map(hci_status_name),
                "source": source,
                "sync_handle": self.receiver.sync_handle(),
                "base": self.receiver.base().map(base_json),
                // The octets as they arrived, for comparison with the source's.
                "base_hex": self.receiver.base_bytes().map(hex_bytes),
                "big_info": big_info,
                "streams": streams,
                "sdu_count": self.receiver.sdu_count(),
                "dropped": self.dropped,
            });
            value.to_string()
        }
    }

    // -- end Broadcast -----------------------------------------------------

    /// A **scripted central on netsim**: the client half of
    /// [`WebPeripheral`], driven by a Rhai script.
    ///
    /// `ScriptedCentral` is transport-free -- H4 packets in, H4 packets out --
    /// so nothing about it assumed the in-page link it was first wired to.
    /// This is the same wrapper `WebPeripheral` is, over the same
    /// `WasmWsTransport` + `HciChannel` pair that `WebSource` already uses to
    /// put a central on netsim. It exists so a scripted client can face a real
    /// controller, and an Android emulator, rather than only its own scene.
    #[wasm_bindgen]
    pub struct WebScriptedCentral {
        transport: WasmWsTransport,
        channel: HciChannel,
        central: crate::scripting::ScriptedCentral,
        started: bool,
    }

    #[wasm_bindgen]
    impl WebScriptedCentral {
        /// Connects a scripted central to netsim at `url` and runs `script`.
        #[wasm_bindgen(constructor)]
        pub fn new(url: &str, script: &str) -> Result<WebScriptedCentral, JsValue> {
            install_panic_hook();
            let central =
                crate::scripting::ScriptedCentral::run_script(script).map_err(js_error)?;
            // netsim reads the URL address LSB-first; connect with the wire
            // form so this device lands on the air where the page says it is.
            let url = ws_url_with_wire_address(url);
            Ok(Self {
                transport: WasmWsTransport::connect(&url)?,
                channel: HciChannel::new(),
                central,
                started: false,
            })
        }

        /// Returns the underlying WebSocket ready state (per the WebSocket API).
        pub fn ready_state(&self) -> u16 {
            self.transport.ready_state()
        }

        /// Aims the script at `address`, overriding whatever it connected to.
        pub fn set_target(&mut self, address: &str) -> Result<(), JsValue> {
            let target: Address = address
                .parse()
                .map_err(|_| JsValue::from_str("target is not a Bluetooth address"))?;
            self.central.set_target(target);
            Ok(())
        }

        /// The first `assert(...)` failure, which is the run's verdict.
        pub fn failure(&self) -> Option<String> {
            self.central.failure().map(str::to_string)
        }

        /// Queues a read of `uuid`, as the in-page `scripted_central_read`
        /// does. A page drives the script rather than replacing it: the
        /// request joins the same outbox the script's own calls use.
        pub fn read(&mut self, uuid: &str) -> Result<(), JsValue> {
            let uuid: crate::types::Uuid =
                uuid.parse().map_err(|_| JsValue::from_str("bad UUID"))?;
            self.central.read(uuid);
            Ok(())
        }

        /// Queues a write of `value` to `uuid`.
        pub fn write(
            &mut self,
            uuid: &str,
            value: Vec<u8>,
            with_response: bool,
        ) -> Result<(), JsValue> {
            let uuid: crate::types::Uuid =
                uuid.parse().map_err(|_| JsValue::from_str("bad UUID"))?;
            self.central.write(uuid, value, with_response);
            Ok(())
        }

        /// Queues enabling or disabling notifications on `uuid`.
        pub fn subscribe(&mut self, uuid: &str, enable: bool) -> Result<(), JsValue> {
            let uuid: crate::types::Uuid =
                uuid.parse().map_err(|_| JsValue::from_str("bad UUID"))?;
            self.central.subscribe(uuid, enable);
            Ok(())
        }

        /// Messages the script emitted since the last call.
        pub fn emitted(&mut self) -> js_sys::Array {
            let out = js_sys::Array::new();
            for message in self.central.take_emitted() {
                out.push(&JsValue::from_str(&message));
            }
            out
        }

        /// One pump: bring the controller up, hand it whatever the script
        /// produced, and return the client's status JSON.
        pub fn tick(&mut self, t_seconds: f64) -> Result<String, JsValue> {
            self.transport.pump(&self.channel)?;

            if !self.started && self.transport.is_open() {
                // Reset and both event masks. The post-Reset default event mask
                // excludes LE Meta Events, so nothing would ever report a
                // connection completing.
                for packet in crate::device::host::init_commands().into_iter().take(3) {
                    self.channel.send_command(&packet[1..]).map_err(js_error)?;
                }
                // The script ran at construction, so its connect is already
                // waiting in the outbox.
                self.drain()?;
                self.started = true;
            }

            while let Some(packet) = self.channel.poll_controller_packet() {
                for out in self.central.on_packet(&packet) {
                    self.channel.inject_host_packet(out).map_err(js_error)?;
                }
            }
            for out in self.central.tick(t_seconds) {
                self.channel.inject_host_packet(out).map_err(js_error)?;
            }
            self.drain()?;

            self.transport.pump(&self.channel)?;
            Ok(self.central.status_json())
        }

        /// Moves anything the script queued outside a packet callback -- a
        /// read or subscribe issued from the page -- onto the wire.
        fn drain(&mut self) -> Result<(), JsValue> {
            for packet in self.central.take_outbox() {
                self.channel.inject_host_packet(packet).map_err(js_error)?;
            }
            Ok(())
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

    // -- Car page: the hands-free car kit ---------------------------------
    //
    // Both endpoints live in one wasm object driven by one timer, because a
    // two-tab design silently stalls: Chrome throttles a hidden tab hard
    // enough that a device in one misses protocol deadlines.

    /// The Car page's engine: a phone and a head unit on one HFP link.
    /// Wraps [`CarKit`](crate::device::car_kit::CarKit), which needs no
    /// transport of its own — there is no WebSocket and no netsim here.
    #[wasm_bindgen]
    pub struct WebCarKit {
        kit: crate::device::car_kit::CarKit,
    }

    #[wasm_bindgen]
    impl WebCarKit {
        /// Creates the pair and starts the head unit reaching for the phone.
        #[wasm_bindgen(constructor)]
        pub fn new() -> WebCarKit {
            install_panic_hook();
            let mut kit = crate::device::car_kit::CarKit::new();
            kit.start();
            Self { kit }
        }

        /// One step of the link. `now_ms` is the page's clock.
        pub fn tick(&mut self, now_ms: f64) {
            self.kit.tick(now_ms.max(0.0) as u64);
        }

        /// Everything the page renders. `since_seq` selects the AT lines the
        /// page has not appended yet.
        pub fn status_json(&self, since_seq: f64) -> String {
            self.kit.status_json(since_seq.max(0.0) as u64)
        }

        /// Routes one UI action. `argument` is the number for `dial`, the
        /// operator name for `operator`, and the level for the gain and
        /// indicator commands; it is ignored otherwise. Returns whether the
        /// action was accepted, so the page can grey out what the link
        /// cannot do yet.
        pub fn command(&mut self, name: &str, argument: &str) -> bool {
            use crate::classic::hfp::AgIndicator;
            let level = || argument.parse::<u8>().unwrap_or(0);
            let value = || argument.parse::<u32>().unwrap_or(0);
            match name {
                "incoming" => self.kit.incoming_call(argument),
                "phone-dial" => self.kit.phone_dial(argument),
                "phone-end" => self.kit.phone_end_call(),
                "answer" => self.kit.answer(),
                "hangup" => self.kit.hang_up(),
                "car-dial" => self.kit.car_dial(argument),
                "speaker" => self.kit.set_speaker_gain(level()),
                "microphone" => self.kit.set_microphone_gain(level()),
                "mute" => self.kit.set_microphone_muted(argument == "1"),
                "voice" => self.kit.set_voice_recognition(argument == "1"),
                "calls" => self.kit.query_calls(),
                "service" => self.kit.set_indicator(AgIndicator::Service, value()),
                "signal" => self.kit.set_indicator(AgIndicator::Signal, value()),
                "battery" => self.kit.set_indicator(AgIndicator::BatteryCharge, value()),
                "roam" => self.kit.set_indicator(AgIndicator::Roam, value()),
                "operator" => {
                    self.kit.set_operator(argument);
                    true
                }
                _ => false,
            }
        }
    }

    impl Default for WebCarKit {
        fn default() -> Self {
            Self::new()
        }
    }

    // -- end Car page ------------------------------------------------------

    // -- USB speaker (Audio page, "USB dongle" controller) -------------------

    /// The other half: a Classic A2DP **source** on a second dongle,
    /// walking the same ladder `examples/a2dp_source.rs` climbs —
    /// [`crate::device::a2dp_source_runner::A2dpSourceRunner`] is that
    /// ladder, extracted — with the page supplying PCM and reading the log.
    /// Point it at a real speaker in pairing mode, or at this page's own
    /// [`WebA2dpSink`] on the other dongle for a full loop over real RF.
    #[wasm_bindgen]
    pub struct WebA2dpSource {
        transport: WasmWsTransport,
        channel: HciChannel,
        runner: crate::device::a2dp_source_runner::A2dpSourceRunner,
        inquiry_length: u8,
        log: Vec<String>,
        failure: Option<String>,
    }

    #[wasm_bindgen]
    impl WebA2dpSource {
        /// Connects to the bridge at `url` (with `?device=` picking the
        /// dongle). `target` is the speaker's address, or empty to inquire
        /// and take the first Audio/Video device that answers.
        #[wasm_bindgen(constructor)]
        pub fn new(url: &str, target: &str) -> Result<WebA2dpSource, JsValue> {
            use crate::device::a2dp_source_runner::A2dpSourceRunner;
            use crate::device::classic_host::{inquiry_mode, io_capability, scan_enable};
            use std::str::FromStr as _;

            install_panic_hook();
            let target = if target.trim().is_empty() {
                None
            } else {
                Some(Address::from_str(target.trim()).map_err(js_error)?)
            };
            let transport = WasmWsTransport::connect(url)?;
            let runner = A2dpSourceRunner::new(target, io_capability::NO_INPUT_NO_OUTPUT, true);
            let channel = HciChannel::new();
            for packet in runner
                .host()
                .start_commands()
                .into_iter()
                // A source is neither discoverable nor connectable: it does
                // the finding.
                .chain(runner.host().set_scan_enable(scan_enable::NONE))
                // Extended inquiry results carry the peer's name in EIR,
                // which is what the page's target picker lists.
                .chain(runner.host().set_inquiry_mode(inquiry_mode::WITH_EXTENDED))
            {
                channel.inject_host_packet(packet).map_err(js_error)?;
            }
            Ok(Self {
                transport,
                channel,
                runner,
                inquiry_length: 8,
                log: vec!["source up, waiting for the bridge socket".to_string()],
                failure: None,
            })
        }

        /// One pump plus one ladder step. `now_ms` is the worker's clock.
        pub fn tick(&mut self, now_ms: f64) {
            if self.failure.is_some() {
                return;
            }
            if let Err(e) = self.transport.pump(&self.channel) {
                self.fail(format!("bridge socket: {e:?}"));
                return;
            }
            while let Some(packet) = self.channel.poll_controller_packet() {
                match self.runner.handle_packet(&packet) {
                    Ok(outgoing) => {
                        for out in outgoing {
                            let _ = self.channel.inject_host_packet(out);
                        }
                    }
                    Err(e) => self.log.push(format!("host: {e}")),
                }
            }
            match self.runner.step(now_ms, self.inquiry_length) {
                Ok(packets) => {
                    for packet in packets {
                        let _ = self.channel.inject_host_packet(packet);
                    }
                }
                Err(e) => {
                    self.fail(e);
                    return;
                }
            }
            self.log.extend(self.runner.take_log());
            self.runner.feed(now_ms);
            for packet in self.runner.poll() {
                let _ = self.channel.inject_host_packet(packet);
            }
        }

        /// Interleaved stereo PCM at 44 100 Hz for the stream; the runner
        /// meters it out at real time.
        pub fn queue_pcm(&mut self, samples: &[i16]) {
            self.runner.queue_pcm(samples);
        }

        /// Samples queued and not yet sent — the page's low-water mark.
        pub fn pending_samples(&self) -> u32 {
            self.runner.pending_samples() as u32
        }

        /// Ends the run.
        pub fn finish(&mut self) {
            self.runner.finish();
        }

        /// Render-ready state; `since` counts log lines already rendered.
        pub fn status_json(&self, since: f64) -> String {
            let since = (since.max(0.0) as usize).min(self.log.len());
            serde_json::json!({
                "stage": self.runner.rung().label(),
                "highest": self.runner.highest().label(),
                "socket_open": self.transport.is_open(),
                "packets_sent": self.runner.packets_sent(),
                "negotiated": self.runner.negotiated(),
                "failure": self.failure,
                "discovered": self.runner.discovered().iter().map(|d| {
                    serde_json::json!({
                        "address": d.address.to_string(),
                        "name": d.name,
                        "class": u32::from_le_bytes([
                            d.class_of_device[0],
                            d.class_of_device[1],
                            d.class_of_device[2],
                            0,
                        ]),
                    })
                }).collect::<Vec<_>>(),
                "log": self.log[since..],
                "log_len": self.log.len(),
            })
            .to_string()
        }

        fn fail(&mut self, reason: String) {
            self.log.push(format!("FAIL: {reason}"));
            self.failure = Some(reason);
        }
    }

    /// A Classic A2DP sink over the `simble --usb` WebSocket bridge: the
    /// browser half of a *real* speaker. The bridge owns a physical dongle
    /// and relays raw HCI both ways, so the phone that pairs with this is a
    /// real phone on real radio — the same ladder `examples/a2dp_sink.rs`
    /// climbs natively, driven from a page that can actually play the PCM.
    ///
    /// The LE devices on the Audio page each own a netsim socket; this owns
    /// the bridge socket, which serves ONE client at a time — the page must
    /// not also point a scanner or a source at it.
    #[wasm_bindgen]
    pub struct WebA2dpSink {
        transport: WasmWsTransport,
        channel: HciChannel,
        host: crate::device::ClassicHost,
        /// Decoded interleaved PCM awaiting `take_pcm()`.
        pcm: Vec<i16>,
        decoded_frames: usize,
        undecodable_bytes: usize,
        /// Milestone lines, appended once each; the page renders `log[since..]`.
        log: Vec<String>,
        avdtp_reported: usize,
        /// Per-layer tallies, so a lossy run names the layer that lost:
        /// H4 packets in from the socket, split by type; media SDUs that
        /// reached the AVDTP handler; host-level parse rejections.
        events_in: usize,
        acl_in: usize,
        media_sdus: usize,
        host_errors: usize,
        /// RTP sequence tracking: packets that never arrived leave no bytes
        /// to count as undecodable — a click in the audio is their only
        /// trace unless the sequence numbers are watched.
        last_rtp_seq: Option<u16>,
        lost_packets: usize,
        said_connected: bool,
        said_paired: bool,
        said_encrypted: bool,
        // The same encryption dance the native example does, for the same
        // reason: a phone will not open an A2DP media channel on an
        // unencrypted link, and after a re-bond it does not always start
        // encryption itself. Authentication first — Set Connection
        // Encryption is only valid on an authenticated link.
        asked_for_authentication: bool,
        asked_for_encryption: bool,
        saw_authentication_complete: bool,
        failure: Option<String>,
    }

    #[wasm_bindgen]
    impl WebA2dpSink {
        /// Connects to the bridge at `url` (e.g. `ws://127.0.0.1:32323/`)
        /// and queues the whole bring-up: reset, event masks, name, Class of
        /// Device `0x240414` (Loudspeaker), SSP, inquiry + page scan, and an
        /// EIR carrying `name` and the Audio Sink service class. The queue
        /// drains once the socket opens.
        /// `keys_json` restores bonds from an earlier life of this sink:
        /// `[{"peer":"AA:BB:..","key":"32 hex chars","key_type":4}, …]`. A
        /// key store that died with the page made every reload a stranger
        /// to the phone that kept its half — an endless pair-again loop.
        #[wasm_bindgen(constructor)]
        pub fn new(url: &str, name: &str, keys_json: &str) -> Result<WebA2dpSink, JsValue> {
            use crate::classic::a2dp::make_audio_sink_service_sdp_records;
            use crate::classic::sdp::SdpServer;
            use crate::device::a2dp::A2dpSink;
            use crate::device::classic_host::{
                LinkKey, authentication_requirements, io_capability, scan_enable,
            };
            use crate::device::{ClassicHost, SdpHandler};
            use std::str::FromStr as _;

            install_panic_hook();
            const AUDIO_SINK_SERVICE_UUID: u16 = 0x110B;
            const SINK_SERVICE_RECORD_HANDLE: u32 = 0x0001_000B;

            let transport = WasmWsTransport::connect(url)?;
            let mut host = ClassicHost::new(name, [0x14, 0x04, 0x24]);
            // A speaker has no display and no keypad; claiming otherwise
            // escalates SSP to Numeric Comparison against a box that cannot
            // show the number.
            host.set_io_capability(
                io_capability::NO_INPUT_NO_OUTPUT,
                authentication_requirements::GENERAL_BONDING,
            );
            let mut sdp = SdpHandler::new(SdpServer::new());
            sdp.server_mut().service_records.insert(
                SINK_SERVICE_RECORD_HANDLE,
                make_audio_sink_service_sdp_records(SINK_SERVICE_RECORD_HANDLE, None),
            );
            host.register_handler(Box::new(sdp)).map_err(js_error)?;
            host.register_handler(Box::new(A2dpSink::new()))
                .map_err(js_error)?;
            if !keys_json.trim().is_empty()
                && let Ok(entries) = serde_json::from_str::<Vec<serde_json::Value>>(keys_json)
            {
                for entry in entries {
                    let (Some(peer), Some(hex), Some(key_type)) = (
                        entry["peer"].as_str(),
                        entry["key"].as_str(),
                        entry["key_type"].as_u64(),
                    ) else {
                        continue;
                    };
                    let Ok(peer) = Address::from_str(peer) else {
                        continue;
                    };
                    let mut value = [0u8; 16];
                    if hex.len() == 32
                        && (0..16).all(|i| {
                            u8::from_str_radix(&hex[2 * i..2 * i + 2], 16)
                                .map(|b| {
                                    value[i] = b;
                                    true
                                })
                                .unwrap_or(false)
                        })
                    {
                        host.insert_link_key(
                            peer,
                            LinkKey {
                                value,
                                key_type: key_type as u8,
                            },
                        );
                    }
                }
            }

            let channel = HciChannel::new();
            for packet in host
                .start_commands()
                .into_iter()
                .chain(host.set_scan_enable(scan_enable::INQUIRY_AND_PAGE))
                .chain(host.set_extended_inquiry_response(name, &[AUDIO_SINK_SERVICE_UUID]))
            {
                channel.inject_host_packet(packet).map_err(js_error)?;
            }
            Ok(Self {
                transport,
                channel,
                host,
                pcm: Vec::new(),
                decoded_frames: 0,
                undecodable_bytes: 0,
                log: vec![format!(
                    "sink up as {name:?}, waiting for the bridge socket"
                )],
                avdtp_reported: 0,
                events_in: 0,
                acl_in: 0,
                media_sdus: 0,
                host_errors: 0,
                last_rtp_seq: None,
                lost_packets: 0,
                said_connected: false,
                said_paired: false,
                said_encrypted: false,
                asked_for_authentication: false,
                asked_for_encryption: false,
                saw_authentication_complete: false,
                failure: None,
            })
        }

        /// One pump of both directions plus the security drive. Call from
        /// the page's timer.
        pub fn tick(&mut self) {
            use crate::classic::avdtp::AvdtpEvent;
            use crate::device::a2dp::A2dpSink;

            if self.failure.is_some() {
                return;
            }
            if let Err(e) = self.transport.pump(&self.channel) {
                self.fail(format!("bridge socket: {e:?}"));
                return;
            }
            while let Some(packet) = self.channel.poll_controller_packet() {
                match packet.first() {
                    Some(&0x04) => self.events_in += 1,
                    Some(&0x02) => self.acl_in += 1,
                    _ => {}
                }
                // Authentication Complete (Vol 4, Part E, 7.7.6): the link
                // key was actually used, as opposed to merely existing.
                if packet.len() > 2 && packet[0] == 0x04 && packet[1] == 0x06 {
                    self.saw_authentication_complete = true;
                }
                match self.host.handle_packet(&packet) {
                    Ok(outgoing) => {
                        for out in outgoing {
                            let _ = self.channel.inject_host_packet(out);
                        }
                    }
                    Err(e) => {
                        self.host_errors += 1;
                        self.log.push(format!("host: {e}"));
                    }
                }
            }
            for packet in self.host.poll() {
                let _ = self.channel.inject_host_packet(packet);
            }

            if !self.said_connected
                && let Some((handle, peer)) = self.host.connection()
            {
                self.log.push(format!(
                    "the phone paged us; ACL up with {peer} on handle {handle:#06x}"
                ));
                self.said_connected = true;
            }
            let security = self.host.security();
            if !self.said_paired {
                if let Some(status) = security.pairing_status.filter(|s| *s != 0x00) {
                    self.fail(format!(
                        "pairing failed: Simple Pairing Complete status {status:#04x}"
                    ));
                    return;
                }
                if security.authenticated {
                    self.log.push("bonded".to_string());
                    self.said_paired = true;
                }
            }
            if self.said_paired && !self.asked_for_authentication {
                self.asked_for_authentication = true;
                for packet in self.host.authenticate() {
                    let _ = self.channel.inject_host_packet(packet);
                }
            }
            if self.saw_authentication_complete && !self.asked_for_encryption {
                self.asked_for_encryption = true;
                for packet in self.host.encrypt(true) {
                    let _ = self.channel.inject_host_packet(packet);
                }
            }
            if !self.said_encrypted && self.host.security().encrypted {
                self.log.push("encrypted".to_string());
                self.said_encrypted = true;
            }

            let Some(sink) = self.host.handler_mut::<A2dpSink>() else {
                self.fail("the sink handler vanished".to_string());
                return;
            };
            while self.avdtp_reported < sink.events().len() {
                let line = match &sink.events()[self.avdtp_reported] {
                    AvdtpEvent::StreamConfigured { seid } => format!("SEID {seid} configured"),
                    AvdtpEvent::StreamOpened { seid } => format!("SEID {seid} open"),
                    AvdtpEvent::StreamStarted { seid } => format!("SEID {seid} streaming"),
                    AvdtpEvent::StreamSuspended { seid } => format!("SEID {seid} suspended"),
                    AvdtpEvent::StreamClosed { seid } => format!("SEID {seid} closed"),
                    other => format!("{other:?}"),
                };
                self.log.push(format!("avdtp: {line}"));
                self.avdtp_reported += 1;
            }
            let frames = sink.take_frames();
            if !frames.is_empty() {
                self.media_sdus += frames.len();
                for frame in &frames {
                    if let Some(last) = self.last_rtp_seq {
                        let gap = frame.sequence_number.wrapping_sub(last).wrapping_sub(1);
                        // A retransmission or wrap glitch looks like a huge
                        // "gap"; count only plausible runs of loss.
                        if gap > 0 && gap < 1000 {
                            self.lost_packets += gap as usize;
                        }
                    }
                    self.last_rtp_seq = Some(frame.sequence_number);
                }
                let audio = A2dpSink::decode(&frames);
                self.decoded_frames += audio.frames;
                self.undecodable_bytes += audio.undecodable_bytes;
                self.pcm.extend_from_slice(&audio.pcm);
            }
        }

        /// The decoded samples that arrived since the last call, interleaved
        /// `i16` at [`Self::sample_rate`] × [`Self::channels`]. The page owns
        /// playback: a wasm module cannot start an `AudioContext`.
        pub fn take_pcm(&mut self) -> Vec<i16> {
            std::mem::take(&mut self.pcm)
        }

        /// The negotiated sampling rate in Hz, or 0 before Set_Configuration.
        pub fn sample_rate(&self) -> u32 {
            use crate::classic::a2dp::sbc::sampling_frequency as sf;
            let Some(configuration) = self.configuration() else {
                return 0;
            };
            match configuration.sampling_frequency {
                x if x == sf::SF_16000 => 16000,
                x if x == sf::SF_32000 => 32000,
                x if x == sf::SF_44100 => 44100,
                x if x == sf::SF_48000 => 48000,
                _ => 0,
            }
        }

        /// Channels in the interleaved PCM: 1 for mono, 2 otherwise, 0
        /// before configuration.
        pub fn channels(&self) -> u32 {
            use crate::classic::a2dp::sbc::channel_mode;
            match self.configuration() {
                None => 0,
                Some(c) if c.channel_mode == channel_mode::MONO => 1,
                Some(_) => 2,
            }
        }

        /// Render-ready state. `since` is how many log lines the page has
        /// already appended; only later ones are included.
        pub fn status_json(&self, since: f64) -> String {
            let since = (since.max(0.0) as usize).min(self.log.len());
            let stage = if self.failure.is_some() {
                "failed"
            } else if self.decoded_frames > 0 {
                "streaming"
            } else if self.said_encrypted {
                "encrypted"
            } else if self.said_paired {
                "paired"
            } else if self.said_connected {
                "connected"
            } else if self.transport.is_open() {
                "waiting"
            } else {
                "connecting"
            };
            serde_json::json!({
                "stage": stage,
                "socket_open": self.transport.is_open(),
                "frames": self.decoded_frames,
                "undecodable_bytes": self.undecodable_bytes,
                "events_in": self.events_in,
                "acl_in": self.acl_in,
                "media_sdus": self.media_sdus,
                "host_errors": self.host_errors,
                "lost_packets": self.lost_packets,
                "sample_rate": self.sample_rate(),
                "channels": self.channels(),
                "failure": self.failure,
                // The silicon's own answer to Read BD_ADDR — the address a
                // phone actually sees, which no page-side constant is.
                "bd_addr": self.host.local_address().map(|a| a.to_string()),
                "link_keys": self.host.all_link_keys().iter().map(|(peer, key)| {
                    serde_json::json!({
                        "peer": peer.to_string(),
                        "key": key.value.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                        "key_type": key.key_type,
                    })
                }).collect::<Vec<_>>(),
                "log": self.log[since..],
                "log_len": self.log.len(),
            })
            .to_string()
        }

        fn configuration(&self) -> Option<crate::classic::a2dp::SbcMediaCodecInformation> {
            self.host
                .handler::<crate::device::a2dp::A2dpSink>()?
                .configuration()
        }

        fn fail(&mut self, reason: String) {
            self.log.push(format!("FAIL: {reason}"));
            self.failure = Some(reason);
        }
    }

    /// The default HRM script, so the page needs no separate fetch.
    ///
    /// Despite the name this builds a *thermometer* — the Playground serves it
    /// as exactly that. Kept for the Playground; a page that wants a specific
    /// device should ask [`catalog_script`] for it by name.
    #[wasm_bindgen]
    pub fn default_heart_rate_script() -> String {
        DEFAULT_HEART_RATE_SCRIPT.to_string()
    }

    /// A device script from the shared catalog, by name.
    ///
    /// The catalog is the one definition of what `"hrm"` or `"thermometer"`
    /// means: MCP's `example` tool, the scene loader and now the demo pages
    /// all read it, so a device cannot mean one thing to an agent and another
    /// in a browser. Returns `undefined` for an unknown name rather than a
    /// placeholder, so a typo fails where it is made.
    #[wasm_bindgen]
    pub fn catalog_script(name: &str) -> Option<String> {
        crate::devices::catalog::script(name).map(str::to_string)
    }

    // -- the bulk-transfer benchmark ---------------------------------------
    //
    // Three wrappers for one measurement, because the two ends may sit on
    // different controllers. In-page both halves share a simulated medium in
    // this object; on netsim each half owns a socket; on the `simble --usb`
    // bridge each half owns a dongle. The Rust in
    // [`crate::device::throughput`] is identical in all three — only where
    // the packets go differs, which is the whole point of being able to
    // compare the numbers.

    /// The clock the benchmark is fed.
    ///
    /// `performance.now()` where there is a window (sub-millisecond,
    /// monotonic, unaffected by the wall clock being set), and `Date.now()`
    /// in a worker where there is not. Never `std::time::Instant`, which
    /// panics on `wasm32-unknown-unknown`.
    fn now_ms() -> f64 {
        web_sys::window()
            .and_then(|window| window.performance())
            .map(|performance| performance.now())
            .unwrap_or_else(js_sys::Date::now)
    }

    /// The bulk-transfer benchmark with both ends in this tab, over the
    /// in-process simulated medium.
    ///
    /// The numbers this produces measure **simble's own stack and a
    /// simulated link** — ATT, L2CAP fragmenting into 27-octet packets, the
    /// in-process controller — and no radio at all. A page showing them
    /// beside a dongle's must say so.
    ///
    /// [`Self::pump`] runs the scene for a slice of wall clock rather than a
    /// fixed number of steps, so a page stays responsive without the
    /// measurement becoming a measurement of the page's frame rate.
    #[wasm_bindgen]
    pub struct WebBulkBench {
        scene: crate::device::throughput::ThroughputScene,
        log: Vec<String>,
    }

    /// The in-page sink's address.
    const BULK_SINK_ADDRESS: Address = Address::new([0x0B, 0x00, 0x00, 0x57, 0x1E, 0xCC]);
    /// The in-page central's address.
    const BULK_CENTRAL_ADDRESS: Address = Address::new([0x0C, 0x00, 0x00, 0x57, 0x1E, 0xCC]);

    #[wasm_bindgen]
    impl WebBulkBench {
        /// One run, configured by the JSON a settings panel produces (see
        /// `BulkOptions`). Unknown keys and malformed JSON fall back to the
        /// defaults rather than refusing to run.
        #[wasm_bindgen(constructor)]
        pub fn new(options_json: &str) -> WebBulkBench {
            install_panic_hook();
            let options = crate::device::throughput::BulkOptions::from_json(options_json);
            Self {
                scene: crate::device::throughput::ThroughputScene::new(
                    BULK_SINK_ADDRESS,
                    BULK_CENTRAL_ADDRESS,
                    options,
                ),
                log: Vec::new(),
            }
        }

        /// Advances the run for up to `budget_ms` of wall clock, then hands
        /// back the report so far. Call it again until
        /// [`Self::is_finished`].
        pub fn pump(&mut self, budget_ms: f64) -> String {
            let deadline = now_ms() + budget_ms.max(1.0);
            while !self.scene.central().is_finished() {
                let now = now_ms();
                if now > deadline {
                    break;
                }
                self.scene.tick(now);
            }
            self.log.extend(self.scene.central_mut().take_log());
            self.scene.report_json()
        }

        /// Whether the run reached its end, successfully or not.
        pub fn is_finished(&self) -> bool {
            self.scene.central().is_finished()
        }

        /// What the run measured.
        pub fn report_json(&self) -> String {
            self.scene.report_json()
        }

        /// The progress lines, oldest first.
        pub fn log(&self) -> js_sys::Array {
            self.log
                .iter()
                .map(|line| JsValue::from_str(line))
                .collect()
        }
    }

    /// The benchmark **peripheral** on a controller of its own: a netsim
    /// socket, or the `simble --usb` bridge holding a dongle.
    ///
    /// It counts the bytes that arrive and stamps when the last one did,
    /// which is the half of the measurement the central cannot make. The
    /// page relays those numbers to [`WebBulkCentral::note_server`] so the
    /// transfer segment ends at arrival rather than at the central's last
    /// queued write.
    #[wasm_bindgen]
    pub struct WebBulkSink {
        transport: WasmWsTransport,
        channel: HciChannel,
        sink: crate::device::throughput::BulkSink,
        started: bool,
    }

    #[wasm_bindgen]
    impl WebBulkSink {
        /// Joins the controller at `url` as a benchmark sink advertising at
        /// `address`. `legacy_masks` narrows the LE event mask to what a
        /// Bluetooth 4.0 dongle accepts — a real part refuses the wider one
        /// outright and then reports no connection at all.
        #[wasm_bindgen(constructor)]
        pub fn new(url: &str, address: &str, legacy_masks: bool) -> Result<WebBulkSink, JsValue> {
            install_panic_hook();
            let address: Address = address
                .parse()
                .map_err(|_| JsValue::from_str("address is not a Bluetooth address"))?;
            let url = ws_url_with_wire_address(url);
            let mut sink = crate::device::throughput::BulkSink::new("simble-bulk-sink", address);
            if legacy_masks {
                sink.set_le_event_mask(crate::device::host::LE_EVENT_MASK_CORE_4_0);
            }
            Ok(Self {
                transport: WasmWsTransport::connect(&url)?,
                channel: HciChannel::new(),
                sink,
                started: false,
            })
        }

        /// The underlying WebSocket ready state (per the WebSocket API).
        pub fn ready_state(&self) -> u16 {
            self.transport.ready_state()
        }

        /// One pump. Returns the counters as JSON — what arrived, and when.
        pub fn tick(&mut self) -> Result<String, JsValue> {
            self.transport.pump(&self.channel)?;
            if !self.started && self.transport.is_open() {
                for packet in self.sink.start_commands() {
                    self.channel.inject_host_packet(packet).map_err(js_error)?;
                }
                self.started = true;
            }
            let now = now_ms();
            for packet in self.sink.poll() {
                let _ = self.channel.inject_host_packet(packet);
            }
            while let Some(packet) = self.channel.poll_controller_packet() {
                for out in self.sink.on_packet(&packet, now) {
                    let _ = self.channel.inject_host_packet(out);
                }
            }
            self.transport.pump(&self.channel)?;
            Ok(self.counters_json())
        }

        /// What the sink has seen, as JSON.
        pub fn counters_json(&self) -> String {
            let counters = self.sink.counters();
            serde_json::to_string(&counters).unwrap_or_else(|_| "{}".to_string())
        }

        /// Bytes received since the last `BEGIN`.
        pub fn bytes(&self) -> f64 {
            self.sink.counters().bytes as f64
        }

        /// Writes those bytes arrived in.
        pub fn chunks(&self) -> u32 {
            self.sink.counters().chunks
        }

        /// When the most recent byte landed, on the page's clock, or `None`
        /// if nothing has.
        pub fn last_byte_ms(&self) -> Option<f64> {
            self.sink.counters().last_byte_ms
        }

        /// Whether a central is connected.
        pub fn is_connected(&self) -> bool {
            self.sink.is_connected()
        }
    }

    /// LE Set Scan Enable — off.
    const SCAN_OFF: [u8; 5] = [0x0C, 0x20, 0x02, 0x00, 0x00];
    /// What the page renders while the scan is still going.
    const DISCOVERING_JSON: &str =
        "{\"phase\":\"discovering\",\"complete\":false,\"failure\":null,\"bytes_sent\":0}";

    /// A discovery that never found anything is still a measurement, and has
    /// to serialise like one so the page's table and CSV keep their shape.
    fn failed_report_json(why: &str) -> String {
        let quoted = serde_json::to_string(why).unwrap_or_else(|_| "\"failed\"".to_string());
        format!(
            "{{\"phase\":\"failed\",\"complete\":false,\"failure\":{quoted},\
             \"bytes_sent\":0,\"confirmation\":\"unconfirmed\"}}"
        )
    }

    /// The benchmark **central** on a controller of its own.
    ///
    /// Point it at a [`WebBulkSink`] on another socket (netsim) or another
    /// dongle (the bridge). Against a peer that is not a benchmark sink the
    /// run still measures discovery, connection and negotiation and then
    /// fails with a reason, which is a data point rather than a hang.
    #[wasm_bindgen]
    pub struct WebBulkCentral {
        transport: WasmWsTransport,
        channel: HciChannel,
        runner: crate::device::throughput::BulkCentral,
        started: bool,
        log: Vec<String>,
        /// Set when the peer has to be found before it can be aimed at.
        discover: Option<Discovery>,
        /// How long the scan took, once it has finished. Kept beside the
        /// report rather than inside it: finding a peer happens *before*
        /// `BulkCentral::start`, so it is not one of the four segments and
        /// must not be folded into `discover_ms`, which measures bring-up and
        /// hearing a peer already known.
        scan_taken_ms: Option<f64>,
        /// Why discovery gave up, if it did.
        failed: Option<String>,
    }

    /// The scan that precedes a run against a peer whose address is not
    /// knowable in advance.
    ///
    /// A phone advertises from a resolvable private address that rotates, and
    /// Android does not tell even its own app what that address currently is.
    /// So there is nothing to write down: the peer is found by the service it
    /// advertises, and the run is aimed at whatever answers.
    struct Discovery {
        options_json: String,
        legacy_masks: bool,
        scanning: bool,
        give_up_at_ms: Option<f64>,
        began_ms: Option<f64>,
        /// Which peer to accept, by advertised name. Empty means the first
        /// one carrying the service.
        ///
        /// With two phones running the sink, the service alone is ambiguous:
        /// the scan would take whichever advertised first while the counters
        /// were fetched from whichever endpoint was configured, and those need
        /// not be the same phone. The name is the only handle available —
        /// Android advertises from a rotating private address it will not
        /// disclose even to its own app.
        name: String,
        /// Addresses heard advertising the service, and the names their scan
        /// responses carried.
        ///
        /// A legacy advertisement is 31 octets and a 128-bit service UUID
        /// takes 16 of them, so the sink puts its name in the *scan response*
        /// — a second report, with no service UUID in it. Neither report
        /// alone identifies a named peer, and waiting for one carrying both
        /// waits forever. They are correlated by address, which is stable for
        /// as long as a scan lasts even when it is a rotating private one.
        heard: Vec<(String, bool, Option<String>)>,
    }

    impl Discovery {
        /// Folds one report in, and says whether that address now satisfies
        /// both halves.
        fn note(&mut self, address: &str, has_service: bool, name: Option<&str>) -> bool {
            let entry = match self.heard.iter_mut().find(|(a, _, _)| a == address) {
                Some(entry) => entry,
                None => {
                    self.heard.push((address.to_string(), false, None));
                    self.heard.last_mut().expect("just pushed")
                }
            };
            entry.1 |= has_service;
            if let Some(name) = name {
                entry.2 = Some(name.to_string());
            }
            let named = self.name.is_empty() || entry.2.as_deref() == Some(self.name.as_str());
            entry.1 && named
        }
    }

    #[wasm_bindgen]
    impl WebBulkCentral {
        /// Joins the controller at `url` and aims at `target`, configured by
        /// the same settings JSON [`WebBulkBench`] takes.
        #[wasm_bindgen(constructor)]
        pub fn new(
            url: &str,
            target: &str,
            options_json: &str,
            legacy_masks: bool,
        ) -> Result<WebBulkCentral, JsValue> {
            install_panic_hook();
            let target: Address = target
                .parse()
                .map_err(|_| JsValue::from_str("target is not a Bluetooth address"))?;
            let url = ws_url_with_wire_address(url);
            let options = crate::device::throughput::BulkOptions::from_json(options_json);
            let mut runner = crate::device::throughput::BulkCentral::new(target, options);
            if legacy_masks {
                runner.set_le_event_mask(crate::device::host::LE_EVENT_MASK_CORE_4_0);
            }
            Ok(Self {
                transport: WasmWsTransport::connect(&url)?,
                channel: HciChannel::new(),
                runner,
                started: false,
                log: Vec::new(),
                discover: None,
                scan_taken_ms: None,
                failed: None,
            })
        }

        /// Joins the controller at `url` and aims at whatever advertises the
        /// bulk service, rather than at an address given in advance.
        ///
        /// This is what a phone needs. See [`Discovery`].
        #[wasm_bindgen(js_name = discovering)]
        pub fn discovering(
            url: &str,
            options_json: &str,
            legacy_masks: bool,
            name: &str,
        ) -> Result<WebBulkCentral, JsValue> {
            install_panic_hook();
            let url = ws_url_with_wire_address(url);
            let options = crate::device::throughput::BulkOptions::from_json(options_json);
            // A placeholder target: the runner is rebuilt, unstarted, the
            // moment the scan produces a real one.
            let mut runner = crate::device::throughput::BulkCentral::new(
                Address::from_be_bytes([0; 6]),
                options,
            );
            if legacy_masks {
                runner.set_le_event_mask(crate::device::host::LE_EVENT_MASK_CORE_4_0);
            }
            Ok(Self {
                transport: WasmWsTransport::connect(&url)?,
                channel: HciChannel::new(),
                runner,
                started: false,
                log: Vec::new(),
                discover: Some(Discovery {
                    options_json: options_json.to_string(),
                    legacy_masks,
                    scanning: false,
                    give_up_at_ms: None,
                    began_ms: None,
                    name: name.to_string(),
                    heard: Vec::new(),
                }),
                scan_taken_ms: None,
                failed: None,
            })
        }

        /// The underlying WebSocket ready state (per the WebSocket API).
        pub fn ready_state(&self) -> u16 {
            self.transport.ready_state()
        }

        /// One pump plus one step. Returns the report as JSON.
        pub fn tick(&mut self) -> Result<String, JsValue> {
            self.transport.pump(&self.channel)?;
            let now = now_ms();
            if let Some(interim) = self.poll_discovery(now)? {
                self.transport.pump(&self.channel)?;
                return Ok(interim);
            }
            if !self.started && self.transport.is_open() {
                for packet in self.runner.start(now) {
                    self.channel.inject_host_packet(packet).map_err(js_error)?;
                }
                self.started = true;
            }
            if self.started {
                while let Some(packet) = self.channel.poll_controller_packet() {
                    for out in self.runner.on_packet(&packet, now) {
                        let _ = self.channel.inject_host_packet(out);
                    }
                }
                for packet in self.runner.step(now) {
                    let _ = self.channel.inject_host_packet(packet);
                }
            }
            self.log.extend(self.runner.take_log());
            self.transport.pump(&self.channel)?;
            Ok(self.runner.report_json())
        }

        /// Tells the run what the peripheral saw. `last_byte_ms` must be on
        /// the same clock this object uses — which it is when both halves
        /// live in one page, and is why netsim runs are `server-stamped`
        /// rather than merely `peer-reported`.
        pub fn note_server(&mut self, bytes: f64, chunks: u32, last_byte_ms: Option<f64>) {
            self.runner
                .note_server(crate::device::throughput::SinkCounters {
                    bytes: bytes.max(0.0) as u64,
                    chunks,
                    expected: 0,
                    first_byte_ms: None,
                    last_byte_ms,
                });
        }

        /// Whether the run reached its end, successfully or not.
        pub fn is_finished(&self) -> bool {
            self.failed.is_some() || self.runner.is_finished()
        }

        /// What the run measured.
        pub fn report_json(&self) -> String {
            match &self.failed {
                Some(why) => failed_report_json(why),
                None => self.runner.report_json(),
            }
        }

        /// How long finding the peer took, in milliseconds, or `-1` if this
        /// run was aimed at an address and never scanned.
        ///
        /// Reported separately because it happens *before* the run starts:
        /// folding it into `discover_ms` would mix finding a stranger with
        /// bring-up against a peer already known.
        pub fn scan_ms(&self) -> f64 {
            self.scan_taken_ms.unwrap_or(-1.0)
        }

        /// Scans for the bulk service, and re-aims the run at what answers.
        ///
        /// Returns `Some(json)` while the scan is still going, which the page
        /// shows as progress; `None` once there is a target (or once there
        /// never will be), so `tick` proceeds to the run itself.
        ///
        /// The discovery state is taken out for the duration rather than
        /// borrowed, so this can use `self.channel` freely.
        fn poll_discovery(&mut self, now: f64) -> Result<Option<String>, JsValue> {
            let Some(mut d) = self.discover.take() else {
                return Ok(None);
            };
            // Nothing to scan with until the controller socket is up.
            if !self.transport.is_open() {
                self.discover = Some(d);
                return Ok(Some(DISCOVERING_JSON.to_string()));
            }
            if !d.scanning {
                queue_scanner_start(&self.channel).map_err(js_error)?;
                d.scanning = true;
                d.began_ms = Some(now);
                // The run's own configured patience, not a second
                // invented number: a caller who widens the timeout
                // because the air is busy means it for the scan too.
                let patience =
                    crate::device::throughput::BulkOptions::from_json(&d.options_json)
                        .timeout_ms;
                d.give_up_at_ms = Some(now + patience);
                self.log.push(if d.name.is_empty() {
                    "scanning for a bulk sink".to_string()
                } else {
                    format!("scanning for {}", d.name)
                });
            }

            let wanted = crate::device::throughput::bulk_uuid::SERVICE.to_string();
            let mut found: Option<String> = None;
            while let Some(packet) = self.channel.poll_controller_packet() {
                for report in parse_scan_reports(&packet) {
                    let has_service = report
                        .service_uuids
                        .iter()
                        .any(|u| u.eq_ignore_ascii_case(&wanted));
                    if d.note(&report.address, has_service, report.name.as_deref()) {
                        found = Some(report.address.clone());
                    }
                }
            }

            if let Some(address) = found {
                let Ok(target) = address.parse::<Address>() else {
                    self.discover = Some(d);
                    return Ok(Some(DISCOVERING_JSON.to_string()));
                };
                // Stop scanning before connecting: a controller still in scan
                // mode has not freed what the connection needs.
                self.channel.send_command(&SCAN_OFF).map_err(js_error)?;
                let options =
                    crate::device::throughput::BulkOptions::from_json(&d.options_json);
                let mut runner = crate::device::throughput::BulkCentral::new(target, options);
                if d.legacy_masks {
                    runner.set_le_event_mask(crate::device::host::LE_EVENT_MASK_CORE_4_0);
                }
                self.runner = runner;
                let took = d.began_ms.map(|began| now - began);
                self.scan_taken_ms = took;
                self.log.push(match took {
                    Some(ms) if !d.name.is_empty() => {
                        format!("found {} at {address} in {ms:.0} ms", d.name)
                    }
                    Some(ms) => format!("found a sink at {address} in {ms:.0} ms"),
                    None => format!("found a sink at {address}"),
                });
                return Ok(None);
            }

            if d.give_up_at_ms.is_some_and(|at| now > at) {
                self.failed = Some(if d.name.is_empty() {
                    "nothing advertising the bulk service — is SimBLE Android running \
                     and in the foreground?"
                        .to_string()
                } else {
                    format!(
                        "no advertisement from {} carrying the bulk service — is SimBLE Android \
                         running and in the foreground on that phone?",
                        d.name
                    )
                });
                // Returning None here would fall through to starting the run,
                // which then aimed at the placeholder address and reported a
                // transfer to 00:00:00:00:00:00.
                return Ok(Some(self.report_json()));
            }

            self.discover = Some(d);
            Ok(Some(DISCOVERING_JSON.to_string()))
        }

        /// The progress lines, oldest first.
        pub fn log(&self) -> js_sys::Array {
            self.log
                .iter()
                .map(|line| JsValue::from_str(line))
                .collect()
        }
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
    WebAdvertiser, WebBulkBench, WebBulkCentral, WebBulkSink, WebCarKit, WebPeripheral, WebScanner,
    WebSession, default_heart_rate_script, run_test,
};

#[cfg(test)]
#[path = "wasm_ws_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "wasm_ws_classic_scene_tests.rs"]
mod classic_scene_tests;
