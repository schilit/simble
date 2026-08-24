// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! GAP (Generic Access Profile) advertising data structures and builders.

use crate::types::SimbleError;
use serde::{Deserialize, Serialize};

/// Standard GAP Advertising Data (AD) Types.
pub mod ad_type {
    /// Flags AD type (0x01).
    pub const FLAGS: u8 = 0x01;
    /// Incomplete list of 16-bit Service UUIDs (0x02).
    pub const INCOMPLETE_16BIT_UUIDS: u8 = 0x02;
    /// Complete list of 16-bit Service UUIDs (0x03).
    pub const COMPLETE_16BIT_UUIDS: u8 = 0x03;
    /// Incomplete list of 128-bit Service UUIDs (0x06).
    pub const INCOMPLETE_128BIT_UUIDS: u8 = 0x06;
    /// Complete list of 128-bit Service UUIDs (0x07).
    pub const COMPLETE_128BIT_UUIDS: u8 = 0x07;
    /// Shortened Local Name (0x08).
    pub const SHORTENED_LOCAL_NAME: u8 = 0x08;
    /// Complete Local Name (0x09).
    pub const COMPLETE_LOCAL_NAME: u8 = 0x09;
    /// Tx Power Level (0x0A).
    pub const TX_POWER_LEVEL: u8 = 0x0A;
    /// Service Data - 16-bit UUID (0x16).
    pub const SERVICE_DATA_16BIT: u8 = 0x16;
    /// Appearance (0x19).
    pub const APPEARANCE: u8 = 0x19;
    /// Resolvable Set Identifier (0x2E) — the six octets (`hash || prand`, each
    /// least significant octet first; CSIS Section 4.9) a CSIP set member
    /// advertises so a coordinator holding the set's SIRK can tell "this is the
    /// other earbud" from "this is some other device". Without it a set member
    /// is discoverable but not identifiable as a member, which is the whole
    /// point of the profile (CSIP Section 5.3).
    pub const RESOLVABLE_SET_IDENTIFIER: u8 = 0x2E;
    /// Broadcast Name (0x30) — the UTF-8 name an Auracast source publishes so
    /// a scanner can list broadcasts before syncing to any of them.
    pub const BROADCAST_NAME: u8 = 0x30;
    /// Manufacturer Specific Data (0xFF).
    pub const MANUFACTURER_SPECIFIC_DATA: u8 = 0xFF;
}

/// The legacy advertising payload limit: 31 octets of AD structures (Core Vol 4,
/// Part E, Section 7.8.7). A payload over this is not truncated by the
/// controller — the command is rejected and the device never transmits at all,
/// which is why every builder here errors instead of returning the oversized
/// bytes.
pub const MAX_ADV_LEN: usize = 31;

/// Common LE Advertising Flags.
pub mod flags {
    /// LE Limited Discoverable Mode.
    pub const LE_LIMITED_DISCOVERABLE: u8 = 0x01;
    /// LE General Discoverable Mode.
    pub const LE_GENERAL_DISCOVERABLE: u8 = 0x02;
    /// BR/EDR Not Supported (LE-only device).
    pub const BR_EDR_NOT_SUPPORTED: u8 = 0x04;
    /// Simultaneous LE and BR/EDR to same device (Controller).
    pub const SIMULTANEOUS_LE_BR_CONTROLLER: u8 = 0x08;
    /// Simultaneous LE and BR/EDR to same device (Host).
    pub const SIMULTANEOUS_LE_BR_HOST: u8 = 0x10;
}

/// Builder for constructing LE Advertising and Scan Response data payloads.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdvertisingData {
    /// Optional LE discovery flags octet.
    pub flags: Option<u8>,
    /// Optional complete local device name.
    pub complete_name: Option<String>,
    /// List of advertised 16-bit Service UUIDs.
    pub service_uuids_16: Vec<u16>,
    /// List of advertised 128-bit Service UUIDs, each in the little-endian byte
    /// order it takes on the air — the reverse of how the UUID is written down.
    /// `#[serde(default)]` so scene JSON written before this field existed still
    /// parses.
    #[serde(default)]
    pub service_uuids_128: Vec<[u8; 16]>,
    /// List of (16-bit UUID, data) Service Data entries.
    pub service_data_16: Vec<(u16, Vec<u8>)>,
    /// Optional manufacturer-specific data (company ID prefixed).
    pub manufacturer_data: Option<Vec<u8>>,
    /// Optional Resolvable Set Identifier: six octets, `hash || prand`, as
    /// produced by [`crate::profiles::csip::rsi`]. `#[serde(default)]` so scene
    /// JSON written before this field existed still parses.
    #[serde(default)]
    pub resolvable_set_identifier: Option<Vec<u8>>,
}

impl AdvertisingData {
    /// Creates an empty advertising-data builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the standard LE discovery flags.
    pub fn with_flags(mut self, flags: u8) -> Self {
        self.flags = Some(flags);
        self
    }

    /// Sets the complete device local name.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.complete_name = Some(name.into());
        self
    }

    /// Adds a 16-bit Service UUID to the advertising payload.
    pub fn with_service_uuid_16(mut self, uuid: u16) -> Self {
        self.service_uuids_16.push(uuid);
        self
    }

    /// Adds a 128-bit Service UUID to the advertising payload.
    ///
    /// `uuid` is the little-endian byte form — the same order
    /// [`crate::types::Uuid::Uuid128`] stores and [`Uuid::to_128_bit_bytes`]
    /// returns — because that is the order it goes on the air. A phone
    /// filtering a scan on a custom service compares against these bytes
    /// reversed, so getting the order wrong makes the device invisible to the
    /// only client that was looking for it.
    ///
    /// [`Uuid::to_128_bit_bytes`]: crate::types::Uuid::to_128_bit_bytes
    pub fn with_service_uuid_128(mut self, uuid: [u8; 16]) -> Self {
        self.service_uuids_128.push(uuid);
        self
    }

    /// Adds 16-bit Service Data (e.g. Eddystone beacon payloads).
    pub fn with_service_data_16(mut self, uuid: u16, data: &[u8]) -> Self {
        self.service_data_16.push((uuid, data.to_vec()));
        self
    }

    /// Sets the Resolvable Set Identifier (CSIS Section 4.9), the six octets
    /// `hash || prand` that [`crate::profiles::csip::rsi`] returns. Hash first
    /// — see that function for why the natural-reading order is the wrong one.
    pub fn with_resolvable_set_identifier(mut self, rsi: &[u8]) -> Self {
        self.resolvable_set_identifier = Some(rsi.to_vec());
        self
    }

    /// Sets manufacturer-specific data.
    pub fn with_manufacturer_data(mut self, company_id: u16, data: &[u8]) -> Self {
        let mut buf = Vec::with_capacity(2 + data.len());
        buf.extend_from_slice(&company_id.to_le_bytes());
        buf.extend_from_slice(data);
        self.manufacturer_data = Some(buf);
        self
    }

    /// Encodes the advertising structure into raw Bluetooth GAP byte format.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        // Flags
        if let Some(f) = self.flags {
            bytes.push(2); // Length: 1 (type) + 1 (data)
            bytes.push(ad_type::FLAGS);
            bytes.push(f);
        }

        // Complete Local Name. An empty name is omitted rather than emitted
        // as a valueless structure: those two bytes buy nothing and come out
        // of a 31-byte budget, which is enough to push a beacon carrying a
        // full-size service-data payload over the limit — at which point the
        // advertisement is rejected and the device silently never transmits.
        if let Some(ref name) = self.complete_name
            && !name.is_empty()
        {
            let name_bytes = name.as_bytes();
            bytes.push((1 + name_bytes.len()) as u8);
            bytes.push(ad_type::COMPLETE_LOCAL_NAME);
            bytes.extend_from_slice(name_bytes);
        }

        // 16-bit Service UUIDs
        if !self.service_uuids_16.is_empty() {
            let len = 1 + (self.service_uuids_16.len() * 2);
            bytes.push(len as u8);
            bytes.push(ad_type::COMPLETE_16BIT_UUIDS);
            for uuid in &self.service_uuids_16 {
                bytes.extend_from_slice(&uuid.to_le_bytes());
            }
        }

        // 128-bit Service UUIDs. Sixteen octets each, least significant first:
        // the service written 0000FE2C-0000-1000-8000-00805F9B34FB goes on the
        // air as FB 34 9B 5F 80 00 00 80 00 10 00 00 2C FE 00 00.
        if !self.service_uuids_128.is_empty() {
            let len = 1 + (self.service_uuids_128.len() * 16);
            bytes.push(len as u8);
            bytes.push(ad_type::COMPLETE_128BIT_UUIDS);
            for uuid in &self.service_uuids_128 {
                bytes.extend_from_slice(uuid);
            }
        }

        // 16-bit Service Data
        for (uuid, data) in &self.service_data_16 {
            let len = 1 + 2 + data.len();
            bytes.push(len as u8);
            bytes.push(ad_type::SERVICE_DATA_16BIT);
            bytes.extend_from_slice(&uuid.to_le_bytes());
            bytes.extend_from_slice(data);
        }

        // Resolvable Set Identifier
        if let Some(ref rsi) = self.resolvable_set_identifier {
            bytes.push((1 + rsi.len()) as u8);
            bytes.push(ad_type::RESOLVABLE_SET_IDENTIFIER);
            bytes.extend_from_slice(rsi);
        }

        // Manufacturer Specific Data
        if let Some(ref mfg) = self.manufacturer_data {
            bytes.push((1 + mfg.len()) as u8);
            bytes.push(ad_type::MANUFACTURER_SPECIFIC_DATA);
            bytes.extend_from_slice(mfg);
        }

        bytes
    }
}

/// Fits an advertising payload into the legacy 31-byte limit by degrading in a
/// fixed order, and is the **only** implementation of that loop: build
/// everything; if it does not fit, drop the optional part; if it still does not
/// fit, trim the name one character at a time down to nothing — the name is a
/// device's identity, so it survives longest.
///
/// `build(name, keep_optional)` re-encodes the payload for each step. What
/// "optional" means is the caller's choice (a caller-supplied UUID list, a
/// beacon's manufacturer data); what is *not* optional is anything a script
/// asked for explicitly, which is why the extras path passes those through
/// `build` unconditionally.
///
/// Returns an error rather than oversized bytes. There were three copies of
/// this loop and only one of them did: the other two handed back a payload
/// longer than 31 bytes, which [`crate::device::host::adv_data_param`] then
/// wrote into a fixed 32-byte HCI parameter block. The controller rejects that
/// command, so the device advertises nothing at all — a silent disappearance
/// that looks nothing like the overflow that caused it.
pub(crate) fn fit_within_legacy_limit(
    name: &str,
    build: impl Fn(&str, bool) -> Vec<u8>,
) -> Result<Vec<u8>, SimbleError> {
    let mut bytes = build(name, true);
    if bytes.len() > MAX_ADV_LEN {
        bytes = build(name, false);
    }
    // `String::pop` removes a whole `char`, so a multi-byte name is never cut
    // mid-codepoint into an AD structure no scanner can decode.
    let mut trimmed = name.to_string();
    while bytes.len() > MAX_ADV_LEN && trimmed.pop().is_some() {
        bytes = build(&trimmed, false);
    }
    if bytes.len() > MAX_ADV_LEN {
        return Err(SimbleError::InvalidParameter(format!(
            "advertising data exceeds the {MAX_ADV_LEN}-byte legacy limit ({} bytes) with nothing \
             left to trim",
            bytes.len()
        )));
    }
    Ok(bytes)
}

/// Builds an advertising payload (flags + 16-bit service UUIDs + complete
/// local name) that fits the legacy 31-byte limit, dropping the UUID list
/// and then trimming the name if needed.
pub fn build_adv_payload(name: &str, service_uuids: &[u16]) -> Result<Vec<u8>, SimbleError> {
    fit_within_legacy_limit(name, |name, keep_uuids| {
        let mut ad = AdvertisingData::new()
            .with_flags(flags::LE_GENERAL_DISCOVERABLE | flags::BR_EDR_NOT_SUPPORTED)
            .with_name(name);
        if keep_uuids {
            for &uuid in service_uuids {
                ad = ad.with_service_uuid_16(uuid);
            }
        }
        ad.to_bytes()
    })
}

/// Like [`build_adv_payload`], but folds in script-staged extras (service
/// UUIDs of both widths, service data, manufacturer data, a CSIP Resolvable
/// Set Identifier — the beacon and set-member idioms). The extras were asked
/// for explicitly, so they survive trimming: the caller's UUID list is dropped
/// first, then the name; if the extras alone still exceed the legacy 31-byte
/// limit, that is a script error, not something to truncate silently.
pub fn build_adv_payload_with_extras(
    name: &str,
    service_uuids: &[u16],
    extras: Option<&AdvertisingData>,
) -> Result<Vec<u8>, SimbleError> {
    let Some(extras) = extras else {
        return build_adv_payload(name, service_uuids);
    };
    // An empty name is omitted entirely (no zero-length AD structure), so a
    // large service-data payload can occupy the whole packet — real beacons
    // (Quick Share, Eddystone) carry no name at all.
    fit_within_legacy_limit(name, |name, keep_uuids| {
        let mut ad = AdvertisingData::new()
            .with_flags(flags::LE_GENERAL_DISCOVERABLE | flags::BR_EDR_NOT_SUPPORTED);
        // `with_name` already drops an empty name, so no special case here.
        ad = ad.with_name(name);
        if keep_uuids {
            for &uuid in service_uuids {
                ad = ad.with_service_uuid_16(uuid);
            }
        }
        // Services registered by a Rust profile registrar live only in the
        // GATT database, so `advertise_service_uuid` stages them here —
        // without this they were silently dropped and the device advertised
        // no services at all.
        for &uuid in &extras.service_uuids_16 {
            if !ad.service_uuids_16.contains(&uuid) {
                ad = ad.with_service_uuid_16(uuid);
            }
        }
        ad.service_uuids_128 = extras.service_uuids_128.clone();
        ad.service_data_16 = extras.service_data_16.clone();
        ad.manufacturer_data = extras.manufacturer_data.clone();
        ad.resolvable_set_identifier = extras.resolvable_set_identifier.clone();
        ad.to_bytes()
    })
}

/// Walks an advertising payload's AD structures, yielding `(type, value)` for
/// each. Stops at the first malformed or zero-length structure rather than
/// guessing — a truncated tail is exactly what a controller's "data status:
/// incomplete" report leaves behind, and reading past it invents fields.
pub fn ad_structures(data: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    let mut offset = 0usize;
    std::iter::from_fn(move || {
        let &length = data.get(offset)?;
        if length == 0 {
            return None;
        }
        let ad_type = *data.get(offset + 1)?;
        let value = data.get(offset + 2..offset + 1 + usize::from(length))?;
        offset += 1 + usize::from(length);
        Some((ad_type, value))
    })
}

/// The payload of the first 16-bit Service Data structure for `uuid`, if the
/// advertising data carries one.
pub fn service_data_16(data: &[u8], uuid: u16) -> Option<&[u8]> {
    ad_structures(data).find_map(|(ad_type, value)| {
        if ad_type != self::ad_type::SERVICE_DATA_16BIT || value.len() < 2 {
            return None;
        }
        (u16::from_le_bytes([value[0], value[1]]) == uuid).then_some(&value[2..])
    })
}

/// The six-byte Resolvable Set Identifier (AD type 0x2E) an advertisement
/// carries, if any — the scanner-side counterpart of
/// [`AdvertisingData::with_resolvable_set_identifier`].
///
/// Feed the result to [`crate::profiles::csip::rsi_matches`] with each SIRK the
/// coordinator holds; a match means "this is a member of that set". A
/// structure of any other length is not an RSI and is ignored rather than
/// resolved against, because `sih` over the wrong number of octets would
/// silently produce a confident answer.
pub fn resolvable_set_identifier(data: &[u8]) -> Option<&[u8]> {
    ad_structures(data).find_map(|(ad_type, value)| {
        (ad_type == self::ad_type::RESOLVABLE_SET_IDENTIFIER && value.len() == 6).then_some(value)
    })
}

#[cfg(test)]
#[path = "advertising_tests.rs"]
mod tests;
