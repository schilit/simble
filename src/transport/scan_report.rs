// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Transport-neutral advertising-scan reporting and demo HCI bring-up.
//!
//! [`parse_scan_reports`] turns an LE Advertising Report event into
//! render-ready [`ScanReport`]s — every AD-structure decoded here in Rust — and
//! the `queue_*` helpers stage the HCI commands a demo scanner or advertiser
//! needs onto an [`HciChannel`]. None of it is browser-specific: the wasm
//! bindings in [`super::wasm_ws`], the scene engines, and the CLI all drive it
//! the same way, so it lives here rather than in the wasm module.

use crate::gap::advertising::fit_within_legacy_limit;
use crate::gap::{AdvertisingData, ad_type, flags};
use crate::packets::hci_events::{
    HciEvent, advertising_reports, event_code as hci_event_code, le_subevent,
};
use crate::transport::hci_adapter::HciChannel;
use crate::types::{Address, SimbleError, Uuid};
use serde::Serialize;
use zerocopy::{FromBytes, byteorder::little_endian::U16};

/// Sends one HCI command on `channel`. The packet itself is built by the
/// host layer (`device::host::command`) so there is a single definition of
/// what an HCI command looks like.
pub(crate) fn queue_command(
    channel: &HciChannel,
    opcode: [u8; 2],
    params: &[u8],
) -> Result<(), SimbleError> {
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

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

pub(crate) fn address_type_name(raw: u8) -> &'static str {
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
                // A packed list of little-endian 16-bit UUIDs. Reinterpret the
                // even-length prefix as `[U16]` and read each with `.get()`;
                // a trailing odd byte (never valid here) is dropped, as the
                // old `as_chunks::<2>()` did.
                let even = payload.len() - payload.len() % 2;
                if let Ok(uuids) = <[U16]>::ref_from_bytes(&payload[..even]) {
                    for uuid in uuids {
                        report
                            .service_uuids
                            .push(Uuid::from_u16(uuid.get()).to_string());
                    }
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
                // A little-endian service UUID, then the service's own data.
                let (uuid, data) = U16::ref_from_prefix(payload).expect("length checked above");
                report.service_data.push(TaggedBytes {
                    tag: Uuid::from_u16(uuid.get()).to_string(),
                    data: hex(data),
                });
            }
            // Exactly six octets or it is not an RSI. A shorter or longer
            // structure is dropped rather than resolved against, because `sih`
            // over the wrong octets still returns a confident three bytes.
            ad_type::RESOLVABLE_SET_IDENTIFIER if payload.len() == 6 => {
                report.resolvable_set_identifier = Some(hex(payload));
            }
            ad_type::MANUFACTURER_SPECIFIC_DATA if payload.len() >= 2 => {
                // A little-endian company identifier, then the vendor payload.
                let (company, data) = U16::ref_from_prefix(payload).expect("length checked above");
                report.manufacturer_data = Some(TaggedBytes {
                    tag: format!("{:04X}", company.get()),
                    data: hex(data),
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
pub(crate) fn send_acl(channel: &HciChannel, handle: u16, l2cap: &[u8]) -> Result<(), SimbleError> {
    for packet in crate::device::host::acl_packets(handle, l2cap) {
        channel.send_acl_data(&packet[1..])?;
    }
    Ok(())
}

#[cfg_attr(not(any(target_arch = "wasm32", test)), allow(dead_code))]
/// Extracts the `address=` query parameter from a netsim WebSocket URL.
/// Pages write it in display order (`CC:1E:57:00:00:06`), which is what the
/// device's identity must be — SMP computes with it.
pub(crate) fn address_from_ws_url(url: &str) -> Option<Address> {
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
pub(crate) fn ws_url_with_wire_address(url: &str) -> String {
    let Some(address) = address_from_ws_url(url) else {
        return url.to_string();
    };
    let wire = address.to_netsim_wire_string();
    let display = address.to_string();
    url.replace(&format!("address={display}"), &format!("address={wire}"))
}
