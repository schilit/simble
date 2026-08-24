// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Ranging Service (RAS, UUID 0x185B) per the Bluetooth SIG Channel Sounding
//! Profile.
//!
//! # What this service is for
//!
//! A Channel Sounding procedure produces measurements at **both** ends, and
//! neither end's measurements mean anything alone: each radio adds its own
//! local oscillator's phase, redrawn on every frequency hop, so one side's
//! tones are noise until they are summed with the other side's. See
//! [`crate::cs::ranging`] for the arithmetic.
//!
//! The reflector's controller therefore has data the initiator's host needs,
//! and there is no path for it below the host — so the Channel Sounding
//! Profile defines a GATT service to carry it. That is all RAS is: a pipe for
//! one device's raw CS step data to reach the other device's application.
//! **It does not carry a distance.** The receiving side computes that.
//!
//! # What is implemented here
//!
//! [`RangingData`] encodes and decodes the profile's Ranging Data body —
//! ranging header, subevent header, and mode-2 steps — and
//! [`segment`](crate::profiles::ras::segment)/[`Reassembler`](crate::profiles::ras::Reassembler)
//! wrap it in the segmentation header that lets a body larger than the ATT MTU
//! cross the link.
//!
//! (Those two are spelled out in full because this module carries an outer
//! `///` doc on its `pub mod ras;` declaration as well as this `//!` header.
//! Mixing the two makes rustdoc resolve every link here in the *parent*
//! module's scope, where only the re-exported names are visible.)
//!
//! Not implemented: On-Demand Ranging Data with its Get/Ack/Retrieve control
//! point flow (only the real-time path exists), Ranging Data Ready and
//! Ranging Data Overwritten notifications, mode-0 and mode-1 step data, and
//! multiple antenna paths.

use crate::cs::tones::{SubeventResult, Tone, decode_pct};
use crate::gatt::{AttributePermissions, CharacteristicProperties, GattDatabase};

/// Bluetooth SIG Ranging Service UUIDs.
///
/// Values are from the SIG's own registry —
/// `assigned_numbers/uuids/characteristic_uuids.yaml` in
/// <https://bitbucket.org/bluetooth-SIG/public>, which is the authoritative
/// list and is served without authentication.
///
/// These were previously 0x2B6E/0x2B70/0x2B71/0x2B72, which are **not assigned
/// to anything** in that registry — they were invented, and a real client
/// looking for RAS would never have found this service. Nothing in-tree could
/// have caught it: both ends of every test here are this codebase, and neither
/// Bumble nor Zephyr mainline implements RAS to disagree with us. Only the
/// registry itself could, which is the argument for checking against it.
pub mod ras_uuid {
    use crate::types::Uuid;

    /// Ranging Service UUID.
    pub const RANGING_SERVICE: Uuid = Uuid::Uuid16(0x185B);
    /// RAS Features characteristic UUID.
    pub const RANGING_FEATURES: Uuid = Uuid::Uuid16(0x2C14);
    /// Real-time Ranging Data characteristic UUID.
    pub const RANGING_REALTIME_DATA: Uuid = Uuid::Uuid16(0x2C15);
    /// On-demand Ranging Data characteristic UUID.
    pub const RANGING_ON_DEMAND_DATA: Uuid = Uuid::Uuid16(0x2C16);
    /// RAS Control Point characteristic UUID.
    pub const RANGING_CONTROL_POINT: Uuid = Uuid::Uuid16(0x2C17);
    /// Ranging Data Ready characteristic UUID. Not served yet; defined so the
    /// value is not invented a second time when it is.
    pub const RANGING_DATA_READY: Uuid = Uuid::Uuid16(0x2C18);
    /// Ranging Data Overwritten characteristic UUID. Not served yet, as above.
    pub const RANGING_DATA_OVERWRITTEN: Uuid = Uuid::Uuid16(0x2C19);
}

/// Ranging Service container holding GATT attribute handles.
#[derive(Debug, Clone)]
pub struct RangingService {
    /// Attribute handle of the service declaration.
    pub service_handle: u16,
    /// Attribute handle of the Features.
    pub features_handle: u16,
    /// Value attribute handle of the Features characteristic.
    pub features_value_handle: u16,
    /// Attribute handle of the Realtime Data.
    pub realtime_data_handle: u16,
    /// Value attribute handle of the Realtime Data characteristic.
    pub realtime_data_value_handle: u16,
    /// Attribute handle of the Control Point.
    pub control_point_handle: u16,
    /// Value attribute handle of the Control Point characteristic.
    pub control_point_value_handle: u16,
}

impl RangingService {
    /// Registers the Ranging Service (0x185B) into a GATT database.
    pub fn register(db: &mut GattDatabase) -> Self {
        let service_handle = db.add_service(ras_uuid::RANGING_SERVICE, true);

        // 1. RAS Features (0x2C14) - Read only
        // Feature bits: bit 0: Real-Time Ranging Data, bit 1: On-Demand Ranging Data
        let (features_handle, features_value_handle) = db.add_characteristic(
            ras_uuid::RANGING_FEATURES,
            CharacteristicProperties(CharacteristicProperties::READ),
            vec![0x03, 0x00, 0x00, 0x00], // Features: Real-Time + On-Demand supported
            AttributePermissions::default(),
        );

        // 2. Real-time Ranging Data (0x2C15) - Notify
        let (realtime_data_handle, realtime_data_value_handle) = db.add_characteristic_with_cccd(
            ras_uuid::RANGING_REALTIME_DATA,
            CharacteristicProperties(CharacteristicProperties::NOTIFY),
            vec![0x00; 8],
            AttributePermissions::default(),
        );

        // 3. RAS Control Point (0x2C17) - Write | Indicate
        let (control_point_handle, control_point_value_handle) = db.add_characteristic_with_cccd(
            ras_uuid::RANGING_CONTROL_POINT,
            CharacteristicProperties(
                CharacteristicProperties::WRITE | CharacteristicProperties::INDICATE,
            ),
            vec![],
            AttributePermissions::write_only(),
        );

        Self {
            service_handle,
            features_handle,
            features_value_handle,
            realtime_data_handle,
            realtime_data_value_handle,
            control_point_handle,
            control_point_value_handle,
        }
    }

    /// Encodes a distance and a confidence as eight little-endian bytes.
    ///
    /// **This is not the profile's Ranging Data format.** Real RAS carries a
    /// peer's raw Channel Sounding steps so the *receiver* can compute a
    /// distance; it never carries a distance itself, and a real Ranging
    /// Service client would not understand these bytes. It is kept because
    /// the `ranging` and `ranging_tag` example scripts publish it as a
    /// stand-in for a device with no CS radio behind it. For the real thing,
    /// see [`RangingData`].
    pub fn encode_ranging_data(distance_meters: f32, confidence: f32) -> Vec<u8> {
        let mut buf = Vec::with_capacity(8);
        buf.extend_from_slice(&distance_meters.to_le_bytes());
        buf.extend_from_slice(&confidence.to_le_bytes());
        buf
    }

    /// Updates the real-time ranging data in the GATT database and creates a notification packet.
    pub fn update_ranging_data(
        &self,
        db: &mut GattDatabase,
        distance_meters: f32,
        confidence: f32,
    ) -> Result<Vec<u8>, u8> {
        let payload = Self::encode_ranging_data(distance_meters, confidence);
        db.set_value(self.realtime_data_value_handle, &payload)?;
        Ok(payload)
    }
}

/// One procedure's Channel Sounding results, in the Ranging Service's
/// Ranging Data format.
///
/// The layout mirrors the profile's: a ranging header naming the procedure,
/// then a subevent header, then one record per step. Only mode-2 (Phase-Based
/// Ranging) steps are produced and understood — enough to carry the tones a
/// distance is computed from, which is the whole job.
#[derive(Debug, Clone, PartialEq)]
pub struct RangingData {
    /// Identifies the procedure. The receiving end must only combine data
    /// whose counter matches its own results: tones from different
    /// procedures were measured with different oscillator phases, and
    /// mixing them destroys the cancellation the sum depends on.
    pub ranging_counter: u16,
    /// Which CS configuration produced this.
    pub config_id: u8,
    /// The transmit power the sender used, in dBm.
    pub selected_tx_power: i8,
    /// Bitmask of antenna paths present in each step.
    pub antenna_paths_mask: u8,
    /// The peer's reference power level for the subevent, in dBm.
    pub reference_power_level: i8,
    /// One tone per mode-2 step.
    pub tones: Vec<Tone>,
}

/// Field widths and positions in the Ranging Data headers.
mod layout {
    /// Ranging header: counter + config id (2), selected TX power (1),
    /// antenna paths mask (1).
    pub const RANGING_HEADER_LEN: usize = 4;
    /// Subevent header: start ACL conn event (2), frequency compensation (2),
    /// done statuses (1), abort reasons (1), reference power level (1),
    /// number of steps (1).
    pub const SUBEVENT_HEADER_LEN: usize = 8;
    /// The ranging counter occupies the low 12 bits of the first two octets.
    pub const RANGING_COUNTER_MASK: u16 = 0x0FFF;
    /// The configuration identifier sits in bits 12–15 — a **4-bit** field,
    /// per RAS v1.0 §3.2.1.2 Table 3.7 (Ranging Counter 12 bits, then
    /// Configuration ID 4 bits, filling the two octets exactly). The spec
    /// restricts the *value* to 0–3 today, but the field is four bits wide and
    /// is decoded as such: masking narrower would silently rewrite a peer's
    /// header rather than report what it sent.
    pub const CONFIG_ID_SHIFT: u32 = 12;
    /// Mask for the configuration identifier once shifted down.
    pub const CONFIG_ID_MASK: u8 = 0x0F;
    /// Step mode lives in the low two bits of the step mode octet; bit 7 is
    /// the "step aborted" flag.
    pub const STEP_MODE_MASK: u8 = 0x03;
    /// Antenna paths this encoder reports. One path is the 1:1 antenna
    /// configuration; `antenna_paths_mask` is hardcoded to match.
    pub const NUM_ANTENNA_PATHS: usize = 1;
    /// Mode-2 step data: an antenna permutation index, then a 3-byte PCT and
    /// a 1-byte Tone Quality Indicator for each of `N_AP + 1` tone entries —
    /// one per antenna path **plus the tone extension slot** (Core 6.0,
    /// Vol 4, Part E, Section 7.7.65.44, which RAS v1.0 §3.2.1.4 carries
    /// through unchanged).
    ///
    /// The `+ 1` is not optional and does not depend on tone extension being
    /// in use: the slot is always present, and a receiver that sizes the step
    /// from `N_AP` alone lands four bytes short. This encoder omitted it and
    /// wrote `1 + 4 = 5`, which no real controller produces — see
    /// `tests/cs_real_capture_test.rs`, where all 9 072 mode-2 steps captured
    /// off an nRF54L15 declare a length of **9**. `controller::sim::pbr_step`
    /// had it right; only this encoder did not.
    pub const MODE_2_STEP_DATA_LEN: usize = 1 + 4 * (NUM_ANTENNA_PATHS + 1);
    /// Offset of the first antenna path's tone entry within mode-2 step data,
    /// past the antenna permutation index.
    pub const MODE_2_FIRST_TONE: usize = 1;
    /// Step mode 2 — Phase-Based Ranging.
    pub const STEP_MODE_PBR: u8 = 2;
}

impl RangingData {
    /// Builds Ranging Data from what a device's own controller reported.
    pub fn from_subevent(result: &SubeventResult, selected_tx_power: i8) -> Self {
        Self {
            ranging_counter: result.procedure_counter & layout::RANGING_COUNTER_MASK,
            config_id: result.config_id,
            selected_tx_power,
            antenna_paths_mask: 0x01,
            reference_power_level: result.reference_power_level,
            tones: result.tones.clone(),
        }
    }

    /// Serializes to the Ranging Data body — the bytes that go inside the
    /// segmentation header.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(
            layout::RANGING_HEADER_LEN
                + layout::SUBEVENT_HEADER_LEN
                + self.tones.len() * (3 + layout::MODE_2_STEP_DATA_LEN),
        );
        let header = (self.ranging_counter & layout::RANGING_COUNTER_MASK)
            | (u16::from(self.config_id & layout::CONFIG_ID_MASK) << layout::CONFIG_ID_SHIFT);
        out.extend_from_slice(&header.to_le_bytes());
        out.push(self.selected_tx_power as u8);
        out.push(self.antenna_paths_mask);

        out.extend_from_slice(&self.ranging_counter.to_le_bytes()); // start ACL conn event
        out.extend_from_slice(&[0xFF, 0xFF]); // frequency compensation: unavailable
        out.push(0x00); // ranging done / subevent done: complete
        out.push(0x00); // ranging abort / subevent abort: none
        out.push(self.reference_power_level as u8);
        out.push(self.tones.len() as u8);

        for tone in &self.tones {
            out.push(layout::STEP_MODE_PBR);
            out.push(tone.channel);
            out.push(layout::MODE_2_STEP_DATA_LEN as u8);
            out.push(0x00); // antenna permutation index
            let packed =
                (u32::from(tone.i as u16) & 0x0FFF) | ((u32::from(tone.q as u16) & 0x0FFF) << 12);
            // One entry per antenna path, then the tone extension slot. Only
            // one path is modelled, so the slot repeats the path's sample —
            // which is what real silicon does too when tone extension is not
            // in use: the captured slots differ from their path by a count or
            // two of the 12-bit quantization.
            for _ in 0..=layout::NUM_ANTENNA_PATHS {
                out.extend_from_slice(&[packed as u8, (packed >> 8) as u8, (packed >> 16) as u8]);
                out.push(tone.quality);
            }
        }
        out
    }

    /// Parses a Ranging Data body, or `None` if it is truncated or carries a
    /// step whose declared length runs past the end.
    pub fn parse(body: &[u8]) -> Option<Self> {
        if body.len() < layout::RANGING_HEADER_LEN + layout::SUBEVENT_HEADER_LEN {
            return None;
        }
        let header = u16::from_le_bytes([body[0], body[1]]);
        let subevent = &body[layout::RANGING_HEADER_LEN..];
        let step_count = subevent[7];
        let mut data = Self {
            ranging_counter: header & layout::RANGING_COUNTER_MASK,
            config_id: ((header >> layout::CONFIG_ID_SHIFT) as u8) & layout::CONFIG_ID_MASK,
            selected_tx_power: body[2] as i8,
            antenna_paths_mask: body[3],
            reference_power_level: subevent[6] as i8,
            tones: Vec::with_capacity(usize::from(step_count)),
        };

        let mut cursor = layout::RANGING_HEADER_LEN + layout::SUBEVENT_HEADER_LEN;
        for _ in 0..step_count {
            let [mode, channel, len] = *body.get(cursor..cursor + 3)? else {
                return None;
            };
            let step = body.get(cursor + 3..cursor + 3 + usize::from(len))?;
            cursor += 3 + usize::from(len);
            if mode & layout::STEP_MODE_MASK != layout::STEP_MODE_PBR {
                continue;
            }
            // The first antenna path's tone entry. Later paths and the tone
            // extension slot follow and are not read — one path is what the
            // 1:1 antenna configuration provides, and what this encoder
            // writes. Indexing past the permutation index rather than off the
            // end of the step means a real 9-byte step and this encoder's own
            // output are read identically.
            let pct = step.get(layout::MODE_2_FIRST_TONE..layout::MODE_2_FIRST_TONE + 4)?;
            let (i, q) = decode_pct([pct[0], pct[1], pct[2]]);
            data.tones.push(Tone {
                channel,
                i,
                q,
                quality: pct[3],
            });
        }
        Some(data)
    }
}

/// Bit positions in the Ranging Data segmentation header.
mod segmentation {
    /// Set on the first segment of a Ranging Data body.
    pub const FIRST: u8 = 0x01;
    /// Set on the last segment.
    pub const LAST: u8 = 0x02;
    /// The rolling counter occupies the remaining six bits.
    pub const COUNTER_SHIFT: u32 = 2;
}

/// Splits a Ranging Data body into notification-sized segments, each prefixed
/// with the profile's one-byte segmentation header.
///
/// A procedure's worth of tones is a few hundred bytes and an ATT
/// notification carries `MTU − 3`, so RAS always has to segment. The header's
/// low two bits mark the first and last segment and the rest is a rolling
/// counter, which is what lets a receiver notice a dropped segment instead of
/// reassembling a body with a hole in it.
///
/// `max_payload` is the notification's value capacity; one byte of it goes to
/// the header. Returns an empty vector if there is no room for any data.
pub fn segment(body: &[u8], max_payload: usize) -> Vec<Vec<u8>> {
    if max_payload < 2 {
        return Vec::new();
    }
    let chunk_size = max_payload - 1;
    let chunks: Vec<&[u8]> = body.chunks(chunk_size).collect();
    chunks
        .iter()
        .enumerate()
        .map(|(index, chunk)| {
            let mut flags = (index as u8 & 0x3F) << segmentation::COUNTER_SHIFT;
            if index == 0 {
                flags |= segmentation::FIRST;
            }
            if index + 1 == chunks.len() {
                flags |= segmentation::LAST;
            }
            let mut segment = Vec::with_capacity(1 + chunk.len());
            segment.push(flags);
            segment.extend_from_slice(chunk);
            segment
        })
        .collect()
}

/// Rebuilds a Ranging Data body from the segments a peer notified.
#[derive(Debug, Default, Clone)]
pub struct Reassembler {
    body: Vec<u8>,
    /// Whether a first segment has been seen, so a stream joined mid-body is
    /// discarded rather than delivered as a truncated measurement.
    started: bool,
    /// The counter the next segment must carry.
    expected_counter: u8,
}

impl Reassembler {
    /// A reassembler waiting for a first segment.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one notification value in, returning the complete body when the
    /// last segment arrives.
    ///
    /// A segment out of counter order means one was lost; the partial body is
    /// dropped rather than stitched, because a Ranging Data body with a hole
    /// in it decodes to tones that were never measured.
    pub fn push(&mut self, segment: &[u8]) -> Option<Vec<u8>> {
        let (&flags, payload) = segment.split_first()?;
        let counter = flags >> segmentation::COUNTER_SHIFT;
        let is_first = flags & segmentation::FIRST != 0;

        if is_first {
            self.body.clear();
            self.started = true;
            self.expected_counter = counter;
        } else if !self.started || counter != self.expected_counter {
            self.reset();
            return None;
        }
        self.body.extend_from_slice(payload);
        self.expected_counter = counter.wrapping_add(1) & 0x3F;

        if flags & segmentation::LAST != 0 {
            self.started = false;
            return Some(std::mem::take(&mut self.body));
        }
        None
    }

    /// Discards any partial body (on disconnect, or after a gap).
    pub fn reset(&mut self) {
        self.body.clear();
        self.started = false;
        self.expected_counter = 0;
    }
}

#[cfg(test)]
#[path = "ras_tests.rs"]
mod tests;
