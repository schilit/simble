// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! A minimal in-process LE controller (`SimController`) and shared medium
//! ([`Link`]) — enough of the Link Layer, modeled at the HCI boundary, to let
//! several Simble host stacks discover, connect to, and exchange data with one
//! another **in a single process, with no netsim, no Rootcanal, and no radio**.
//!
//! This is the lowest rung of Simble's controller ladder. It is deliberately a
//! thin HCI *matchmaker*, not a faithful controller: it routes advertising to
//! scanners, completes connections, shuttles ACL and ISO data between peers,
//! and fans a broadcast's ISO streams out to everyone synchronized to it — but
//! it models none of the PHY (channel hopping, timing, encryption, retries,
//! scheduling). For that fidelity, point a host at a real Rootcanal over the
//! WebSocket transport; for ranging and device movement, at netsim. Because it
//! is pure Rust with no FFI, it runs the same natively and on `wasm32`, so a
//! single web page can host a whole scene of devices.
//!
//! Two subsystems are modelled past plain routing, and both are modelled as
//! *sequencing only*:
//!
//! * **Channel Sounding** produces real tone phases from the devices'
//!   positions — `Link::tick_channel_sounding` is the one place the simulated
//!   radio computes physics rather than shuffling bytes. (Named, not linked:
//!   it is private, and rustdoc denies a public doc linking to a private item
//!   under `-D warnings`, which is how this reached CI.)
//! * **Periodic advertising and BIGs** carry an Auracast broadcast end to end,
//!   with the BIGInfo a receiver reads derived from the source's own
//!   `LE Create BIG`. Nothing about the air interface is simulated; what is
//!   simulated is which command is legal when, which event answers it, and who
//!   is told what when a BIG appears or goes away.
//!
//! HCI packets are parsed and built with zero-copy `#[repr(C)]` structs (the
//! same idiom as `crate::packets`), so the wire layouts are explicit rather
//! than hand-indexed byte offsets.
//!
//! ```
//! use simble::controller::sim::Link;
//! use simble::types::Address;
//!
//! let mut link = Link::new();
//! let adv = link.add_device("AA:BB:CC:00:00:01".parse::<Address>().unwrap());
//! let scan = link.add_device("AA:BB:CC:00:00:02".parse::<Address>().unwrap());
//!
//! adv.send_command(&[0x08, 0x20, 0x04, 0x03, 0x02, 0x01, 0x06]).unwrap(); // adv data
//! adv.send_command(&[0x0A, 0x20, 0x01, 0x01]).unwrap(); // LE Set Advertising Enable
//! scan.send_command(&[0x0C, 0x20, 0x02, 0x01, 0x00]).unwrap(); // LE Set Scan Enable
//!
//! link.tick(); // route advertising across the shared medium
//!
//! assert!(scan.poll_controller_packet().is_some()); // an LE Advertising Report
//! ```

use crate::controller::lmp::{LmpLink, LmpRole};
use crate::controller::propagation::{
    PathLossModel, Position, Rng, channel_frequency_hz, phase_noise_sigma_rad,
    propagation_phase_rad, wrap_phase,
};
use crate::packets::big::{
    LeBigCreateSyncHeader, LeBigInfoAdvertisingReportEvent, LeBigSyncEstablishedEventHeader,
    LeBigSyncLostEvent, LeBigTerminateSyncResponse, LeCreateBig, LeCreateBigCompleteEventHeader,
    LeTerminateBig, LeTerminateBigCompleteEvent, big_subevent_code,
};
use crate::packets::ext_adv::{
    ExtendedAdvertisingReportHeader, LeExtendedAdvertisingReportEvent,
    LePeriodicAdvertisingCreateSync, LePeriodicAdvertisingReportEventHeader,
    LePeriodicAdvertisingSyncEstablishedEvent, LeSetExtendedAdvertisingDataHeader,
    LeSetExtendedAdvertisingEnableHeader, LeSetExtendedAdvertisingParameters,
    LeSetExtendedScanEnable, LeSetPeriodicAdvertisingDataHeader, LeSetPeriodicAdvertisingEnable,
    U24 as ExtU24, adv_phy, data_operation, ext_adv_subevent_code,
};
use crate::transport::HciChannel;
use crate::transport::h4_type;
use crate::types::Address;
use std::collections::VecDeque;
use std::sync::Arc;
use zerocopy::byteorder::little_endian::U16;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Ref, Unaligned};

/// The HCI command opcodes the minimal controller acts on. Every other opcode
/// is answered with a success Command Complete so a host's bring-up sequence
/// never stalls on an unimplemented command.
mod opcode {
    use crate::packets::hci::cs_opcode;
    /// Disconnect (OGF 0x01, OCF 0x0006).
    pub const DISCONNECT: u16 = 0x0406;
    /// Reset (OGF 0x03, OCF 0x0003).
    pub const RESET: u16 = 0x0C03;
    /// Read BD_ADDR (OGF 0x04, OCF 0x0009).
    pub const READ_BD_ADDR: u16 = 0x1009;
    /// Read RSSI (OGF 0x05, OCF 0x0005).
    pub const READ_RSSI: u16 = 0x1405;
    /// LE Set Advertising Parameters (OGF 0x08, OCF 0x0006).
    pub const LE_SET_ADV_PARAMS: u16 = 0x2006;
    /// LE Set Advertising Data (OGF 0x08, OCF 0x0008).
    pub const LE_SET_ADV_DATA: u16 = 0x2008;
    /// LE Set Advertising Enable (OGF 0x08, OCF 0x000A).
    pub const LE_SET_ADV_ENABLE: u16 = 0x200A;
    /// LE Set Scan Enable (OGF 0x08, OCF 0x000C).
    pub const LE_SET_SCAN_ENABLE: u16 = 0x200C;
    /// LE Create Connection (OGF 0x08, OCF 0x000D).
    pub const LE_CREATE_CONNECTION: u16 = 0x200D;
    /// LE Create Connection Cancel (OGF 0x08, OCF 0x000E).
    pub const LE_CREATE_CONNECTION_CANCEL: u16 = 0x200E;
    /// LE Connection Update (OGF 0x08, OCF 0x0013).
    pub const LE_CONNECTION_UPDATE: u16 = 0x2013;
    /// LE Set PHY (OGF 0x08, OCF 0x0032).
    pub const LE_SET_PHY: u16 = 0x2032;
    /// LE Create CIS (OGF 0x08, OCF 0x0064).
    pub const LE_CREATE_CIS: u16 = 0x2064;
    /// LE Accept CIS Request (OGF 0x08, OCF 0x0066).
    pub const LE_ACCEPT_CIS_REQUEST: u16 = 0x2066;
    /// LE CS Security Enable (OGF 0x08, OCF 0x008C).
    pub const LE_CS_SECURITY_ENABLE: u16 = cs_opcode::LE_CS_SECURITY_ENABLE.as_u16();
    /// LE CS Create Config (OGF 0x08, OCF 0x0090).
    pub const LE_CS_CREATE_CONFIG: u16 = cs_opcode::LE_CS_CREATE_CONFIG.as_u16();
    /// LE CS Remove Config (OGF 0x08, OCF 0x0091).
    pub const LE_CS_REMOVE_CONFIG: u16 = cs_opcode::LE_CS_REMOVE_CONFIG.as_u16();
    /// LE CS Set Procedure Parameters (OGF 0x08, OCF 0x0093).
    pub const LE_CS_SET_PROCEDURE_PARAMETERS: u16 =
        cs_opcode::LE_CS_SET_PROCEDURE_PARAMETERS.as_u16();
    /// LE CS Procedure Enable (OGF 0x08, OCF 0x0094).
    pub const LE_CS_PROCEDURE_ENABLE: u16 = cs_opcode::LE_CS_PROCEDURE_ENABLE.as_u16();

    // --- BR/EDR (Bluetooth Classic) -------------------------------------
    //
    // Link Control is OGF 0x01, Controller & Baseband OGF 0x03. Which event
    // answers which of these is *not* uniform, and getting it wrong hangs a
    // host silently — see the table on `Link::handle_classic_command`.
    /// Inquiry (OGF 0x01, OCF 0x0001).
    pub const INQUIRY: u16 = 0x0401;
    /// Inquiry Cancel (OGF 0x01, OCF 0x0002).
    pub const INQUIRY_CANCEL: u16 = 0x0402;
    /// Create Connection (OGF 0x01, OCF 0x0005).
    pub const CREATE_CONNECTION: u16 = 0x0405;
    /// Create Connection Cancel (OGF 0x01, OCF 0x0008).
    pub const CREATE_CONNECTION_CANCEL: u16 = 0x0408;
    /// Accept Connection Request (OGF 0x01, OCF 0x0009).
    pub const ACCEPT_CONNECTION_REQUEST: u16 = 0x0409;
    /// Reject Connection Request (OGF 0x01, OCF 0x000A).
    pub const REJECT_CONNECTION_REQUEST: u16 = 0x040A;
    /// Remote Name Request (OGF 0x01, OCF 0x0019).
    pub const REMOTE_NAME_REQUEST: u16 = 0x0419;

    // --- SCO / eSCO (the synchronous, "call audio" links) ----------------
    /// Setup Synchronous Connection (OGF 0x01, OCF 0x0028).
    pub const SETUP_SYNCHRONOUS_CONNECTION: u16 = 0x0428;
    /// Accept Synchronous Connection Request (OGF 0x01, OCF 0x0029).
    pub const ACCEPT_SYNCHRONOUS_CONNECTION_REQUEST: u16 = 0x0429;
    /// Reject Synchronous Connection Request (OGF 0x01, OCF 0x002A).
    pub const REJECT_SYNCHRONOUS_CONNECTION_REQUEST: u16 = 0x042A;
    /// Enhanced Setup Synchronous Connection (OGF 0x01, OCF 0x003D).
    pub const ENHANCED_SETUP_SYNCHRONOUS_CONNECTION: u16 = 0x043D;
    /// Enhanced Accept Synchronous Connection Request (OGF 0x01, OCF 0x003E).
    pub const ENHANCED_ACCEPT_SYNCHRONOUS_CONNECTION_REQUEST: u16 = 0x043E;
    /// Write Local Name (OGF 0x03, OCF 0x0013).
    pub const WRITE_LOCAL_NAME: u16 = 0x0C13;
    /// Read Local Name (OGF 0x03, OCF 0x0014).
    pub const READ_LOCAL_NAME: u16 = 0x0C14;
    /// Read Scan Enable (OGF 0x03, OCF 0x0019).
    pub const READ_SCAN_ENABLE: u16 = 0x0C19;
    /// Write Scan Enable (OGF 0x03, OCF 0x001A).
    pub const WRITE_SCAN_ENABLE: u16 = 0x0C1A;
    /// Read Class of Device (OGF 0x03, OCF 0x0023).
    pub const READ_CLASS_OF_DEVICE: u16 = 0x0C23;
    /// Write Class of Device (OGF 0x03, OCF 0x0024).
    pub const WRITE_CLASS_OF_DEVICE: u16 = 0x0C24;

    // --- BR/EDR security: Secure Simple Pairing, link keys, encryption ---
    //
    // The answer split here is the one that hangs hosts, so it is spelled out
    // on `Link::handle_classic_security_command` rather than left to the
    // reader. Roughly: the *replies* a host sends to a controller's question
    // are Command Complete (they are the answer), and the commands that start
    // a procedure are Command Status (they promise a later event).
    /// Link Key Request Reply (OGF 0x01, OCF 0x000B).
    pub const LINK_KEY_REQUEST_REPLY: u16 = 0x040B;
    /// Link Key Request Negative Reply (OGF 0x01, OCF 0x000C).
    pub const LINK_KEY_REQUEST_NEGATIVE_REPLY: u16 = 0x040C;
    /// Authentication Requested (OGF 0x01, OCF 0x0011).
    pub const AUTHENTICATION_REQUESTED: u16 = 0x0411;
    /// Set Connection Encryption (OGF 0x01, OCF 0x0013).
    pub const SET_CONNECTION_ENCRYPTION: u16 = 0x0413;
    /// Change Connection Link Key (OGF 0x01, OCF 0x0015).
    pub const CHANGE_CONNECTION_LINK_KEY: u16 = 0x0415;
    /// Link Key Selection (OGF 0x01, OCF 0x0017).
    pub const LINK_KEY_SELECTION: u16 = 0x0417;
    /// IO Capability Request Reply (OGF 0x01, OCF 0x002B).
    pub const IO_CAPABILITY_REQUEST_REPLY: u16 = 0x042B;
    /// User Confirmation Request Reply (OGF 0x01, OCF 0x002C).
    pub const USER_CONFIRMATION_REQUEST_REPLY: u16 = 0x042C;
    /// User Confirmation Request Negative Reply (OGF 0x01, OCF 0x002D).
    pub const USER_CONFIRMATION_REQUEST_NEGATIVE_REPLY: u16 = 0x042D;
    /// User Passkey Request Reply (OGF 0x01, OCF 0x002E).
    pub const USER_PASSKEY_REQUEST_REPLY: u16 = 0x042E;
    /// User Passkey Request Negative Reply (OGF 0x01, OCF 0x002F).
    pub const USER_PASSKEY_REQUEST_NEGATIVE_REPLY: u16 = 0x042F;
    /// IO Capability Request Negative Reply (OGF 0x01, OCF 0x0034).
    pub const IO_CAPABILITY_REQUEST_NEGATIVE_REPLY: u16 = 0x0434;
    /// Read Simple Pairing Mode (OGF 0x03, OCF 0x0055).
    pub const READ_SIMPLE_PAIRING_MODE: u16 = 0x0C55;
    /// Write Simple Pairing Mode (OGF 0x03, OCF 0x0056).
    ///
    /// **0x0C56, not 0x0C45** — 0x0C45 is Write *Inquiry* Mode. The two get
    /// confused because both are Controller & Baseband writes named after a
    /// mode, and a host that sends one meaning the other silently gets the
    /// wrong feature.
    pub const WRITE_SIMPLE_PAIRING_MODE: u16 = 0x0C56;

    // --- LE encryption ----------------------------------------------------
    /// LE Enable Encryption (OGF 0x08, OCF 0x0019).
    pub const LE_ENABLE_ENCRYPTION: u16 = 0x2019;
    /// LE Long Term Key Request Reply (OGF 0x08, OCF 0x001A).
    pub const LE_LTK_REQUEST_REPLY: u16 = 0x201A;
    /// LE Long Term Key Request Negative Reply (OGF 0x08, OCF 0x001B).
    pub const LE_LTK_REQUEST_NEGATIVE_REPLY: u16 = 0x201B;

    use crate::packets::big::big_opcode;
    use crate::packets::ext_adv::ext_adv_opcode;

    /// LE Set Extended Advertising Parameters.
    pub const LE_SET_EXT_ADV_PARAMS: u16 =
        ext_adv_opcode::LE_SET_EXTENDED_ADVERTISING_PARAMETERS.as_u16();
    /// LE Set Extended Advertising Data.
    pub const LE_SET_EXT_ADV_DATA: u16 = ext_adv_opcode::LE_SET_EXTENDED_ADVERTISING_DATA.as_u16();
    /// LE Set Extended Advertising Enable.
    pub const LE_SET_EXT_ADV_ENABLE: u16 =
        ext_adv_opcode::LE_SET_EXTENDED_ADVERTISING_ENABLE.as_u16();
    /// LE Set Periodic Advertising Data.
    pub const LE_SET_PERIODIC_ADV_DATA: u16 =
        ext_adv_opcode::LE_SET_PERIODIC_ADVERTISING_DATA.as_u16();
    /// LE Set Periodic Advertising Enable.
    pub const LE_SET_PERIODIC_ADV_ENABLE: u16 =
        ext_adv_opcode::LE_SET_PERIODIC_ADVERTISING_ENABLE.as_u16();
    /// LE Set Extended Scan Enable.
    pub const LE_SET_EXT_SCAN_ENABLE: u16 = ext_adv_opcode::LE_SET_EXTENDED_SCAN_ENABLE.as_u16();
    /// LE Periodic Advertising Create Sync.
    pub const LE_PERIODIC_ADV_CREATE_SYNC: u16 =
        ext_adv_opcode::LE_PERIODIC_ADVERTISING_CREATE_SYNC.as_u16();
    /// LE Periodic Advertising Terminate Sync.
    pub const LE_PERIODIC_ADV_TERMINATE_SYNC: u16 =
        ext_adv_opcode::LE_PERIODIC_ADVERTISING_TERMINATE_SYNC.as_u16();
    /// LE Create BIG.
    pub const LE_CREATE_BIG: u16 = big_opcode::LE_CREATE_BIG.as_u16();
    /// LE Terminate BIG.
    pub const LE_TERMINATE_BIG: u16 = big_opcode::LE_TERMINATE_BIG.as_u16();
    /// LE BIG Create Sync.
    pub const LE_BIG_CREATE_SYNC: u16 = big_opcode::LE_BIG_CREATE_SYNC.as_u16();
    /// LE BIG Terminate Sync.
    pub const LE_BIG_TERMINATE_SYNC: u16 = big_opcode::LE_BIG_TERMINATE_SYNC.as_u16();
}

/// HCI event codes the controller generates.
mod event {
    /// Inquiry Complete event (0x01) — the *end* of an inquiry, and the only
    /// thing that tells a host discovery is over. It is not the answer to the
    /// Inquiry command; a Command Status is.
    pub const INQUIRY_COMPLETE: u8 = 0x01;
    /// Inquiry Result event (0x02).
    pub const INQUIRY_RESULT: u8 = 0x02;
    /// Connection Complete event (0x03), BR/EDR.
    pub const CONNECTION_COMPLETE: u8 = 0x03;
    /// Connection Request event (0x04) — a peer is paging us.
    pub const CONNECTION_REQUEST: u8 = 0x04;
    /// Disconnection Complete event (0x05).
    pub const DISCONNECTION_COMPLETE: u8 = 0x05;
    /// Authentication Complete event (0x06) — the completion event
    /// Authentication Requested promises, and it goes only to the host that
    /// asked. The *other* host learns a pairing happened from Simple Pairing
    /// Complete and Link Key Notification.
    pub const AUTHENTICATION_COMPLETE: u8 = 0x06;
    /// Remote Name Request Complete event (0x07).
    pub const REMOTE_NAME_REQUEST_COMPLETE: u8 = 0x07;
    /// Synchronous Connection Complete event (0x2C) — the completion event
    /// every one of the SCO/eSCO setup commands promises. There is no
    /// Synchronous Connection *Changed* (0x2D) here: nothing in this
    /// controller renegotiates a synchronous link's parameters once it is
    /// up, and an event nobody can cause is a fiction.
    pub const SYNCHRONOUS_CONNECTION_COMPLETE: u8 = 0x2C;
    /// Encryption Change event (0x08) — the completion event both
    /// Set Connection Encryption (BR/EDR) and LE Enable Encryption promise.
    pub const ENCRYPTION_CHANGE: u8 = 0x08;
    /// Change Connection Link Key Complete event (0x09).
    pub const CHANGE_CONNECTION_LINK_KEY_COMPLETE: u8 = 0x09;
    /// Link Key Request event (0x17) — the controller asking its host
    /// whether it is already bonded to this peer. The answer is what decides
    /// whether pairing runs at all.
    pub const LINK_KEY_REQUEST: u8 = 0x17;
    /// Link Key Notification event (0x18) — a new key for the host to store.
    pub const LINK_KEY_NOTIFICATION: u8 = 0x18;
    /// IO Capability Request event (0x31).
    pub const IO_CAPABILITY_REQUEST: u8 = 0x31;
    /// IO Capability Response event (0x32) — what the *peer's* host answered.
    pub const IO_CAPABILITY_RESPONSE: u8 = 0x32;
    /// User Confirmation Request event (0x33), carrying the six-digit value
    /// both ends are shown.
    pub const USER_CONFIRMATION_REQUEST: u8 = 0x33;
    /// User Passkey Request event (0x34) — asked of the side that can type.
    pub const USER_PASSKEY_REQUEST: u8 = 0x34;
    /// Simple Pairing Complete event (0x36).
    pub const SIMPLE_PAIRING_COMPLETE: u8 = 0x36;
    /// User Passkey Notification event (0x3B) — told to the side that can
    /// only display.
    pub const USER_PASSKEY_NOTIFICATION: u8 = 0x3B;
    /// Command Complete event (0x0E).
    pub const COMMAND_COMPLETE: u8 = 0x0E;
    /// Command Status event (0x0F).
    pub const COMMAND_STATUS: u8 = 0x0F;
    /// LE Meta event (0x3E).
    pub const LE_META: u8 = 0x3E;
    /// LE Connection Complete subevent (0x01).
    pub const LE_CONNECTION_COMPLETE: u8 = 0x01;
    /// LE Advertising Report subevent (0x02).
    pub const LE_ADVERTISING_REPORT: u8 = 0x02;
    /// LE CS Security Enable Complete subevent (0x2E).
    pub const LE_CS_SECURITY_ENABLE_COMPLETE: u8 = 0x2E;
    /// LE CS Config Complete subevent (0x2F).
    pub const LE_CS_CONFIG_COMPLETE: u8 = 0x2F;
    /// LE CS Procedure Enable Complete subevent (0x30).
    pub const LE_CS_PROCEDURE_ENABLE_COMPLETE: u8 = 0x30;
    /// LE CS Subevent Result subevent (0x31).
    pub const LE_CS_SUBEVENT_RESULT: u8 = 0x31;
    /// LE Connection Update Complete subevent (0x03) — the completion event
    /// LE Connection Update promises. Without it the host's Command Status is
    /// a promise the controller never keeps.
    pub const LE_CONNECTION_UPDATE_COMPLETE: u8 = 0x03;
    /// LE PHY Update Complete subevent (0x0C).
    pub const LE_PHY_UPDATE_COMPLETE: u8 = 0x0C;
    /// LE CIS Established subevent (0x19), version 1.
    pub const LE_CIS_ESTABLISHED: u8 = 0x19;
    /// LE CIS Request subevent (0x1A) — how a peripheral's host learns a
    /// central wants an isochronous stream to it.
    pub const LE_CIS_REQUEST: u8 = 0x1A;
    /// LE Long Term Key Request subevent (0x05) — the peripheral's
    /// controller asking its host for the key the central named.
    pub const LE_LONG_TERM_KEY_REQUEST: u8 = 0x05;
}

/// LE PHY identifiers (Vol 4, Part E, Section 7.8.49).
mod le_phy {
    /// LE 1M.
    pub const LE_1M: u8 = 0x01;
    /// LE 2M.
    pub const LE_2M: u8 = 0x02;
    /// LE Coded.
    pub const LE_CODED: u8 = 0x03;
}

/// Channel Sounding parameters this controller models.
///
/// Simble's radio reports one subevent per [`Link::tick`], carrying one
/// mode-2 (Phase-Based Ranging) step per tone. The tone plan is fixed rather
/// than driven by the host's channel map, because the *spacing* is what
/// bounds the measurement and a demo that let a page choose it could quietly
/// produce nonsense — see `TONE_SPACING_CHANNELS`.
pub mod cs_plan {
    /// The first channel index a tone is placed on.
    pub const FIRST_TONE_CHANNEL: u8 = 0;

    /// Channel indices (1 MHz each) between adjacent tones.
    ///
    /// Phase-Based Ranging recovers distance from how fast phase rotates with
    /// frequency, so it can only see rotations smaller than half a turn
    /// between neighbouring tones. That caps the unambiguous range at
    /// `c / (4·Δf)`: 37.5 m at this 2 MHz spacing. Wider spacing measures
    /// more precisely over the same number of tones and wraps sooner — the
    /// central trade-off of the technique.
    pub const TONE_SPACING_CHANNELS: u8 = 2;

    /// Tones per subevent. Nineteen mode-2 steps plus the subevent header is
    /// 245 bytes, just inside an HCI event's 255-byte parameter budget.
    pub const TONES_PER_SUBEVENT: usize = 19;

    /// Antenna paths reported. One path plus the mandatory extension slot is
    /// the 1:1 antenna configuration every CS-capable radio supports.
    pub const NUM_ANTENNA_PATHS: u8 = 1;

    /// Step mode 2: Phase-Based Ranging, the mode that carries tone PCTs.
    pub const STEP_MODE_PBR: u8 = 2;

    /// "All results complete" for the procedure/subevent done status fields.
    pub const DONE_STATUS_COMPLETE: u8 = 0x00;

    /// Tone Quality Indicator: "tone quality is high" (Vol 4, Part E,
    /// Section 7.7.65.44).
    pub const TONE_QUALITY_HIGH: u8 = 0x00;

    /// The unambiguous range of this tone plan, in metres.
    pub fn unambiguous_range_m() -> f64 {
        crate::types::SPEED_OF_LIGHT_M_PER_S
            / (4.0
                * f64::from(TONE_SPACING_CHANNELS)
                * crate::controller::propagation::CHANNEL_SPACING_HZ)
    }

    /// The channel indices tones are placed on, low frequency first.
    pub fn tone_channels() -> Vec<u8> {
        (0..TONES_PER_SUBEVENT)
            .map(|i| FIRST_TONE_CHANNEL + TONE_SPACING_CHANNELS * i as u8)
            .collect()
    }
}

/// `STATUS_SUCCESS` (0x00) HCI status code.
const STATUS_SUCCESS: u8 = 0x00;
/// Disconnection reason: "Connection Terminated By Local Host" (0x16).
const REASON_LOCAL_HOST: u8 = 0x16;
/// Disconnection reason: "Remote User Terminated Connection" (0x13).
const REASON_REMOTE_USER: u8 = 0x13;
/// `INVALID_HCI_COMMAND_PARAMETERS` (0x12).
const STATUS_INVALID_PARAMETERS: u8 = 0x12;
/// `UNKNOWN_CONNECTION_IDENTIFIER` (0x02) — the handle names no live
/// connection.
const STATUS_UNKNOWN_CONNECTION: u8 = 0x02;
/// `COMMAND_DISALLOWED` (0x0C) — the command is well formed but not legal in
/// the controller's current state.
const STATUS_COMMAND_DISALLOWED: u8 = 0x0C;
/// `UNKNOWN_ADVERTISING_IDENTIFIER` (0x42) — the advertising or periodic sync
/// handle names nothing this controller issued.
const STATUS_UNKNOWN_ADVERTISING_ID: u8 = 0x42;
/// `CONNECTION_FAILED_TO_BE_ESTABLISHED` (0x3E) — used here for a BIG that
/// could not be decrypted with the code the receiver offered.
const STATUS_CONNECTION_FAILED: u8 = 0x3E;
/// `PAGE_TIMEOUT` (0x04) — nobody answered the page. In this controller that
/// means the address names no device in the scene, or names one whose host
/// never enabled page scan.
const STATUS_PAGE_TIMEOUT: u8 = 0x04;
/// `CONNECTION_ALREADY_EXISTS` (0x0B).
const STATUS_CONNECTION_ALREADY_EXISTS: u8 = 0x0B;
/// `CONNECTION_REJECTED_DUE_TO_LIMITED_RESOURCES` (0x0D) — what a host that
/// answers a page with Reject Connection Request usually says.
const STATUS_CONNECTION_REJECTED_RESOURCES: u8 = 0x0D;
/// `AUTHENTICATION_FAILURE` (0x05) — the pairing was refused. This is what a
/// User Confirmation Request Negative Reply turns into on the wire.
const STATUS_AUTHENTICATION_FAILURE: u8 = 0x05;
/// `PIN_OR_KEY_MISSING` (0x06) — encryption was asked for on a link that has
/// no key to encrypt with.
const STATUS_PIN_OR_KEY_MISSING: u8 = 0x06;
/// `PAIRING_NOT_ALLOWED` (0x18) — pairing would be needed and cannot run.
/// Here that means a host that never sent Write Simple Pairing Mode: this
/// controller models SSP and not legacy PIN pairing, and says so rather than
/// running SSP behind the host's back.
const STATUS_PAIRING_NOT_ALLOWED: u8 = 0x18;
/// `UNKNOWN_HCI_COMMAND` (0x01) — this controller does not implement the
/// command at all. Said out loud, in a Command Status, rather than implied by
/// silence or contradicted by a success: a host can retry, fall back, or fail
/// fast on it, and none of those is possible if the answer is a lie.
const STATUS_UNKNOWN_HCI_COMMAND: u8 = 0x01;

/// The opcodes the Core specification answers with a **Command Status** and
/// never a Command Complete (Core v6.3, Vol 4, Part E — 61 of the 339
/// commands).
///
/// A controller gets exactly one choice per command: Command Complete, which
/// *is* the result, or Command Status, which promises a later completion
/// event. Answering a Command-Status-only command with a Command Complete
/// does not fail — it hangs. The host takes the Complete as "in progress,
/// wait for the real event" is not needed, or worse, never sees the event it
/// is blocked on. Nothing crashes and nothing logs. That shape produced five
/// bugs here in two weeks, all of them because the catch-all below answered
/// everything it did not recognise with Command Complete.
///
/// So the catch-all consults this table. An opcode listed here that this
/// controller does not model is answered with a Command Status carrying
/// [`STATUS_UNKNOWN_HCI_COMMAND`] — still wrong about the *behaviour*, but no
/// longer wrong about the *shape*, which is the part that hangs.
///
/// `scripts/check_hci_command_answers.py` derives this list from the SIG's
/// published Core HTML, cross-checks it against Bumble's
/// `HCI_AsyncCommand`/`HCI_SyncCommand` split, and fails if this table or any
/// explicit arm below drifts from it. Do not edit it by hand — run
/// `--emit-table`.
#[rustfmt::skip]
const COMMAND_STATUS_OPCODES: &[u16] = &[
    0x0401, // 7.1.1     HCI_Inquiry
    0x0405, // 7.1.5     HCI_Create_Connection
    0x0406, // 7.1.6     HCI_Disconnect
    0x0409, // 7.1.8     HCI_Accept_Connection_Request
    0x040A, // 7.1.9     HCI_Reject_Connection_Request
    0x040F, // 7.1.14    HCI_Change_Connection_Packet_Type
    0x0411, // 7.1.15    HCI_Authentication_Requested
    0x0413, // 7.1.16    HCI_Set_Connection_Encryption
    0x0415, // 7.1.17    HCI_Change_Connection_Link_Key
    0x0417, // 7.1.18    HCI_Link_Key_Selection
    0x0419, // 7.1.19    HCI_Remote_Name_Request
    0x041B, // 7.1.21    HCI_Read_Remote_Supported_Features
    0x041C, // 7.1.22    HCI_Read_Remote_Extended_Features
    0x041D, // 7.1.23    HCI_Read_Remote_Version_Information
    0x041F, // 7.1.24    HCI_Read_Clock_Offset
    0x0428, // 7.1.26    HCI_Setup_Synchronous_Connection
    0x0429, // 7.1.27    HCI_Accept_Synchronous_Connection_Request
    0x042A, // 7.1.28    HCI_Reject_Synchronous_Connection_Request
    0x043D, // 7.1.45    HCI_Enhanced_Setup_Synchronous_Connection
    0x043E, // 7.1.46    HCI_Enhanced_Accept_Synchronous_Connection_Request
    0x043F, // 7.1.47    HCI_Truncated_Page
    0x0443, // 7.1.51    HCI_Start_Synchronization_Train
    0x0444, // 7.1.52    HCI_Receive_Synchronization_Train
    0x0801, // 7.2.1     HCI_Hold_Mode
    0x0803, // 7.2.2     HCI_Sniff_Mode
    0x0804, // 7.2.3     HCI_Exit_Sniff_Mode
    0x0807, // 7.2.6     HCI_QoS_Setup
    0x080B, // 7.2.8     HCI_Switch_Role
    0x0810, // 7.2.13    HCI_Flow_Specification
    0x0C53, // 7.3.57    HCI_Refresh_Encryption_Key
    0x0C5F, // 7.3.66    HCI_Enhanced_Flush
    0x200D, // 7.8.12    HCI_LE_Create_Connection
    0x2013, // 7.8.18    HCI_LE_Connection_Update
    0x2016, // 7.8.21    HCI_LE_Read_Remote_Features_Page_0
    0x2019, // 7.8.24    HCI_LE_Enable_Encryption
    0x2025, // 7.8.36    HCI_LE_Read_Local_P-256_Public_Key
    0x2026, // 7.8.37    HCI_LE_Generate_DHKey [v1]
    0x2032, // 7.8.49    HCI_LE_Set_PHY
    0x2043, // 7.8.66    HCI_LE_Extended_Create_Connection [v1]
    0x2044, // 7.8.67    HCI_LE_Periodic_Advertising_Create_Sync
    0x205E, // 7.8.37    HCI_LE_Generate_DHKey [v2]
    0x2064, // 7.8.99    HCI_LE_Create_CIS
    0x2066, // 7.8.101   HCI_LE_Accept_CIS_Request
    0x2068, // 7.8.103   HCI_LE_Create_BIG
    0x2069, // 7.8.104   HCI_LE_Create_BIG_Test
    0x206A, // 7.8.105   HCI_LE_Terminate_BIG
    0x206B, // 7.8.106   HCI_LE_BIG_Create_Sync
    0x206D, // 7.8.108   HCI_LE_Request_Peer_SCA
    0x2077, // 7.8.118   HCI_LE_Read_Remote_Transmit_Power_Level
    0x207E, // 7.8.124   HCI_LE_Subrate_Request
    0x2085, // 7.8.66    HCI_LE_Extended_Create_Connection [v2]
    0x2088, // 7.8.129   HCI_LE_Read_All_Remote_Features
    0x208A, // 7.8.131   HCI_LE_CS_Read_Remote_Supported_Capabilities
    0x208C, // 7.8.133   HCI_LE_CS_Security_Enable
    0x208E, // 7.8.135   HCI_LE_CS_Read_Remote_FAE_Table
    0x2090, // 7.8.137   HCI_LE_CS_Create_Config
    0x2091, // 7.8.138   HCI_LE_CS_Remove_Config
    0x2094, // 7.8.141   HCI_LE_CS_Procedure_Enable
    0x2096, // 7.8.143   HCI_LE_CS_Test_End
    0x209D, // 7.8.151   HCI_LE_Frame_Space_Update
    0x20A1, // 7.8.154   HCI_LE_Connection_Rate_Request
];

/// Whether the Core specification answers `opcode` with a Command Status.
fn answered_by_command_status(opcode: u16) -> bool {
    COMMAND_STATUS_OPCODES.contains(&opcode)
}

/// Which PHY to move to, given the bitmask of the ones the host will allow
/// (bit 0 LE 1M, bit 1 LE 2M, bit 2 LE Coded) and the one in use now.
///
/// The spec leaves the choice to the controller. This one takes the fastest
/// the host permits, and keeps the current PHY when the host permits nothing —
/// which is what an empty mask means, not an error.
fn preferred_phy(allowed: u8, current: u8) -> u8 {
    if allowed & 0x02 != 0 {
        le_phy::LE_2M
    } else if allowed & 0x01 != 0 {
        le_phy::LE_1M
    } else if allowed & 0x04 != 0 {
        le_phy::LE_CODED
    } else {
        current
    }
}

/// The connection interval every link starts at, in 1.25 ms units (30 ms).
const DEFAULT_CONN_INTERVAL: u16 = 0x0018;
/// The supervision timeout every link starts at, in 10 ms units (420 ms).
const DEFAULT_SUPERVISION_TIMEOUT: u16 = 0x002A;

/// Scan Enable bits (Vol 4, Part E, Section 7.3.18). A device with neither
/// bit set is genuinely invisible: not discoverable, not connectable. That is
/// modelled rather than treated as an error, because "I forgot Write Scan
/// Enable" is the single most common reason a real BR/EDR peripheral is never
/// found, and a simulator that quietly made it work would hide the bug.
mod scan_enable {
    /// Inquiry scan enabled — the device answers inquiries (discoverable).
    pub const INQUIRY: u8 = 0x01;
    /// Page scan enabled — the device answers pages (connectable).
    pub const PAGE: u8 = 0x02;
}

/// Link type 0x01 = ACL, in Connection Request / Connection Complete.
const LINK_TYPE_ACL: u8 = 0x01;

// --- SCO / eSCO ------------------------------------------------------------
//
// Everything below models the *sequencing* of a synchronous connection:
// which handles exist, which packets route where, and what events fire in
// what order. It deliberately models none of the air interface — no reserved
// slots, no eSCO retransmission window, no 3.75 ms interval, no loss. A tick
// here is not a unit of time, and a simulator that pretended otherwise would
// be competing with rootcanal/netsim, which do that job properly. The
// interval and window fields of the completion event below are reported as
// "do not care" for exactly that reason.

/// Link type 0x00 = SCO, in Connection Request / Synchronous Connection
/// Complete.
const LINK_TYPE_SCO: u8 = 0x00;
/// Link type 0x02 = eSCO.
const LINK_TYPE_ESCO: u8 = 0x02;

/// eSCO packet-type bits in Setup Synchronous Connection's `Packet_Type`
/// (Vol 4, Part E, Section 7.1.26): EV3, EV4, EV5. A host asking for any of
/// them is asking for an extended synchronous link; a host asking only for
/// HV1/HV2/HV3 is asking for a plain SCO one.
const ESCO_PACKET_TYPES: u16 = 0x0008 | 0x0010 | 0x0020;

/// Air Mode, the last field of Synchronous Connection Complete (Vol 4, Part
/// E, Section 7.7.35). Note that these numbers are *not* the Voice Setting's
/// air coding format numbers, which is a transcription trap: CVSD is air
/// coding format 0 but air mode 2.
mod air_mode {
    /// μ-law log.
    pub const U_LAW: u8 = 0x00;
    /// A-law log.
    pub const A_LAW: u8 = 0x01;
    /// CVSD — what a plain SCO narrowband call runs.
    pub const CVSD: u8 = 0x02;
    /// Transparent data — the controller does not touch the payload. This is
    /// what wideband speech (mSBC over eSCO) rides on.
    pub const TRANSPARENT: u8 = 0x03;
}

/// The air mode a Voice Setting (Vol 4, Part E, Section 6.12) asks for. Bits
/// 1:0 are the air coding format: 0 CVSD, 1 μ-law, 2 A-law, 3 transparent.
fn air_mode_of_voice_setting(voice_setting: u16) -> u8 {
    match voice_setting & 0x0003 {
        0 => air_mode::CVSD,
        1 => air_mode::U_LAW,
        2 => air_mode::A_LAW,
        _ => air_mode::TRANSPARENT,
    }
}

/// The air mode a coding format asks for, as Enhanced Setup Synchronous
/// Connection names it: a five-octet Coding_Format whose first octet is an
/// Assigned Numbers codec ID.
///
/// mSBC maps to **transparent**, not to a codec ID of its own: the controller
/// carries mSBC frames without touching them, which is exactly what
/// transparent means. Reporting `0x05` here would be reporting a mode the
/// event's field does not have.
fn air_mode_of_coding_format(codec_id: u8) -> u8 {
    match codec_id {
        0x00 => air_mode::U_LAW,
        0x01 => air_mode::A_LAW,
        0x02 => air_mode::CVSD,
        _ => air_mode::TRANSPARENT,
    }
}

// --- Secure Simple Pairing ------------------------------------------------

/// IO capabilities, as a host reports them in IO Capability Request Reply
/// (Vol 4, Part E, Section 7.7.40). The four values and their order are what
/// the association-model table below is indexed by, so they are named rather
/// than written as bare integers at the call sites.
mod io_capability {
    /// The device can show a number but has no yes/no input.
    pub const DISPLAY_ONLY: u8 = 0x00;
    /// The device can show a number and take a yes/no answer.
    pub const DISPLAY_YES_NO: u8 = 0x01;
    /// The device can take digits but shows nothing.
    pub const KEYBOARD_ONLY: u8 = 0x02;
    /// Neither. A headset button is not an input for this purpose.
    pub const NO_INPUT_NO_OUTPUT: u8 = 0x03;
}

/// The `Authentication_Requirements` bit that says "MITM protection
/// required". The field is 0x00–0x05 (`…_NO_BONDING`, `…_DEDICATED_BONDING`,
/// `…_GENERAL_BONDING`, each with and without MITM), and the odd values are
/// the MITM ones — so this is a bit test, not an equality test.
const AUTH_REQ_MITM: u8 = 0x01;

/// How the two devices authenticate each other in stage 1 of SSP.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum AssociationModel {
    /// Numeric Comparison with automatic confirmation. The controller still
    /// sends User Confirmation Request; what differs is that no human is
    /// expected to look at it, so the resulting key is *unauthenticated*.
    JustWorks,
    /// Numeric Comparison: both hosts show the same six digits and a person
    /// says whether they match. The only model here that resists MITM
    /// without a keyboard.
    NumericComparison,
    /// Passkey Entry: one side displays six digits, the other types them.
    PasskeyEntry,
}

/// Which association model two devices use, from their IO capabilities and
/// authentication requirements.
///
/// The source is Core Vol 3, Part C, Section 5.2.2.6 — the same table Zephyr
/// keeps in `subsys/bluetooth/host/classic/ssp.c` as `ssp_method[remote][local]`
/// and Bumble in `device.py`'s `on_authentication_user_confirmation_request`.
/// Both were read; they agree, and this agrees with them.
///
/// Two rules, in order:
///
/// 1. If **neither** side set the MITM bit, the model is Just Works —
///    Numeric Comparison with automatic confirmation at both ends. The table
///    below never gets consulted. This is the rule a table-only reading
///    misses, and it is why two `DisplayYesNo` devices that both asked for no
///    MITM still pair without a prompt.
/// 2. Otherwise the table applies: a `NoInputNoOutput` anywhere forces Just
///    Works (there is nothing to compare with), a `KeyboardOnly` opposite
///    anything that can display gives Passkey Entry, two `DisplayYesNo`
///    devices give Numeric Comparison, and everything else — `DisplayOnly`
///    against a display — is Numeric Comparison with the confirmation
///    automatic on the side that cannot answer, i.e. Just Works.
///
/// Out-of-band data is not modelled: this controller has no OOB channel to
/// carry it over, so the `oob_data_present` byte a host sends is recorded and
/// ignored rather than quietly changing the model.
fn association_model(
    local_io: u8,
    local_auth: u8,
    remote_io: u8,
    remote_auth: u8,
) -> AssociationModel {
    if local_auth & AUTH_REQ_MITM == 0 && remote_auth & AUTH_REQ_MITM == 0 {
        return AssociationModel::JustWorks;
    }
    use io_capability::{DISPLAY_ONLY, DISPLAY_YES_NO, KEYBOARD_ONLY, NO_INPUT_NO_OUTPUT};
    match (local_io, remote_io) {
        (NO_INPUT_NO_OUTPUT, _) | (_, NO_INPUT_NO_OUTPUT) => AssociationModel::JustWorks,
        (KEYBOARD_ONLY, _) | (_, KEYBOARD_ONLY) => AssociationModel::PasskeyEntry,
        (DISPLAY_YES_NO, DISPLAY_YES_NO) => AssociationModel::NumericComparison,
        (DISPLAY_ONLY, _) | (_, DISPLAY_ONLY) => AssociationModel::JustWorks,
        // Every value is covered above; an IO capability outside 0x00–0x03 is
        // not a capability this controller can reason about, and the safe
        // reading of "I do not understand your input hardware" is the model
        // that asks it for nothing.
        _ => AssociationModel::JustWorks,
    }
}

/// The AES-CMAC key that separates simble's derived pairing material from
/// every other use of the same primitive.
const PAIRING_DOMAIN: [u8; 16] = *b"simble link key ";

/// The link key a completed pairing hands both hosts.
///
/// **This is not the specification's f2.** Real SSP derives the key from a
/// P-192 or P-256 ECDH shared secret through HMAC-SHA-256, and the point of
/// this controller is the *sequence* — which command is legal when, which
/// event answers it, and who is told what — not the cryptography, which
/// rootcanal and real silicon already provide.
///
/// What a link key has to be for the sequence to work is exactly three
/// things: the same sixteen bytes at both ends, different for every pair of
/// devices, and *stable across reconnects* so that a key a host stored still
/// matches the one the controller would derive next time. AES-CMAC (the real
/// one, from [`crate::crypto`]) over the two addresses in a fixed order is
/// all three.
fn derived_link_key(a: Address, b: Address) -> [u8; 16] {
    let (low, high) = if a.to_be_bytes() <= b.to_be_bytes() {
        (a, b)
    } else {
        (b, a)
    };
    let mut input = [0u8; 12];
    input[..6].copy_from_slice(&low.to_be_bytes());
    input[6..].copy_from_slice(&high.to_be_bytes());
    crate::crypto::aes_cmac(&PAIRING_DOMAIN, &input)
}

/// Six decimal digits derived from a link key: the value User Confirmation
/// Request shows (`tag` 0) and the passkey Passkey Entry uses (`tag` 1).
///
/// Real SSP computes these with g and f4 over the ECDH public keys and the
/// pairing nonces, so they differ on every attempt. These do not — the same
/// pair of devices sees the same digits every time, which is wrong for a real
/// radio and exactly right for a test that wants to assert on them.
fn pairing_digits(link_key: &[u8; 16], tag: u8) -> u32 {
    let out = crate::crypto::aes_cmac(link_key, &[tag]);
    u32::from_be_bytes([out[0], out[1], out[2], out[3]]) % 1_000_000
}

/// Link key types, as Link Key Notification reports them (Vol 4, Part E,
/// Section 7.7.24). This controller models P-192 SSP, so it reports the P-192
/// key types and starts E0-era encryption — claiming a P-256 key type while
/// reporting `Encryption_Enabled = 0x01` would be two halves of two different
/// stories.
mod link_key_type {
    /// Unauthenticated Combination key from P-192 — what Just Works produces,
    /// and the reason a Just Works bond does not satisfy a service that
    /// requires MITM protection.
    pub const UNAUTHENTICATED_P192: u8 = 0x04;
    /// Authenticated Combination key from P-192 — Numeric Comparison or
    /// Passkey Entry, where a person was in the loop.
    pub const AUTHENTICATED_P192: u8 = 0x05;
}

/// `Encryption_Enabled` in Encryption Change: 0x01 is "on", which for BR/EDR
/// means E0 and for LE means AES-CCM. 0x02 (BR/EDR AES-CCM) would require
/// Secure Connections host support, which this controller does not model.
const ENCRYPTION_ON: u8 = 0x01;
/// `Encryption_Enabled` = off.
const ENCRYPTION_OFF: u8 = 0x00;

/// What one host answered its IO Capability Request with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct IoCapabilities {
    /// `IO_Capability` (see [`io_capability`]).
    io: u8,
    /// `OOB_Data_Present`. Recorded and not acted on — see
    /// [`association_model`].
    oob: u8,
    /// `Authentication_Requirements`.
    auth: u8,
}

/// Where a pairing conversation has got to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PairingStage {
    /// Both hosts have been asked for a stored link key; neither, one, or
    /// both answers are in. Two matching keys end the whole thing here — that
    /// is what "a reconnect skips SSP" *is*.
    LinkKey,
    /// Both hosts have been asked for their IO capabilities.
    IoCapability,
    /// Both hosts have been asked for a user decision: User Confirmation
    /// Request, or User Passkey Request / Notification.
    UserAction,
}

/// One Secure Simple Pairing conversation.
///
/// It lives on the [`Link`] rather than on either controller because it has
/// two ends and neither owns it: every step is "one host answered, so tell
/// the other one". A pairing is created by Authentication Requested and
/// destroyed the moment it resolves, in either direction — there is no state
/// left behind for a later connection to inherit.
struct ClassicPairing {
    /// The ACL connection being authenticated.
    handle: u16,
    /// Controller indices of the two ends. `ends[0]` is the side whose host
    /// sent Authentication Requested, and the only side that will be sent an
    /// Authentication Complete.
    ends: [usize; 2],
    /// What each host answered Link Key Request with. `None` = has not
    /// answered; `Some(None)` = Link Key Request Negative Reply.
    link_key: [Option<Option<[u8; 16]>>; 2],
    /// What each host answered IO Capability Request with. `None` = has not
    /// answered; a negative reply resolves the pairing instead of landing
    /// here.
    io: [Option<IoCapabilities>; 2],
    /// Each host's user decision: confirm/reject, or "a passkey was
    /// supplied".
    decision: [Option<bool>; 2],
    /// The six digits each side ended up with under Passkey Entry — typed on
    /// a keyboard, or notified to a display. Success needs the two to be
    /// equal, which is the *only* correct check: with a keyboard at both ends
    /// the controller never told either side what to type, so comparing
    /// against its own generated passkey would reject a pairing the user got
    /// right.
    entered: [Option<u32>; 2],
    stage: PairingStage,
    /// The key this pairing will notify if it succeeds.
    key: [u8; 16],
    /// The six digits both hosts are shown in User Confirmation Request.
    numeric_value: u32,
    /// The six digits Passkey Entry displays on one side and asks for on the
    /// other.
    passkey: u32,
    /// Which model the IO capabilities selected, once both are in.
    model: AssociationModel,
}

impl ClassicPairing {
    /// Which end of this pairing controller `index` is, if either.
    fn side_of(&self, index: usize) -> Option<usize> {
        self.ends.iter().position(|end| *end == index)
    }
}

/// The page-0 LMP feature mask both ends of a simulated classic link
/// advertise. Nominal: this controller implements no optional LMP feature, so
/// the mask exists to give [`LmpLink`]'s feature exchange something to
/// converge on rather than to describe a capability any code consults.
const LMP_FEATURES: [u8; 8] = [0xFF, 0xFF, 0x8F, 0xFE, 0xDB, 0xFF, 0x5B, 0x87];

/// Shuttle LMP PDUs between the two ends of a link until neither has anything
/// more to say.
///
/// The exchange is synchronous and short — `accepted`, `features_req`,
/// `features_res` — so it settles well inside the bound, and running it
/// entirely within one [`Link::tick`] keeps a classic connection exactly as
/// many ticks deep as an LE one. The bound is a guard against a future PDU
/// that answers itself, not a real limit.
fn shuttle_lmp(from: &mut LmpLink, to: &mut LmpLink, initial: Vec<Vec<u8>>) {
    let mut to_peer = initial;
    let mut to_self: Vec<Vec<u8>> = Vec::new();
    for _ in 0..8 {
        if to_peer.is_empty() && to_self.is_empty() {
            return;
        }
        let mut next_to_self = Vec::new();
        for pdu in to_peer.drain(..) {
            next_to_self.extend(to.receive(&pdu).unwrap_or_default());
        }
        let mut next_to_peer = Vec::new();
        for pdu in to_self.drain(..) {
            next_to_peer.extend(from.receive(&pdu).unwrap_or_default());
        }
        to_peer = next_to_peer;
        to_self = next_to_self;
    }
}

/// How many [`Link::tick`]s a page waits for an answer before the initiator's
/// host is told Page Timeout. A page is not instantaneous — the target's host
/// must see a Connection Request and answer it — so the initiator has to
/// tolerate at least a few ticks of silence, and a target that is simply not
/// there has to stop being waited for.
const PAGE_TIMEOUT_TICKS: u32 = 8;

// --- zero-copy HCI packet layouts ------------------------------------------

/// HCI command packet header: opcode then parameter-total-length (the byte
/// after the H4 type byte).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout)]
struct CommandHeader {
    /// Command opcode (OGF/OCF), little-endian.
    opcode: U16,
    /// Length of the parameters that follow.
    parameter_length: u8,
}

/// The leading fixed fields of LE Create Connection, up to and including the
/// peer address — enough to learn who the host wants to connect to.
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout)]
struct LeCreateConnectionPrefix {
    /// LE scan interval.
    scan_interval: U16,
    /// LE scan window.
    scan_window: U16,
    /// Initiator filter policy.
    initiator_filter_policy: u8,
    /// Peer address type.
    peer_address_type: u8,
    /// Peer device address (little-endian on the wire).
    peer_address: [u8; 6],
}

/// HCI ACL data packet header (handle + flags, then payload length).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout)]
struct AclHeader {
    /// Lower 12 bits connection handle; upper 4 bits PB/BC flags.
    handle_and_flags: U16,
    /// Payload length in this fragment.
    data_length: U16,
}

/// HCI synchronous (SCO) data packet header (Vol 4, Part E, Section 5.4.3).
///
/// Shaped like [`AclHeader`] except that the length is a single octet — a
/// synchronous payload never exceeds 255 bytes — and the top nibble carries a
/// two-bit Packet_Status_Flag rather than PB/BC flags.
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout)]
struct ScoHeader {
    /// Lower 12 bits connection handle; bits 12-13 Packet_Status_Flag.
    handle_and_flags: U16,
    /// Payload length, in octets.
    data_total_length: u8,
}

/// Command Complete event body: `num_hci_command_packets`, the opcode, then the
/// command's return parameters (status first).
#[repr(C)]
#[derive(IntoBytes, Immutable)]
struct CommandCompleteHeader {
    /// Number of HCI command packets the host may now send (always 1 here).
    num_hci_command_packets: u8,
    /// Opcode of the completed command.
    opcode: U16,
}

/// Command Status event body.
#[repr(C)]
#[derive(IntoBytes, Immutable)]
struct CommandStatusBody {
    /// Status of the command.
    status: u8,
    /// Number of HCI command packets the host may now send.
    num_hci_command_packets: u8,
    /// Opcode of the command whose status this reports.
    opcode: U16,
}

/// LE Connection Complete subevent body (fixed-size).
#[repr(C)]
#[derive(IntoBytes, Immutable)]
struct LeConnectionCompleteBody {
    /// LE Meta subevent code (0x01).
    subevent_code: u8,
    /// Connection status.
    status: u8,
    /// Assigned connection handle.
    connection_handle: U16,
    /// Local role: 0x00 central, 0x01 peripheral.
    role: u8,
    /// Peer address type (0x00 public).
    peer_address_type: u8,
    /// Peer device address (little-endian).
    peer_address: [u8; 6],
    /// Connection interval (units of 1.25 ms).
    connection_interval: U16,
    /// Peripheral latency (in connection events).
    peripheral_latency: U16,
    /// Supervision timeout (units of 10 ms).
    supervision_timeout: U16,
    /// Central clock accuracy.
    central_clock_accuracy: u8,
}

/// LE CS Procedure Enable Complete subevent body (Vol 4, Part E, Section
/// 7.7.65.48), after the subevent code.
#[repr(C)]
#[derive(IntoBytes, Immutable)]
struct LeCsProcedureEnableCompleteBody {
    /// Status of the enable/disable request.
    status: u8,
    /// The connection the procedure runs on.
    connection_handle: U16,
    /// Which configuration was enabled.
    config_id: u8,
    /// 0x00 disabled, 0x01 enabled.
    state: u8,
    /// Antenna configuration index (0x00 = 1:1).
    tone_antenna_config_selection: u8,
    /// Transmit power the controller selected, in dBm.
    selected_tx_power: i8,
    /// Subevent length in microseconds (24-bit).
    subevent_len: [u8; 3],
    /// Subevents per CS event.
    subevents_per_event: u8,
    /// Time between subevents, in 625 µs slots.
    subevent_interval: U16,
    /// Time between CS events, in connection intervals.
    event_interval: U16,
    /// Time between procedures, in connection intervals.
    procedure_interval: U16,
    /// How many procedures will run (0 = until disabled).
    procedure_count: U16,
    /// Maximum procedure duration, in 625 µs slots.
    max_procedure_len: U16,
}

/// LE Advertising Report subevent header (one report), before the variable data
/// and trailing RSSI byte.
#[repr(C)]
#[derive(IntoBytes, Immutable)]
struct LeAdvertisingReportHeader {
    /// LE Meta subevent code (0x02).
    subevent_code: u8,
    /// Number of reports (always 1 here).
    num_reports: u8,
    /// Advertising event type (ADV_IND etc.).
    event_type: u8,
    /// Advertiser address type.
    address_type: u8,
    /// Advertiser address (little-endian).
    address: [u8; 6],
    /// Length of the advertising data that follows.
    data_length: u8,
}

/// Disconnection Complete event body.
#[repr(C)]
#[derive(IntoBytes, Immutable)]
struct DisconnectionCompleteBody {
    /// Status of the disconnection.
    status: u8,
    /// Handle of the now-closed connection.
    connection_handle: U16,
    /// Reason code.
    reason: u8,
}

// --- controller + link -----------------------------------------------------

/// A live connection as seen by one controller: the shared handle and the index
/// of the peer controller within the [`Link`].
#[derive(Clone, Copy)]
struct Connection {
    handle: u16,
    peer: usize,
    /// Connection interval in 1.25 ms units, as last agreed. Carried so that
    /// LE Connection Update's completion event reports what the *connection*
    /// now is rather than echoing what the host asked for.
    interval: u16,
    /// Peripheral latency, in connection events.
    latency: u16,
    /// Supervision timeout in 10 ms units.
    timeout: u16,
    /// Central-to-peripheral PHY (1 = LE 1M, 2 = LE 2M, 3 = LE Coded).
    tx_phy: u8,
    /// Peripheral-to-central PHY.
    rx_phy: u8,
    /// Whether this link has a link key both ends agree on — either freshly
    /// paired or recognised from a host's store. Encryption is refused
    /// without it, which is the whole point of tracking it.
    authenticated: bool,
    /// Whether the link is encrypted. Modelled as *state*, not as
    /// cryptography: nothing on this link is actually enciphered, and a
    /// profile that requires encryption asks this rather than measuring it.
    encrypted: bool,
    /// The LTK an LE Enable Encryption named, held until the peripheral's
    /// host answers its LE Long Term Key Request. Lives on the *central's*
    /// connection, because the central is the side that was told the key.
    pending_ltk: Option<[u8; 16]>,
    /// How many times Change Connection Link Key has rolled this link's key.
    /// Part of the derivation, so a second rotation is a different key.
    key_rotations: u8,
}

impl Connection {
    /// A new connection at the parameters [`le_connection_complete`] reports
    /// at establishment: 30 ms interval, no latency, 420 ms supervision
    /// timeout, LE 1M both ways. A real controller negotiates these; this one
    /// states them, and LE Connection Update / LE Set PHY move them.
    fn new(handle: u16, peer: usize) -> Self {
        Self {
            handle,
            peer,
            interval: DEFAULT_CONN_INTERVAL,
            latency: 0,
            timeout: DEFAULT_SUPERVISION_TIMEOUT,
            tx_phy: le_phy::LE_1M,
            rx_phy: le_phy::LE_1M,
            authenticated: false,
            encrypted: false,
            pending_ltk: None,
            key_rotations: 0,
        }
    }
}

/// An isochronous stream (CIS) one host has asked for on top of an existing
/// ACL connection.
///
/// What is modelled is the *handshake*: LE Create CIS on the central, LE CIS
/// Request to the peripheral's host, LE Accept CIS Request back, LE CIS
/// Established at both ends. What is not modelled is the isochronous link
/// itself — there is no CIG scheduling, no ISO interval, no flush timeout,
/// and ISO SDUs are still routed over the ACL handle by [`Action::Iso`]. The
/// handshake is what a host's state machine blocks on; the scheduling is what
/// a real radio would do with the time in between.
#[derive(Clone, Copy)]
struct CisLink {
    /// The stream's own connection handle, chosen by the central's host.
    cis_handle: u16,
    /// The ACL connection the stream was set up over.
    acl_handle: u16,
    /// Whether LE CIS Established has been sent for it.
    established: bool,
}

/// A Channel Sounding configuration a host has created on one connection, and
/// whether it is currently producing measurements.
#[derive(Clone, Copy)]
struct CsSession {
    /// The connection the procedure runs on.
    handle: u16,
    /// The configuration identifier the host chose.
    config_id: u8,
    /// 0x00 initiator, 0x01 reflector. Only the initiator's session drives
    /// the measurement; the reflector's exists so its host is told about the
    /// tones it received.
    role: u8,
    /// Whether LE CS Procedure Enable has been accepted.
    enabled: bool,
    /// Increments once per completed procedure, and is how a host pairs its
    /// own subevent results with the ones the peer sends over the Ranging
    /// Service.
    procedure_counter: u16,
}

/// One extended advertising set, with the periodic train that may ride on it.
///
/// Extended and periodic advertising are modelled only as *carriage*: whatever
/// bytes the host set are handed to a scanner or a synchronized receiver
/// verbatim, once per [`Link::tick`]. Nothing about the secondary channel, the
/// AUX chain, the periodic interval or the advertising PHY is simulated, so a
/// train here is always found immediately and never drifts.
#[derive(Default, Clone)]
struct ExtAdvSet {
    /// Identifier the host gave this set.
    advertising_handle: u8,
    /// Advertising SID, which is half of what a receiver names a periodic
    /// train by (the advertiser's address is the other half).
    advertising_sid: u8,
    /// Extended advertising data as the host last set it.
    data: Vec<u8>,
    /// Whether LE Set Extended Advertising Enable turned this set on.
    enabled: bool,
    /// Periodic advertising data as the host last set it — for an Auracast
    /// source, the BASE.
    periodic_data: Vec<u8>,
    /// Whether LE Set Periodic Advertising Enable turned the train on.
    periodic_enabled: bool,
}

/// A Broadcast Isochronous Group this controller transmits.
///
/// The fields are exactly what the host wrote in LE Create BIG, kept so the
/// BIGInfo delivered to receivers can be *derived* from the source's own
/// parameters rather than invented. That derivation is the point: it is what
/// makes a broadcaster and a receiver in one process disagree if the
/// broadcaster fills LE Create BIG in wrongly.
#[derive(Clone)]
struct BigSource {
    /// Identifier the host gave the BIG.
    big_handle: u8,
    /// The advertising set whose periodic train carries the BIGInfo.
    advertising_handle: u8,
    /// One connection handle per BIS, BIS index 1 first.
    bis_handles: Vec<u16>,
    /// Whether the host asked for encrypted streams.
    encryption: u8,
    /// The broadcast code the host supplied.
    broadcast_code: [u8; 16],
    /// SDU interval, in microseconds.
    sdu_interval_us: u32,
    /// Largest SDU, in octets.
    max_sdu: u16,
    /// PHY bitfield.
    phy: u8,
    /// Framing (0 unframed, 1 framed).
    framing: u8,
}

/// A periodic advertising train this controller is synchronized to.
#[derive(Clone, Copy)]
struct PaSync {
    /// Handle the controller assigned to this synchronization.
    sync_handle: u16,
    /// Index of the controller whose train it is.
    source: usize,
    /// Which of that controller's advertising sets.
    advertising_handle: u8,
}

/// A BIG this controller has joined as a receiver.
#[derive(Clone)]
struct BigSink {
    /// Identifier the host gave the BIG it joined.
    big_handle: u8,
    /// Index of the controller transmitting it.
    source: usize,
    /// The 1-based BIS indices the host asked for, in the order it asked.
    indices: Vec<u8>,
    /// The local connection handles they were given, in the same order.
    bis_handles: Vec<u16>,
}

/// An unresolved LE Periodic Advertising Create Sync: whom the host is looking
/// for, kept until an advertiser matching it has its train on air.
#[derive(Clone, Copy)]
struct PendingPaSync {
    /// Advertiser address named in the command.
    address: Address,
    /// Advertising SID named in the command.
    advertising_sid: u8,
}

/// An inquiry a host started with HCI Inquiry.
struct Inquiry {
    /// Ticks left before Inquiry Complete goes out.
    ticks_remaining: u32,
    /// Whether the Inquiry Result events have already been delivered.
    ///
    /// A real controller reports the same device over and over for as long as
    /// the inquiry runs, and every host dedupes. Reporting each discoverable
    /// device exactly once instead keeps a scene's assertions stable and
    /// spares every future scripted device a dedupe table it would only need
    /// because of the simulator.
    results_sent: bool,
}

/// A page this controller is running: we sent Create Connection and are
/// waiting for the target's host to answer.
struct Page {
    /// Who we are paging.
    target: Address,
    /// Ticks left before the initiator's host is told Page Timeout.
    ticks_remaining: u32,
    /// Once the page has reached a connectable target: that controller's
    /// index, and our (central) end of the LMP link with it.
    reached: Option<(usize, LmpLink)>,
}

/// A synchronous (SCO/eSCO) connection, as one controller sees it.
///
/// It has a handle of its own — distinct from the ACL handle it was set up
/// over — and that is the whole point: SCO data is addressed to *this*
/// handle, and a host that sends call audio on the ACL handle is heard by
/// nobody. It cannot outlive its ACL.
#[derive(Clone, Copy)]
struct ScoLink {
    /// The synchronous link's own connection handle, which every HCI SCO
    /// data packet on it is addressed to.
    sco_handle: u16,
    /// The ACL connection it was set up over, and dies with.
    acl_handle: u16,
    /// The controller at the far end.
    peer: usize,
}

/// A synchronous connection a peer's host has asked for, raised to this
/// host as a Connection Request whose link type is SCO or eSCO, and whose
/// Accept/Reject Synchronous Connection Request this controller is waiting
/// for.
///
/// Kept separate from [`InboundPage`] rather than folded into it: the two
/// arrive as the same event code but are answered with *different commands*,
/// and a host that answers a synchronous request with plain Accept
/// Connection Request gets nothing back — which is the bug this separation
/// exists to make impossible to model away.
struct InboundSco {
    /// The controller that asked.
    initiator: usize,
    /// Its address, which the host's answer must name.
    initiator_address: Address,
    /// The ACL connection the synchronous link hangs off.
    acl_handle: u16,
    /// SCO or eSCO, as the initiator's packet types implied.
    link_type: u8,
    /// The air mode the initiator proposed. A real pair negotiates this over
    /// LMP; here the initiator's proposal stands, and both ends are told the
    /// same value — agreement being the property a host's state machine
    /// actually depends on.
    air_mode: u8,
}

/// A page *from* a peer that this controller has raised to its host, and
/// whose Accept/Reject it is waiting for.
struct InboundPage {
    /// The paging device.
    initiator: Address,
    /// Our (peripheral) end of the LMP link, deferring to the host.
    lmp: LmpLink,
}

/// The BR/EDR half of one simulated controller.
///
/// Kept in its own struct rather than spread across [`SimController`]: the LE
/// fields there already run to twenty, and every field here is meaningless
/// without the others.
struct ClassicState {
    /// Inquiry/page scan bits, as Write Scan Enable last set them. Zero at
    /// power-on, so a host that never writes it is invisible.
    scan_enable: u8,
    /// The name Remote Name Request answers with, as Write Local Name set it.
    local_name: [u8; 248],
    /// Class of Device reported in Inquiry Result and Connection Request.
    class_of_device: [u8; 3],
    /// The inquiry this host is running, if any.
    inquiry: Option<Inquiry>,
    /// The page this host is running, if any.
    page: Option<Page>,
    /// A page from a peer awaiting this host's decision.
    inbound_page: Option<InboundPage>,
    /// A synchronous connection a peer asked for, awaiting this host's
    /// Accept/Reject Synchronous Connection Request.
    inbound_sco: Option<InboundSco>,
    /// Remote Name Requests to answer on the next tick, oldest first.
    remote_name_requests: Vec<Address>,
    /// Simple Pairing Mode, as Write Simple Pairing Mode last set it. Zero at
    /// power-on: a host that never enables it gets Pairing Not Allowed rather
    /// than SSP it did not ask for, because legacy PIN pairing is the thing
    /// it would have got on real hardware and that is not modelled here.
    simple_pairing_mode: u8,
    /// The Key_Flag of the last Link Key Selection: 0x00 semi-permanent,
    /// 0x01 temporary.
    link_key_flag: u8,
}

impl Default for ClassicState {
    fn default() -> Self {
        Self {
            scan_enable: 0x00,
            local_name: [0u8; 248],
            class_of_device: [0u8; 3],
            inquiry: None,
            page: None,
            inbound_page: None,
            inbound_sco: None,
            remote_name_requests: Vec::new(),
            simple_pairing_mode: 0x00,
            link_key_flag: 0x00,
        }
    }
}

impl ClassicState {
    /// Whether this device answers inquiries.
    fn is_discoverable(&self) -> bool {
        self.scan_enable & scan_enable::INQUIRY != 0
    }

    /// Whether this device answers pages.
    fn is_connectable(&self) -> bool {
        self.scan_enable & scan_enable::PAGE != 0
    }
}

/// One device's simulated controller: it owns the controller side of an
/// [`HciChannel`], tracks the minimal advertising/scanning/connection state a
/// host drives over HCI, and buffers the events it will hand back.
struct SimController {
    address: Address,
    channel: Arc<HciChannel>,
    advertising: bool,
    adv_data: Vec<u8>,
    adv_event_type: u8,
    own_adv_addr_type: u8,
    scanning: bool,
    pending_connect: Option<Address>,
    connections: Vec<Connection>,
    /// Where this device stands on the simulated floor plan, in metres. This
    /// is the ground truth every RSSI and every Channel Sounding tone is
    /// derived from, and no host ever sees it.
    position: Position,
    /// The Channel Sounding configuration on this device, if a host created
    /// one.
    cs: Option<CsSession>,
    /// Extended advertising sets, by the handle the host gave them.
    ext_adv_sets: Vec<ExtAdvSet>,
    /// Whether LE Set Extended Scan Enable turned extended scanning on.
    ext_scanning: bool,
    /// An LE Periodic Advertising Create Sync still looking for its train.
    pending_pa_sync: Option<PendingPaSync>,
    /// Periodic advertising trains this controller is synchronized to.
    pa_syncs: Vec<PaSync>,
    /// The BIG this controller transmits, if a host created one.
    big: Option<BigSource>,
    /// BIGs this controller has joined as a receiver.
    big_sinks: Vec<BigSink>,
    /// Isochronous streams asked for or carried on this controller's links.
    cis_links: Vec<CisLink>,
    /// Synchronous (SCO/eSCO) links carried on this controller's ACLs — the
    /// BR/EDR counterpart of `cis_links`, and where call audio is addressed.
    sco_links: Vec<ScoLink>,
    /// Everything BR/EDR: scan enable, inquiry, paging, name, Class of Device.
    classic: ClassicState,
    /// H4 packets to deliver to this device's host at the end of the tick.
    outbox: VecDeque<Vec<u8>>,
}

impl SimController {
    fn new(address: Address, channel: Arc<HciChannel>) -> Self {
        Self {
            address,
            channel,
            advertising: false,
            adv_data: Vec::new(),
            adv_event_type: 0x00,
            own_adv_addr_type: 0x00,
            scanning: false,
            pending_connect: None,
            connections: Vec::new(),
            position: Position::default(),
            cs: None,
            ext_adv_sets: Vec::new(),
            ext_scanning: false,
            pending_pa_sync: None,
            pa_syncs: Vec::new(),
            big: None,
            big_sinks: Vec::new(),
            cis_links: Vec::new(),
            sco_links: Vec::new(),
            classic: ClassicState::default(),
            outbox: VecDeque::new(),
        }
    }

    /// The advertising set with `handle`, creating an empty one if the host
    /// has not touched it yet — the order the parameter/data/enable commands
    /// arrive in is the host's business, not the controller's.
    fn ext_adv_set(&mut self, handle: u8) -> &mut ExtAdvSet {
        if let Some(index) = self
            .ext_adv_sets
            .iter()
            .position(|s| s.advertising_handle == handle)
        {
            return &mut self.ext_adv_sets[index];
        }
        self.ext_adv_sets.push(ExtAdvSet {
            advertising_handle: handle,
            ..Default::default()
        });
        self.ext_adv_sets.last_mut().expect("just pushed")
    }

    /// Whether `handle` names one of this controller's live connections.
    fn is_connected(&self, handle: u16) -> bool {
        self.connections.iter().any(|c| c.handle == handle)
    }

    /// Reset to power-on defaults (HCI Reset).
    ///
    /// Position survives: it is a property of the simulated room, not of the
    /// controller's register file, and a host that resets its chip does not
    /// teleport.
    fn reset(&mut self) {
        self.advertising = false;
        self.adv_data.clear();
        self.scanning = false;
        self.pending_connect = None;
        self.connections.clear();
        self.cs = None;
        self.ext_adv_sets.clear();
        self.ext_scanning = false;
        self.pending_pa_sync = None;
        self.pa_syncs.clear();
        self.big = None;
        self.big_sinks.clear();
        self.cis_links.clear();
        self.sco_links.clear();
        // Scan Enable returns to 0x00 on Reset, which is why every BR/EDR
        // bring-up sequence writes it *after* the Reset rather than before.
        self.classic = ClassicState::default();
    }
}

/// Cross-controller effects collected while handling one controller's commands,
/// applied in a later phase that can touch two controllers at once.
enum Action {
    Disconnect {
        from: usize,
        handle: u16,
    },
    Acl {
        from: usize,
        handle: u16,
        data: Vec<u8>,
    },
    /// An isochronous SDU (LE Audio media plane), routed like ACL data on
    /// the same connection handle — Simble carries ISO over the established
    /// connection rather than modeling CIG/CIS setup.
    Iso {
        from: usize,
        handle: u16,
        data: Vec<u8>,
    },
    /// LE CS Create Config with Create_Context = 0x01, which the spec defines
    /// as "write the configuration in both the local and the remote
    /// controller" (Vol 4, Part E, Section 7.8.137). The peer's host is told
    /// with its own LE CS Config Complete, and gets the mirrored role — this
    /// is how a reflector learns it is in a procedure at all.
    CsConfigure {
        from: usize,
        handle: u16,
        config_id: u8,
        role: u8,
        propagate_to_peer: bool,
    },
    /// LE CS Procedure Enable. Enabling on the initiator enables the
    /// reflector's matching configuration too, since a procedure has two
    /// ends by definition.
    CsEnable {
        from: usize,
        handle: u16,
        config_id: u8,
        enable: bool,
    },
    /// LE Create BIG. Deferred because allocating the BIS connection handles
    /// needs the [`Link`]'s handle counter, not just the one controller.
    CreateBig {
        from: usize,
        /// The BIG as the host described it in LE Create BIG.
        source: Box<BigSource>,
        /// How many BISes to allocate handles for.
        num_bis: u8,
    },
    /// LE Terminate BIG. Touches every receiver synchronized to it, which is
    /// how they learn the source is gone.
    TerminateBig {
        from: usize,
        big_handle: u8,
    },
    /// The host answered an inbound page: HCI Accept Connection Request, or
    /// Reject Connection Request when `reject` carries a reason. Deferred
    /// because completing the page drives the *initiator's* LMP link too.
    ClassicAnswerPage {
        /// The controller whose host answered.
        from: usize,
        /// The address the host named — which must match the page it was
        /// told about, or the answer is for a page that never happened.
        peer: Address,
        /// `Some(reason)` to reject, `None` to accept.
        reject: Option<u8>,
    },
    // --- SCO / eSCO ------------------------------------------------------
    /// HCI Setup Synchronous Connection, or its Enhanced form: open a
    /// synchronous link over an existing ACL. Deferred because it raises a
    /// Connection Request at the *peer's* host, which is the only way that
    /// host learns audio is being opened to it.
    ScoSetup {
        from: usize,
        /// The ACL the synchronous link hangs off.
        acl_handle: u16,
        /// SCO or eSCO, as the requested packet types implied.
        link_type: u8,
        /// The air mode the initiator's Voice Setting or coding format asked
        /// for.
        air_mode: u8,
    },
    /// The host answered a synchronous Connection Request: Accept (or
    /// Enhanced Accept) Synchronous Connection Request, or Reject with a
    /// reason. Establishes — or refuses — the link at both ends at once.
    ScoAnswer {
        from: usize,
        /// The address the host named, which must match the request it was
        /// told about.
        peer: Address,
        /// `Some(reason)` to reject, `None` to accept.
        reject: Option<u8>,
    },
    /// An HCI synchronous data packet, routed to the far end of the SCO/eSCO
    /// link its handle names. This is the call audio.
    Sco {
        from: usize,
        handle: u16,
        data: Vec<u8>,
    },
    // --- end SCO / eSCO --------------------------------------------------
    /// LE Connection Update. A connection has two ends and one set of
    /// parameters, so both hosts get an LE Connection Update Complete — a
    /// peripheral that only ever heard about the update from its own host
    /// would be a fiction no real link produces.
    ConnectionUpdate {
        from: usize,
        handle: u16,
        interval: u16,
        latency: u16,
        timeout: u16,
    },
    /// LE Set PHY. Symmetric for the same reason: the PHY is a property of
    /// the link, and the peer's host is told with its own LE PHY Update
    /// Complete, with the directions swapped.
    PhyUpdate {
        from: usize,
        handle: u16,
        tx_phy: u8,
        rx_phy: u8,
    },
    /// LE Create CIS. The peer's host is told with an LE CIS Request, which
    /// is the only way it learns a stream is being opened to it.
    CisRequest {
        from: usize,
        acl_handle: u16,
        cis_handle: u16,
    },
    /// LE Accept CIS Request. Establishes the stream at both ends at once.
    CisAccept {
        from: usize,
        cis_handle: u16,
    },
    // --- security (BR/EDR SSP and LE encryption) ------------------------
    //
    // Every one of these is deferred for the same reason: a security step is
    // a sentence with two subjects. One host answers a question and the
    // *other* host is the one that has to be told, and that needs both
    // controllers at once.
    /// A step in a Secure Simple Pairing conversation, or the start of one.
    ClassicSecurity {
        /// The controller whose host acted.
        from: usize,
        step: SecurityStep,
    },
    /// LE Enable Encryption: ask the peripheral's host for the key.
    LeEnableEncryption {
        from: usize,
        handle: u16,
        ltk: [u8; 16],
        rand: [u8; 8],
        ediv: u16,
    },
    /// The peripheral's host answered its LE Long Term Key Request. `None`
    /// is the negative reply.
    LeLtkReply {
        from: usize,
        handle: u16,
        ltk: Option<[u8; 16]>,
    },
    /// LE BIG Create Sync. Needs the source controller's BIG to answer, and
    /// the handle counter to name the receiver's own BIS handles.
    BigCreateSync {
        from: usize,
        big_handle: u8,
        sync_handle: u16,
        encryption: u8,
        broadcast_code: [u8; 16],
        /// 1-based BIS indices the host asked to join.
        indices: Vec<u8>,
    },
}

/// One host's contribution to a security procedure, deferred so the peer's
/// controller can be reached.
enum SecurityStep {
    /// Authentication Requested: start (or restart) the whole conversation.
    Authenticate { handle: u16 },
    /// Link Key Request Reply, or — with `key: None` — its negative reply.
    LinkKeyReply {
        peer: Address,
        key: Option<[u8; 16]>,
    },
    /// IO Capability Request Reply, or — with `capabilities: None` — its
    /// negative reply carrying `reason`.
    IoCapabilityReply {
        peer: Address,
        capabilities: Option<IoCapabilities>,
        reason: u8,
    },
    /// User Confirmation Request Reply or Negative Reply.
    ConfirmationReply { peer: Address, accept: bool },
    /// User Passkey Request Reply (with the digits the user typed) or its
    /// negative reply.
    PasskeyReply { peer: Address, passkey: Option<u32> },
    /// Set Connection Encryption.
    Encrypt { handle: u16, enable: u8 },
    /// Change Connection Link Key.
    ChangeLinkKey { handle: u16 },
}

/// The shared medium. Holds every `SimController` on the "air" and, on each
/// [`tick`](Self::tick), drains their hosts' HCI, routes advertising and data
/// between them, and delivers the resulting events — the same role as Bumble's
/// `LocalLink`, sized for an in-process scene of any number of devices.
#[derive(Default)]
pub struct Link {
    controllers: Vec<SimController>,
    next_handle: u16,
    /// How the medium attenuates: one model for the whole scene, since it
    /// describes the *room* rather than any device.
    path_loss: PathLossModel,
    /// Shadowing and phase noise. Seeded, so a test that asserts on a noisy
    /// measurement gets the same noise every run.
    rng: Rng,
    /// Secure Simple Pairing conversations in flight. See [`ClassicPairing`]
    /// for why they are held here and not on a controller.
    pairings: Vec<ClassicPairing>,
}

impl Link {
    /// Creates an empty medium with no devices.
    pub fn new() -> Self {
        Self {
            controllers: Vec::new(),
            next_handle: 0x0001,
            path_loss: PathLossModel::default(),
            rng: Rng::default(),
            pairings: Vec::new(),
        }
    }

    /// The propagation model advertising reports and Channel Sounding tones
    /// are generated through.
    pub fn path_loss(&self) -> PathLossModel {
        self.path_loss
    }

    /// Replaces the propagation model — transmit power, reference loss,
    /// path-loss exponent, shadowing.
    pub fn set_path_loss(&mut self, model: PathLossModel) {
        self.path_loss = model;
    }

    /// Reseeds the medium's noise. Same seed, same shadowing and phase noise.
    pub fn set_noise_seed(&mut self, seed: u64) {
        self.rng = Rng::new(seed);
    }

    /// Moves the device at `address` to `position`. Returns false if no such
    /// device is on the medium.
    pub fn set_position(&mut self, address: Address, position: Position) -> bool {
        match self.controllers.iter_mut().find(|c| c.address == address) {
            Some(controller) => {
                controller.position = position;
                true
            }
            None => false,
        }
    }

    /// Where the device at `address` stands.
    pub fn position(&self, address: Address) -> Option<Position> {
        self.controllers
            .iter()
            .find(|c| c.address == address)
            .map(|c| c.position)
    }

    /// The true separation of two devices in metres — the ground truth a
    /// ranging demo compares its estimate against, and which no host on the
    /// medium can see.
    pub fn distance_between(&self, a: Address, b: Address) -> Option<f64> {
        Some(self.position(a)?.distance_to(self.position(b)?))
    }

    /// Adds a device with `address` and returns the host side of its HCI
    /// channel: send commands and ACL to it, and poll it for events, exactly as
    /// if it were a real controller. The returned handle and the [`Link`] share
    /// the channel; [`tick`](Self::tick) services it.
    pub fn add_device(&mut self, address: Address) -> Arc<HciChannel> {
        let channel = Arc::new(HciChannel::new());
        self.controllers
            .push(SimController::new(address, Arc::clone(&channel)));
        channel
    }

    /// The number of devices on the medium.
    pub fn device_count(&self) -> usize {
        self.controllers.len()
    }

    fn alloc_handle(&mut self) -> u16 {
        let h = self.next_handle;
        self.next_handle = self.next_handle.wrapping_add(1).max(0x0001);
        h
    }

    /// Advances the simulation by one step: handle every host's queued HCI,
    /// route advertising to scanners and data between connected peers, and hand
    /// the resulting events back to each host. Non-blocking; call it in a loop
    /// or on a timer to keep the scene live.
    pub fn tick(&mut self) {
        // Phase A: drain and handle each host's queued commands / ACL.
        let mut actions: Vec<Action> = Vec::new();
        for i in 0..self.controllers.len() {
            while let Some(pkt) = self.controllers[i].channel.poll_host_packet() {
                self.handle_packet(i, &pkt, &mut actions);
            }
        }

        // Phase B: deliver an advertising report from each advertiser to every
        // other scanning device, stamped with the RSSI that advertiser's
        // signal actually arrives with at that scanner's position.
        let advertisers: Vec<(Address, u8, u8, Vec<u8>, Position)> = self
            .controllers
            .iter()
            .filter(|c| c.advertising)
            .map(|c| {
                (
                    c.address,
                    c.adv_event_type,
                    c.own_adv_addr_type,
                    c.adv_data.clone(),
                    c.position,
                )
            })
            .collect();
        let path_loss = self.path_loss;
        for i in 0..self.controllers.len() {
            if !self.controllers[i].scanning {
                continue;
            }
            let scanner_position = self.controllers[i].position;
            let scanner_address = self.controllers[i].address;
            for (addr, event_type, addr_type, data, advertiser_position) in &advertisers {
                if *addr == scanner_address {
                    continue; // a device never hears its own advertisement
                }
                // Shadowing is redrawn per report, which is exactly why an
                // RSSI reading jitters while the devices sit still.
                let distance = advertiser_position.distance_to(scanner_position);
                let shadowing = self.rng.normal_scaled(path_loss.shadowing_sigma_db);
                let rssi = quantize_rssi(path_loss.rssi_dbm(distance, shadowing));
                self.controllers[i].outbox.push_back(le_advertising_report(
                    *event_type,
                    *addr_type,
                    *addr,
                    data,
                    rssi,
                ));
            }
        }

        // Phase B2: extended advertising reports. Legacy reports above cannot
        // carry an Advertising SID, so this is the only path by which a
        // broadcast receiver can find a source's periodic train.
        self.tick_extended_advertising();

        // Phase C: pending connections — a scanner that asked to connect to an
        // advertiser's address is joined to it once that advertiser is on air.
        for i in 0..self.controllers.len() {
            if let Some(target) = self.controllers[i].pending_connect
                && let Some(a) = self
                    .controllers
                    .iter()
                    .position(|c| c.address == target && c.advertising)
            {
                self.establish_connection(i, a);
                self.controllers[i].pending_connect = None;
            }
        }

        // Phase C2: resolve any outstanding LE Periodic Advertising Create
        // Sync, the same "keep looking until the target is on air" shape as
        // the pending connections above.
        self.tick_periodic_sync_requests();

        // Phase C3: BR/EDR — run inquiries, advance pages, answer Remote Name
        // Requests. This sits beside the LE phases rather than inside them:
        // the two transports share the medium, the handle space and the ACL
        // router, and nothing else.
        self.tick_classic();

        // Phase D: apply disconnects and ACL routing (touch two controllers).
        for action in actions {
            match action {
                Action::Disconnect { from, handle } => self.route_disconnect(from, handle),
                Action::Acl { from, handle, data } => self.route_acl(from, handle, &data),
                Action::Iso { from, handle, data } => self.route_iso(from, handle, &data),
                Action::CsConfigure {
                    from,
                    handle,
                    config_id,
                    role,
                    propagate_to_peer,
                } => self.route_cs_configure(from, handle, config_id, role, propagate_to_peer),
                Action::CsEnable {
                    from,
                    handle,
                    config_id,
                    enable,
                } => self.route_cs_enable(from, handle, config_id, enable),
                Action::CreateBig {
                    from,
                    source,
                    num_bis,
                } => self.route_create_big(from, *source, num_bis),
                Action::TerminateBig { from, big_handle } => {
                    self.route_terminate_big(from, big_handle)
                }
                Action::ClassicAnswerPage { from, peer, reject } => {
                    self.route_classic_answer_page(from, peer, reject)
                }
                Action::ClassicSecurity { from, step } => self.route_classic_security(from, step),
                Action::LeEnableEncryption {
                    from,
                    handle,
                    ltk,
                    rand,
                    ediv,
                } => self.route_le_enable_encryption(from, handle, ltk, rand, ediv),
                Action::LeLtkReply { from, handle, ltk } => {
                    self.route_le_ltk_reply(from, handle, ltk)
                }
                Action::BigCreateSync {
                    from,
                    big_handle,
                    sync_handle,
                    encryption,
                    broadcast_code,
                    indices,
                } => self.route_big_create_sync(
                    from,
                    big_handle,
                    sync_handle,
                    encryption,
                    broadcast_code,
                    &indices,
                ),
                Action::ConnectionUpdate {
                    from,
                    handle,
                    interval,
                    latency,
                    timeout,
                } => self.route_connection_update(from, handle, interval, latency, timeout),
                Action::PhyUpdate {
                    from,
                    handle,
                    tx_phy,
                    rx_phy,
                } => self.route_phy_update(from, handle, tx_phy, rx_phy),
                Action::CisRequest {
                    from,
                    acl_handle,
                    cis_handle,
                } => self.route_cis_request(from, acl_handle, cis_handle),
                Action::CisAccept { from, cis_handle } => self.route_cis_accept(from, cis_handle),
                // --- SCO / eSCO ---
                Action::ScoSetup {
                    from,
                    acl_handle,
                    link_type,
                    air_mode,
                } => self.route_sco_setup(from, acl_handle, link_type, air_mode),
                Action::ScoAnswer { from, peer, reject } => {
                    self.route_sco_answer(from, peer, reject)
                }
                Action::Sco { from, handle, data } => self.route_sco(from, handle, &data),
                // --- end SCO / eSCO ---
            }
        }

        // Phase D2: run one Channel Sounding procedure per enabled session.
        self.tick_channel_sounding();

        // Phase D3: one periodic advertising report — and, where a BIG hangs
        // off the train, one BIGInfo report — to every synchronized receiver.
        self.tick_periodic_advertising();

        // Phase E: flush every outbox to its host.
        for c in &mut self.controllers {
            while let Some(pkt) = c.outbox.pop_front() {
                let _ = c.channel.receive_from_controller(pkt);
            }
        }
    }

    /// Handle one H4 packet a host sent to controller `i`.
    fn handle_packet(&mut self, i: usize, pkt: &[u8], actions: &mut Vec<Action>) {
        match pkt.first().copied() {
            Some(h4_type::HCI_COMMAND) => {
                if let Ok((hdr, params)) = Ref::<_, CommandHeader>::from_prefix(&pkt[1..]) {
                    self.handle_command(i, hdr.opcode.get(), params, actions);
                }
            }
            Some(h4_type::HCI_ACL_DATA) => {
                if let Ok((hdr, _)) = Ref::<_, AclHeader>::from_prefix(&pkt[1..]) {
                    actions.push(Action::Acl {
                        from: i,
                        handle: hdr.handle_and_flags.get() & 0x0FFF,
                        data: pkt[1..].to_vec(), // handle+flags+len+payload, forwarded verbatim
                    });
                }
            }
            Some(h4_type::HCI_SCO_DATA) => {
                // Call audio. Its handle is the *synchronous* link's, not the
                // ACL's — a packet addressed to the ACL handle finds no SCO
                // link and is dropped, which is what real hardware does with
                // it too.
                if let Ok((hdr, _)) = Ref::<_, ScoHeader>::from_prefix(&pkt[1..]) {
                    actions.push(Action::Sco {
                        from: i,
                        handle: hdr.handle_and_flags.get() & 0x0FFF,
                        data: pkt[1..].to_vec(), // handle+flags+len+payload, verbatim
                    });
                }
            }
            Some(h4_type::HCI_ISO_DATA) => {
                if let Ok((hdr, _)) = Ref::<_, AclHeader>::from_prefix(&pkt[1..]) {
                    // An ISO header shares the ACL header's shape (handle +
                    // flags, then a length), so the same view reads it.
                    actions.push(Action::Iso {
                        from: i,
                        handle: hdr.handle_and_flags.get() & 0x0FFF,
                        data: pkt[1..].to_vec(),
                    });
                }
            }
            _ => {}
        }
    }

    /// Handle one parsed HCI command from controller `i`'s host.
    fn handle_command(&mut self, i: usize, opcode: u16, params: &[u8], actions: &mut Vec<Action>) {
        // Read RSSI needs the *peer's* position, so it is answered before the
        // borrow of this one controller that the rest of the match works
        // under.
        if opcode == opcode::READ_RSSI {
            self.handle_read_rssi(i, le_u16(params, 0));
            return;
        }
        // Security — BR/EDR pairing and encryption, and LE encryption start —
        // is dispatched first and kept in one function of its own. It spans
        // both transports, so it does not belong under either heading below.
        if self.handle_security_command(i, opcode, params, actions) {
            return;
        }
        // BR/EDR is dispatched first and separately: its commands are mostly
        // answered with a Command *Status* plus a later completion event,
        // which is the opposite convention to the LE commands below.
        if self.handle_classic_command(i, opcode, params, actions) {
            return;
        }
        let c = &mut self.controllers[i];
        match opcode {
            opcode::RESET => {
                c.reset();
                c.outbox
                    .push_back(command_complete(opcode, &[STATUS_SUCCESS]));
            }
            opcode::READ_BD_ADDR => {
                let mut ret = vec![STATUS_SUCCESS];
                ret.extend_from_slice(&addr_le(c.address));
                c.outbox.push_back(command_complete(opcode, &ret));
            }
            opcode::LE_SET_ADV_PARAMS => {
                // interval_min(2) interval_max(2) adv_type(1) own_addr_type(1) …
                if params.len() >= 6 {
                    c.adv_event_type = params[4];
                    c.own_adv_addr_type = params[5];
                }
                c.outbox
                    .push_back(command_complete(opcode, &[STATUS_SUCCESS]));
            }
            opcode::LE_SET_ADV_DATA => {
                // length(1) data(31)
                if let Some(&len) = params.first() {
                    let len = (len as usize).min(params.len().saturating_sub(1));
                    c.adv_data = params[1..1 + len].to_vec();
                }
                c.outbox
                    .push_back(command_complete(opcode, &[STATUS_SUCCESS]));
            }
            opcode::LE_SET_ADV_ENABLE => {
                c.advertising = params.first().copied() == Some(0x01);
                c.outbox
                    .push_back(command_complete(opcode, &[STATUS_SUCCESS]));
            }
            opcode::LE_SET_SCAN_ENABLE => {
                c.scanning = params.first().copied() == Some(0x01);
                c.outbox
                    .push_back(command_complete(opcode, &[STATUS_SUCCESS]));
            }
            opcode::LE_CREATE_CONNECTION => {
                if let Ok((prefix, _)) = Ref::<_, LeCreateConnectionPrefix>::from_prefix(params) {
                    let mut be = prefix.peer_address;
                    be.reverse(); // wire is little-endian; Address is big-endian
                    c.pending_connect = Some(Address::from_be_bytes(be));
                }
                c.outbox.push_back(command_status(STATUS_SUCCESS, opcode));
            }
            opcode::LE_CREATE_CONNECTION_CANCEL => {
                c.pending_connect = None;
                c.outbox
                    .push_back(command_complete(opcode, &[STATUS_SUCCESS]));
            }
            opcode::LE_CONNECTION_UPDATE => {
                // handle(2) interval_min(2) interval_max(2) latency(2)
                // supervision_timeout(2) min_ce(2) max_ce(2).
                //
                // Vol 4, Part E, Section 7.8.18: Command Status, then an LE
                // Connection Update Complete when the *link layer* has
                // changed the parameters. The Complete carries the
                // connection's new values, not the host's request, so the
                // controller has to pick something inside the requested
                // range and remember it. This one takes the maximum interval,
                // which is what a controller with nothing else scheduled
                // would settle on.
                //
                // Not modelled: the LL_CONNECTION_UPDATE_IND procedure and
                // its instant, so the change is immediate rather than taking
                // effect six connection events later; and a peripheral's
                // right to reject via LL_REJECT_EXT_IND, so an update here is
                // never refused by the peer.
                let handle = le_u16(params, 0);
                if !c.is_connected(handle) {
                    c.outbox
                        .push_back(command_status(STATUS_UNKNOWN_CONNECTION, opcode));
                } else if params.len() < 14 {
                    c.outbox
                        .push_back(command_status(STATUS_INVALID_PARAMETERS, opcode));
                } else {
                    c.outbox.push_back(command_status(STATUS_SUCCESS, opcode));
                    actions.push(Action::ConnectionUpdate {
                        from: i,
                        handle,
                        interval: le_u16(params, 4),
                        latency: le_u16(params, 6),
                        timeout: le_u16(params, 8),
                    });
                }
            }
            opcode::LE_SET_PHY => {
                // handle(2) all_phys(1) tx_phys(1) rx_phys(1) phy_options(2).
                //
                // Vol 4, Part E, Section 7.8.49: Command Status, then an LE
                // PHY Update Complete — which the spec says is sent even when
                // nothing changed, so a host that asks for the PHY it already
                // has is still unblocked. Bit 0 of All_PHYs means "the host
                // has no preference for TX", bit 1 the same for RX; in that
                // case the current PHY stays.
                //
                // Not modelled: the LL_PHY_REQ/LL_PHY_RSP exchange, PHY
                // Options (S=2/S=8 coding), and any reason a controller might
                // decline — the best PHY the host allows is simply taken.
                let handle = le_u16(params, 0);
                if !c.is_connected(handle) {
                    c.outbox
                        .push_back(command_status(STATUS_UNKNOWN_CONNECTION, opcode));
                } else if params.len() < 7 {
                    c.outbox
                        .push_back(command_status(STATUS_INVALID_PARAMETERS, opcode));
                } else {
                    let all_phys = params[2];
                    let current = c
                        .connections
                        .iter()
                        .find(|conn| conn.handle == handle)
                        .map(|conn| (conn.tx_phy, conn.rx_phy))
                        .unwrap_or((le_phy::LE_1M, le_phy::LE_1M));
                    let tx_phy = if all_phys & 0x01 != 0 {
                        current.0
                    } else {
                        preferred_phy(params[3], current.0)
                    };
                    let rx_phy = if all_phys & 0x02 != 0 {
                        current.1
                    } else {
                        preferred_phy(params[4], current.1)
                    };
                    c.outbox.push_back(command_status(STATUS_SUCCESS, opcode));
                    actions.push(Action::PhyUpdate {
                        from: i,
                        handle,
                        tx_phy,
                        rx_phy,
                    });
                }
            }
            opcode::LE_CREATE_CIS => {
                // cis_count(1) then (cis_handle(2), acl_handle(2)) per stream.
                //
                // Vol 4, Part E, Section 7.8.99: Command Status, and then one
                // LE CIS Established per stream — but only after the
                // peripheral's host has answered the LE CIS Request this
                // sends it. A host that got a Command Complete here would sit
                // on a stream that never establishes and never fails.
                let count = params.first().copied().unwrap_or(0) as usize;
                let pairs: Vec<(u16, u16)> = (0..count)
                    .filter_map(|n| {
                        let at = 1 + n * 4;
                        (params.len() >= at + 4)
                            .then(|| (le_u16(params, at), le_u16(params, at + 2)))
                    })
                    .collect();
                if pairs.len() != count || count == 0 {
                    c.outbox
                        .push_back(command_status(STATUS_INVALID_PARAMETERS, opcode));
                } else if pairs.iter().any(|(_, acl)| !c.is_connected(*acl)) {
                    // A stream is opened *on* an ACL link. Naming a handle
                    // that is not one is the same mistake as a CS command on
                    // a dead connection, and gets the same answer.
                    c.outbox
                        .push_back(command_status(STATUS_UNKNOWN_CONNECTION, opcode));
                } else {
                    c.outbox.push_back(command_status(STATUS_SUCCESS, opcode));
                    for (cis_handle, acl_handle) in pairs {
                        c.cis_links.push(CisLink {
                            cis_handle,
                            acl_handle,
                            established: false,
                        });
                        actions.push(Action::CisRequest {
                            from: i,
                            acl_handle,
                            cis_handle,
                        });
                    }
                }
            }
            opcode::LE_ACCEPT_CIS_REQUEST => {
                // connection_handle(2) — the CIS handle from LE CIS Request.
                //
                // Vol 4, Part E, Section 7.8.101: Command Status, then LE CIS
                // Established at both ends.
                let cis_handle = le_u16(params, 0);
                if c.cis_links.iter().any(|l| l.cis_handle == cis_handle) {
                    c.outbox.push_back(command_status(STATUS_SUCCESS, opcode));
                    actions.push(Action::CisAccept {
                        from: i,
                        cis_handle,
                    });
                } else {
                    c.outbox
                        .push_back(command_status(STATUS_UNKNOWN_CONNECTION, opcode));
                }
            }
            opcode::LE_CS_SECURITY_ENABLE => {
                // The CS security start procedure derives the keys that
                // randomize tone phases against an attacker. Simble models no
                // CS encryption, so this succeeds immediately — a host that
                // waits for the completion event gets one, and a host that
                // skips the step is not stopped, which is the difference that
                // matters for a demo. Vol 4, Part E, Section 7.8.133.
                //
                // A handle that names no connection is refused, though: every
                // Channel Sounding command runs *on* a connection, and a
                // controller that accepted one without would leave the host
                // waiting for a completion event that can never come.
                let handle = le_u16(params, 0);
                if !c.is_connected(handle) {
                    c.outbox
                        .push_back(command_status(STATUS_UNKNOWN_CONNECTION, opcode));
                    return;
                }
                c.outbox.push_back(command_status(STATUS_SUCCESS, opcode));
                let mut body = vec![event::LE_CS_SECURITY_ENABLE_COMPLETE, STATUS_SUCCESS];
                body.extend_from_slice(&handle.to_le_bytes());
                c.outbox.push_back(event_packet(event::LE_META, &body));
            }
            opcode::LE_CS_CREATE_CONFIG => {
                // connection_handle(2) config_id(1) create_context(1)
                // main_mode_type(1) sub_mode_type(1) … role at offset 10.
                if params.len() >= 11 {
                    let handle = le_u16(params, 0);
                    let config_id = params[2];
                    let propagate_to_peer = params[3] == 0x01;
                    let role = params[10];
                    if !c.is_connected(handle) {
                        c.outbox
                            .push_back(command_status(STATUS_UNKNOWN_CONNECTION, opcode));
                        return;
                    }
                    c.outbox.push_back(command_status(STATUS_SUCCESS, opcode));
                    actions.push(Action::CsConfigure {
                        from: i,
                        handle,
                        config_id,
                        role,
                        propagate_to_peer,
                    });
                } else {
                    c.outbox
                        .push_back(command_status(STATUS_INVALID_PARAMETERS, opcode));
                }
            }
            opcode::LE_CS_SET_PROCEDURE_PARAMETERS => {
                // Timing and tone-antenna choices Simble does not model: it
                // reports one subevent per tick regardless. Answered with the
                // Command Complete the spec specifies (status, then the
                // connection handle) so a host's sequencing still works.
                let handle = le_u16(params, 0);
                let mut ret = vec![STATUS_SUCCESS];
                ret.extend_from_slice(&handle.to_le_bytes());
                c.outbox.push_back(command_complete(opcode, &ret));
            }
            opcode::LE_CS_PROCEDURE_ENABLE => {
                if params.len() >= 4 {
                    let handle = le_u16(params, 0);
                    let config_id = params[2];
                    let enable = params[3] == 0x01;
                    c.outbox.push_back(command_status(STATUS_SUCCESS, opcode));
                    actions.push(Action::CsEnable {
                        from: i,
                        handle,
                        config_id,
                        enable,
                    });
                } else {
                    c.outbox
                        .push_back(command_status(STATUS_INVALID_PARAMETERS, opcode));
                }
            }
            opcode::LE_CS_REMOVE_CONFIG => {
                // Vol 4, Part E, 7.8.138: Command Status, and THEN an
                // LE CS Config Complete carrying action 0x00. Sending only the
                // status left a host that waits for the event hanging forever
                // -- the same shape as the Procedure Enable silence, and the
                // reason that one was found: a command whose declared answer
                // nobody sends.
                let removed = c.cs.take();
                c.outbox.push_back(command_status(STATUS_SUCCESS, opcode));
                if let Some(session) = removed {
                    c.outbox.push_back(cs_config_complete(
                        session.handle,
                        session.config_id,
                        session.role,
                        cs_action::REMOVED,
                    ));
                }
            }

            // --- extended and periodic advertising ---------------------------
            opcode::LE_SET_EXT_ADV_PARAMS => {
                // The SID is the last octet before scan_request_notification;
                // it and the advertising handle are the only fields that
                // change what a receiver can find.
                if let Ok(params) = LeSetExtendedAdvertisingParameters::ref_from_bytes(params) {
                    let sid = params.advertising_sid;
                    c.ext_adv_set(params.advertising_handle).advertising_sid = sid;
                }
                // Status, then the selected TX power (Vol 4, Part E, 7.8.53).
                c.outbox
                    .push_back(command_complete(opcode, &[STATUS_SUCCESS, 0x00]));
            }
            opcode::LE_SET_EXT_ADV_DATA => {
                if let Some((header, data)) = LeSetExtendedAdvertisingDataHeader::parse(params) {
                    let (handle, operation, data) =
                        (header.advertising_handle, header.operation, data.to_vec());
                    let set = c.ext_adv_set(handle);
                    // Only a complete-data write is modelled; a host that
                    // fragments would need the first/intermediate/last states.
                    if operation == data_operation::COMPLETE {
                        set.data = data;
                    }
                }
                c.outbox
                    .push_back(command_complete(opcode, &[STATUS_SUCCESS]));
            }
            opcode::LE_SET_EXT_ADV_ENABLE => {
                if let Some((header, entries)) = LeSetExtendedAdvertisingEnableHeader::parse(params)
                {
                    let enable = header.enable == 0x01;
                    let handles: Vec<u8> = entries.iter().map(|e| e.advertising_handle).collect();
                    for handle in handles {
                        c.ext_adv_set(handle).enabled = enable;
                    }
                }
                c.outbox
                    .push_back(command_complete(opcode, &[STATUS_SUCCESS]));
            }
            opcode::LE_SET_PERIODIC_ADV_DATA => {
                if let Some((header, data)) = LeSetPeriodicAdvertisingDataHeader::parse(params) {
                    let (handle, operation, data) =
                        (header.advertising_handle, header.operation, data.to_vec());
                    let set = c.ext_adv_set(handle);
                    if operation == data_operation::COMPLETE {
                        set.periodic_data = data;
                    }
                }
                c.outbox
                    .push_back(command_complete(opcode, &[STATUS_SUCCESS]));
            }
            opcode::LE_SET_PERIODIC_ADV_ENABLE => {
                if let Ok(enable) = LeSetPeriodicAdvertisingEnable::ref_from_bytes(params) {
                    let on = enable.enable & 0x01 == 0x01;
                    c.ext_adv_set(enable.advertising_handle).periodic_enabled = on;
                }
                c.outbox
                    .push_back(command_complete(opcode, &[STATUS_SUCCESS]));
            }
            opcode::LE_SET_EXT_SCAN_ENABLE => {
                if let Ok(enable) = LeSetExtendedScanEnable::ref_from_bytes(params) {
                    c.ext_scanning = enable.enable == 0x01;
                }
                c.outbox
                    .push_back(command_complete(opcode, &[STATUS_SUCCESS]));
            }
            opcode::LE_PERIODIC_ADV_CREATE_SYNC => {
                if let Ok(sync) = LePeriodicAdvertisingCreateSync::ref_from_bytes(params) {
                    let mut be = sync.advertiser_address;
                    be.reverse();
                    c.pending_pa_sync = Some(PendingPaSync {
                        address: Address::from_be_bytes(be),
                        advertising_sid: sync.advertising_sid,
                    });
                    c.outbox.push_back(command_status(STATUS_SUCCESS, opcode));
                } else {
                    c.outbox
                        .push_back(command_status(STATUS_INVALID_PARAMETERS, opcode));
                }
            }
            opcode::LE_PERIODIC_ADV_TERMINATE_SYNC => {
                let handle = le_u16(params, 0);
                c.pa_syncs.retain(|s| s.sync_handle != handle);
                c.outbox
                    .push_back(command_complete(opcode, &[STATUS_SUCCESS]));
            }

            // --- Broadcast Isochronous Groups --------------------------------
            opcode::LE_CREATE_BIG => {
                let Ok(create) = LeCreateBig::ref_from_bytes(params) else {
                    c.outbox
                        .push_back(command_status(STATUS_INVALID_PARAMETERS, opcode));
                    return;
                };
                // A BIG rides in the ACAD of a periodic train, so there has to
                // be one running on the named advertising set. This is the
                // refusal a host meets when it enables the BIG before the
                // train, and the reason the setup sequence has the order it
                // does.
                let train_running = c.ext_adv_sets.iter().any(|s| {
                    s.advertising_handle == create.advertising_handle
                        && s.enabled
                        && s.periodic_enabled
                });
                if !train_running || create.num_bis == 0 {
                    c.outbox
                        .push_back(command_status(STATUS_COMMAND_DISALLOWED, opcode));
                    return;
                }
                c.outbox.push_back(command_status(STATUS_SUCCESS, opcode));
                actions.push(Action::CreateBig {
                    from: i,
                    num_bis: create.num_bis,
                    source: Box::new(BigSource {
                        big_handle: create.big_handle,
                        advertising_handle: create.advertising_handle,
                        bis_handles: Vec::new(),
                        encryption: create.encryption,
                        broadcast_code: create.broadcast_code,
                        sdu_interval_us: create.sdu_interval.get(),
                        max_sdu: create.max_sdu.get(),
                        phy: create.phy,
                        framing: create.framing,
                    }),
                });
            }
            opcode::LE_TERMINATE_BIG => {
                let Ok(terminate) = LeTerminateBig::ref_from_bytes(params) else {
                    c.outbox
                        .push_back(command_status(STATUS_INVALID_PARAMETERS, opcode));
                    return;
                };
                if c.big.as_ref().map(|b| b.big_handle) != Some(terminate.big_handle) {
                    c.outbox
                        .push_back(command_status(STATUS_UNKNOWN_CONNECTION, opcode));
                    return;
                }
                c.outbox.push_back(command_status(STATUS_SUCCESS, opcode));
                actions.push(Action::TerminateBig {
                    from: i,
                    big_handle: terminate.big_handle,
                });
            }
            opcode::LE_BIG_CREATE_SYNC => {
                let Some((header, indices)) = LeBigCreateSyncHeader::parse(params) else {
                    c.outbox
                        .push_back(command_status(STATUS_INVALID_PARAMETERS, opcode));
                    return;
                };
                let sync_handle = header.sync_handle.get();
                // 0x42 is Unknown Advertising Identifier, which is what a
                // controller answers for a sync handle it never issued.
                if !c.pa_syncs.iter().any(|s| s.sync_handle == sync_handle) {
                    c.outbox
                        .push_back(command_status(STATUS_UNKNOWN_ADVERTISING_ID, opcode));
                    return;
                }
                let action = Action::BigCreateSync {
                    from: i,
                    big_handle: header.big_handle,
                    sync_handle,
                    encryption: header.encryption,
                    broadcast_code: header.broadcast_code,
                    indices: indices.to_vec(),
                };
                c.outbox.push_back(command_status(STATUS_SUCCESS, opcode));
                actions.push(action);
            }
            opcode::LE_BIG_TERMINATE_SYNC => {
                // Answered by Command Complete and by nothing else: leaving a
                // BIG raises no BIG Sync Lost, because nothing was lost. A
                // host that waits for one waits forever.
                let big_handle = params.first().copied().unwrap_or(0);
                let known = c.big_sinks.iter().any(|s| s.big_handle == big_handle);
                c.big_sinks.retain(|s| s.big_handle != big_handle);
                let status = if known {
                    STATUS_SUCCESS
                } else {
                    STATUS_UNKNOWN_CONNECTION
                };
                let response = LeBigTerminateSyncResponse { status, big_handle };
                c.outbox
                    .push_back(command_complete(opcode, response.as_bytes()));
            }
            opcode::DISCONNECT => {
                let handle = params
                    .get(0..2)
                    .map(|b| u16::from_le_bytes([b[0], b[1]]))
                    .unwrap_or(0);
                c.outbox.push_back(command_status(STATUS_SUCCESS, opcode));
                actions.push(Action::Disconnect { from: i, handle });
            }
            // Set Event Mask, LE Set Event Mask, scan/adv params, scan-response
            // data, and anything else: accept with a success Command Complete so
            // the host's bring-up never stalls on an unimplemented command.
            //
            // Except that "anything else" used to include the 61 commands the
            // spec answers with a Command *Status*, and for those a cheerful
            // Command Complete is not a harmless stub — it is the answer to a
            // question the host did not ask, and the event it *is* waiting for
            // never arrives. So the shape is looked up even where the
            // behaviour is not modelled: an unmodelled Command-Status command
            // gets a Command Status saying Unknown HCI Command, which a host
            // can act on. See [`COMMAND_STATUS_OPCODES`].
            _ if answered_by_command_status(opcode) => {
                c.outbox
                    .push_back(command_status(STATUS_UNKNOWN_HCI_COMMAND, opcode));
            }
            _ => {
                c.outbox
                    .push_back(command_complete(opcode, &[STATUS_SUCCESS]));
            }
        }
    }

    /// One tick of BR/EDR: inquiries, pages, Remote Name Requests.
    fn tick_classic(&mut self) {
        self.tick_inquiry();
        self.tick_paging();
        self.tick_remote_names();
        self.prune_pairings();
    }

    /// Deliver Inquiry Results to every inquiring host, then Inquiry Complete
    /// when the inquiry's window closes.
    ///
    /// A device is found only if its host enabled inquiry scan. That is the
    /// whole point of modelling Scan Enable: "the peripheral was never made
    /// discoverable" is the commonest BR/EDR bring-up bug there is, and a
    /// simulator that found everything regardless would never reproduce it.
    fn tick_inquiry(&mut self) {
        // One snapshot for the whole tick, so two devices inquiring at once
        // see the same scene.
        let discoverable: Vec<(Address, [u8; 3])> = self
            .controllers
            .iter()
            .filter(|c| c.classic.is_discoverable())
            .map(|c| (c.address, c.classic.class_of_device))
            .collect();

        for i in 0..self.controllers.len() {
            let Some(inquiry) = &self.controllers[i].classic.inquiry else {
                continue;
            };
            let send_results = !inquiry.results_sent;
            let remaining = inquiry.ticks_remaining.saturating_sub(1);
            let address = self.controllers[i].address;

            if send_results {
                let found: Vec<(Address, [u8; 3])> = discoverable
                    .iter()
                    .copied()
                    .filter(|(a, _)| *a != address) // never finds itself
                    .collect();
                if !found.is_empty() {
                    self.controllers[i].outbox.push_back(inquiry_result(&found));
                }
            }

            if remaining == 0 {
                // Results first, then the Complete that closes the window —
                // a host treats Inquiry Complete as "that is everything".
                self.controllers[i].classic.inquiry = None;
                self.controllers[i]
                    .outbox
                    .push_back(inquiry_complete(STATUS_SUCCESS));
            } else if let Some(inquiry) = self.controllers[i].classic.inquiry.as_mut() {
                inquiry.results_sent = true;
                inquiry.ticks_remaining = remaining;
            }
        }
    }

    /// Advance every page in flight: deliver a Connection Request to a
    /// connectable target's host, and time out a page nobody answers.
    fn tick_paging(&mut self) {
        for i in 0..self.controllers.len() {
            let Some(page) = &self.controllers[i].classic.page else {
                continue;
            };
            let target_address = page.target;
            let already_delivered = page.reached.is_some();

            if !already_delivered {
                // A target must exist, have page scan on, not already be
                // fielding another page, and not already be connected to us.
                let target = self.controllers.iter().position(|c| {
                    c.address == target_address
                        && c.classic.is_connectable()
                        && c.classic.inbound_page.is_none()
                });
                if let Some(target) = target
                    && target != i
                {
                    self.deliver_page(i, target);
                }
            }

            // Whether or not it landed, the page is on a clock. A target that
            // never enables page scan, or a host that never answers the
            // Connection Request, must both end in Page Timeout rather than
            // in a host waiting forever.
            let Some(page) = self.controllers[i].classic.page.as_mut() else {
                continue;
            };
            page.ticks_remaining = page.ticks_remaining.saturating_sub(1);
            if page.ticks_remaining > 0 {
                continue;
            }
            let reached = self.controllers[i]
                .classic
                .page
                .take()
                .and_then(|page| page.reached.map(|(peer, _)| peer));
            if let Some(peer) = reached {
                // Take back the Connection Request we raised: its page is
                // gone, so an Accept arriving later must find nothing.
                self.controllers[peer].classic.inbound_page = None;
            }
            self.controllers[i].outbox.push_back(connection_complete(
                STATUS_PAGE_TIMEOUT,
                0,
                target_address,
            ));
        }
    }

    /// Hand controller `target`'s host a Connection Request from `initiator`,
    /// and open the LMP link that will carry the answer.
    fn deliver_page(&mut self, initiator: usize, target: usize) {
        let initiator_address = self.controllers[initiator].address;
        let initiator_class = self.controllers[initiator].classic.class_of_device;

        let mut central = LmpLink::new(LmpRole::Central, LMP_FEATURES);
        let Ok(request) = central.build_connection_request() else {
            return;
        };
        // The peripheral end defers: answering a page is the host's call, and
        // its answer arrives as Accept/Reject Connection Request over HCI.
        let mut peripheral = LmpLink::deferring(LMP_FEATURES);
        if peripheral.receive(&request).is_err() {
            return;
        }

        if let Some(page) = self.controllers[initiator].classic.page.as_mut() {
            page.reached = Some((target, central));
        }
        self.controllers[target].classic.inbound_page = Some(InboundPage {
            initiator: initiator_address,
            lmp: peripheral,
        });
        self.controllers[target]
            .outbox
            .push_back(connection_request(
                initiator_address,
                initiator_class,
                LINK_TYPE_ACL,
            ));
    }

    /// The host answered a Connection Request. On acceptance, finish the LMP
    /// handshake and give **both** hosts a Connection Complete; on rejection,
    /// give both one carrying the reason.
    fn route_classic_answer_page(&mut self, from: usize, peer: Address, reject: Option<u8>) {
        let Some(inbound) = self.controllers[from].classic.inbound_page.take() else {
            return;
        };
        if inbound.initiator != peer {
            self.controllers[from].classic.inbound_page = Some(inbound);
            return;
        }
        // The initiator is whoever has a page that reached us — matching on
        // the page rather than only on the address, so a stale answer cannot
        // complete somebody else's connection.
        let initiator = self.controllers.iter().position(|c| {
            c.address == peer
                && c.classic
                    .page
                    .as_ref()
                    .and_then(|page| page.reached.as_ref())
                    .is_some_and(|(target, _)| *target == from)
        });
        let Some(initiator) = initiator else {
            return;
        };
        let Some(page) = self.controllers[initiator].classic.page.take() else {
            return;
        };
        let Some((_, central)) = page.reached else {
            return;
        };

        let (mut central, mut peripheral) = (central, inbound.lmp);
        let acceptor_address = self.controllers[from].address;

        if let Some(reason) = reject {
            let Ok(pdus) = peripheral.reject_pending_connection(reason) else {
                return;
            };
            shuttle_lmp(&mut peripheral, &mut central, pdus);
            // Both hosts are owed a completion: the initiator's Create
            // Connection and the acceptor's Reject Connection Request were
            // both answered with a Command Status, and a Command Status is a
            // promise of an event to come.
            self.controllers[initiator]
                .outbox
                .push_back(connection_complete(reason, 0, acceptor_address));
            self.controllers[from]
                .outbox
                .push_back(connection_complete(reason, 0, peer));
            return;
        }

        let Ok(pdus) = peripheral.accept_pending_connection() else {
            return;
        };
        shuttle_lmp(&mut peripheral, &mut central, pdus);
        if !central.is_connected() || !peripheral.is_connected() {
            // The LMP exchange did not converge. Say so rather than
            // reporting a connection neither link manager agreed to.
            self.controllers[initiator]
                .outbox
                .push_back(connection_complete(
                    STATUS_CONNECTION_FAILED,
                    0,
                    acceptor_address,
                ));
            self.controllers[from].outbox.push_back(connection_complete(
                STATUS_CONNECTION_FAILED,
                0,
                peer,
            ));
            return;
        }

        let handle = self.alloc_handle();
        self.controllers[initiator]
            .connections
            .push(Connection::new(handle, from));
        self.controllers[from]
            .connections
            .push(Connection::new(handle, initiator));
        self.controllers[initiator]
            .outbox
            .push_back(connection_complete(
                STATUS_SUCCESS,
                handle,
                acceptor_address,
            ));
        self.controllers[from]
            .outbox
            .push_back(connection_complete(STATUS_SUCCESS, handle, peer));
    }

    // --- SCO / eSCO ---------------------------------------------------------

    /// HCI Setup Synchronous Connection: raise a Connection Request with a
    /// synchronous link type at the peer's host, and wait for its answer.
    ///
    /// Nothing is allocated here. A handle that existed before the far end
    /// agreed would be a half-open link — the exact state a rejected setup
    /// must not leave behind — so the handle is allocated in
    /// [`Self::route_sco_answer`], on acceptance, and nowhere else.
    fn route_sco_setup(&mut self, from: usize, acl_handle: u16, link_type: u8, air_mode: u8) {
        let initiator_address = self.controllers[from].address;
        let Some(peer) = self.peer_of(from, acl_handle) else {
            // The ACL was there when the command was parsed and is gone now.
            self.controllers[from]
                .outbox
                .push_back(synchronous_connection_complete(
                    STATUS_UNKNOWN_CONNECTION,
                    0,
                    Address::ANY,
                    link_type,
                    air_mode,
                ));
            return;
        };
        let peer_address = self.controllers[peer].address;
        if self.controllers[peer].classic.inbound_sco.is_some()
            || self.controllers[peer]
                .sco_links
                .iter()
                .any(|l| l.peer == from)
        {
            // The peer's host already owes an answer for a synchronous
            // request, or already has one up. Refusing is the honest answer;
            // overwriting the pending request would strand the first one.
            self.controllers[from]
                .outbox
                .push_back(synchronous_connection_complete(
                    STATUS_CONNECTION_REJECTED_RESOURCES,
                    0,
                    peer_address,
                    link_type,
                    air_mode,
                ));
            return;
        }
        let initiator_class = self.controllers[from].classic.class_of_device;
        self.controllers[peer].classic.inbound_sco = Some(InboundSco {
            initiator: from,
            initiator_address,
            acl_handle,
            link_type,
            air_mode,
        });
        self.controllers[peer].outbox.push_back(connection_request(
            initiator_address,
            initiator_class,
            link_type,
        ));
    }

    /// The host answered a synchronous Connection Request. On acceptance,
    /// allocate the SCO handle and give **both** hosts a Synchronous
    /// Connection Complete carrying it; on rejection, give both one carrying
    /// the reason and no handle.
    fn route_sco_answer(&mut self, from: usize, peer: Address, reject: Option<u8>) {
        let Some(request) = self.controllers[from].classic.inbound_sco.take() else {
            return;
        };
        if request.initiator_address != peer {
            self.controllers[from].classic.inbound_sco = Some(request);
            return;
        }
        let initiator = request.initiator;
        let acceptor_address = self.controllers[from].address;
        let (link_type, air_mode) = (request.link_type, request.air_mode);

        // Both hosts are owed a completion: the initiator's Setup and the
        // acceptor's Accept/Reject were each answered with a Command Status,
        // and a Command Status is a promise of an event to come.
        let refuse = |link: &mut Self, status: u8| {
            link.controllers[initiator]
                .outbox
                .push_back(synchronous_connection_complete(
                    status,
                    0,
                    acceptor_address,
                    link_type,
                    air_mode,
                ));
            link.controllers[from]
                .outbox
                .push_back(synchronous_connection_complete(
                    status, 0, peer, link_type, air_mode,
                ));
        };

        if let Some(reason) = reject {
            refuse(self, reason);
            return;
        }
        if self.peer_of(initiator, request.acl_handle) != Some(from) {
            // The ACL went away while the host was deciding. Without it there
            // is nothing to carry the audio.
            refuse(self, STATUS_UNKNOWN_CONNECTION);
            return;
        }

        let sco_handle = self.alloc_handle();
        for (index, other) in [(initiator, from), (from, initiator)] {
            self.controllers[index].sco_links.push(ScoLink {
                sco_handle,
                acl_handle: request.acl_handle,
                peer: other,
            });
        }
        self.controllers[initiator]
            .outbox
            .push_back(synchronous_connection_complete(
                STATUS_SUCCESS,
                sco_handle,
                acceptor_address,
                link_type,
                air_mode,
            ));
        self.controllers[from]
            .outbox
            .push_back(synchronous_connection_complete(
                STATUS_SUCCESS,
                sco_handle,
                peer,
                link_type,
                air_mode,
            ));
    }

    /// Deliver a synchronous data packet to the far end of its SCO/eSCO link
    /// — the call-audio counterpart of [`Self::route_acl`].
    ///
    /// The payload crosses **untouched**: this controller transcodes nothing.
    /// A CVSD or mSBC frame written by one host is the byte-identical frame
    /// the other host reads. What a real controller does to those bytes on
    /// the air is not modelled, and pretending otherwise is what the codec
    /// seam in `crate::classic::hfp` exists to avoid.
    fn route_sco(&mut self, from: usize, handle: u16, data: &[u8]) {
        if let Some(peer) = self.sco_peer_of(from, handle) {
            let mut pkt = vec![h4_type::HCI_SCO_DATA];
            pkt.extend_from_slice(data);
            self.controllers[peer].outbox.push_back(pkt);
        }
    }

    /// The peer controller index for `from`'s synchronous link on `handle`.
    ///
    /// Deliberately *not* folded into [`Self::peer_of`]: an ACL handle and a
    /// SCO handle come out of the same allocator but name different links,
    /// and a lookup that accepted either would let call audio ride the
    /// signalling channel — which works in a simulator and in nothing else.
    fn sco_peer_of(&self, from: usize, handle: u16) -> Option<usize> {
        self.controllers[from]
            .sco_links
            .iter()
            .find(|l| l.sco_handle == handle)
            .map(|l| l.peer)
    }

    /// Tear down the synchronous link `handle` names, telling both hosts.
    /// Returns whether `handle` was a SCO handle at all.
    fn route_sco_disconnect(&mut self, from: usize, handle: u16) -> bool {
        let Some(peer) = self.sco_peer_of(from, handle) else {
            return false;
        };
        for index in [from, peer] {
            self.controllers[index]
                .sco_links
                .retain(|l| l.sco_handle != handle);
        }
        self.controllers[from]
            .outbox
            .push_back(disconnection_complete(handle, REASON_LOCAL_HOST));
        self.controllers[peer]
            .outbox
            .push_back(disconnection_complete(handle, REASON_REMOTE_USER));
        true
    }

    // --- end SCO / eSCO -----------------------------------------------------

    /// Answer each queued Remote Name Request with the peer's Write Local
    /// Name, or with Page Timeout if it names nobody reachable.
    ///
    /// A Remote Name Request *pages* the device, so it needs page scan — a
    /// device that is discoverable but not connectable shows up in an inquiry
    /// and then refuses to give its name, which is exactly what real hardware
    /// does and exactly what a host's "unknown device" entry means.
    fn tick_remote_names(&mut self) {
        for i in 0..self.controllers.len() {
            let requests = std::mem::take(&mut self.controllers[i].classic.remote_name_requests);
            for target in requests {
                let name = self
                    .controllers
                    .iter()
                    .find(|c| c.address == target && c.classic.is_connectable())
                    .map(|c| c.classic.local_name);
                let event = match name {
                    Some(name) => remote_name_request_complete(STATUS_SUCCESS, target, &name),
                    None => remote_name_request_complete(STATUS_PAGE_TIMEOUT, target, &[0u8; 248]),
                };
                self.controllers[i].outbox.push_back(event);
            }
        }
    }

    /// Handles one BR/EDR command, returning whether it was one.
    ///
    /// **The answer each command gives**, because getting this wrong is the
    /// recurring bug in this project — a host that gets a Command Complete
    /// where it expected a Command Status waits forever for a completion
    /// event that is never coming:
    ///
    /// | Command | Answer |
    /// |---|---|
    /// | Inquiry | Command **Status**, then Inquiry Result(s), then Inquiry Complete |
    /// | Inquiry Cancel | Command **Complete** (and *no* Inquiry Complete) |
    /// | Create Connection | Command **Status**, then Connection Complete (Connection Request at the peer) |
    /// | Create Connection Cancel | Command **Complete**, then Connection Complete with an error status |
    /// | Accept Connection Request | Command **Status**, then Connection Complete at both ends |
    /// | Reject Connection Request | Command **Status**, then Connection Complete carrying the refusal at the initiator |
    /// | Remote Name Request | Command **Status**, then Remote Name Request Complete |
    /// | Write Scan Enable / Local Name / Class of Device | Command **Complete** |
    /// | Read Scan Enable / Local Name / Class of Device | Command **Complete** carrying the value |
    ///
    /// The split is Bumble's `HCI_AsyncCommand` / `HCI_SyncCommand` split in
    /// `bumble/hci.py`, which is the table this was checked against rather
    /// than transcribed by eye.
    fn handle_classic_command(
        &mut self,
        i: usize,
        opcode: u16,
        params: &[u8],
        actions: &mut Vec<Action>,
    ) -> bool {
        // Whether the address in a Create Connection already names a peer we
        // hold a link to. Computed before the single-controller borrow below,
        // since it needs to look at the other end.
        let already_connected = |link: &Self, params: &[u8]| -> bool {
            let Some(target) = classic_address(params) else {
                return false;
            };
            link.controllers[i]
                .connections
                .iter()
                .any(|conn| link.controllers[conn.peer].address == target)
        };
        let duplicate_connection =
            opcode == opcode::CREATE_CONNECTION && already_connected(self, params);

        let c = &mut self.controllers[i];
        match opcode {
            opcode::INQUIRY => {
                // Inquiry_LAP(3), Inquiry_Length(1), Num_Responses(1). The
                // length is in 1.28 s units; here it is ticks, clamped so a
                // host asking for the spec maximum does not stall a scene.
                let length = u32::from(params.get(3).copied().unwrap_or(1)).clamp(1, 8);
                c.outbox.push_back(command_status(STATUS_SUCCESS, opcode));
                c.classic.inquiry = Some(Inquiry {
                    ticks_remaining: length,
                    results_sent: false,
                });
                true
            }
            opcode::INQUIRY_CANCEL => {
                // Command Complete, and deliberately no Inquiry Complete: the
                // spec says a cancelled inquiry does not send one (Vol 4,
                // Part E, Section 7.1.2), so a host that waits for one after
                // cancelling waits forever — on real hardware too.
                c.classic.inquiry = None;
                c.outbox
                    .push_back(command_complete(opcode, &[STATUS_SUCCESS]));
                true
            }
            opcode::CREATE_CONNECTION => {
                let target = classic_address(params).unwrap_or(Address::ANY);
                if duplicate_connection {
                    // BR/EDR allows exactly one ACL link between a pair of
                    // devices; a second Create Connection is refused rather
                    // than silently making a parallel one.
                    c.outbox
                        .push_back(command_status(STATUS_CONNECTION_ALREADY_EXISTS, opcode));
                    return true;
                }
                if c.classic.page.is_some() {
                    // One page at a time. The refusal is a Command Status,
                    // not a Command Complete — an error answer to a
                    // status-type command is still a status.
                    c.outbox
                        .push_back(command_status(STATUS_COMMAND_DISALLOWED, opcode));
                    return true;
                }
                c.outbox.push_back(command_status(STATUS_SUCCESS, opcode));
                c.classic.page = Some(Page {
                    target,
                    ticks_remaining: PAGE_TIMEOUT_TICKS,
                    reached: None,
                });
                true
            }
            opcode::CREATE_CONNECTION_CANCEL => {
                // Command Complete carrying status + BD_ADDR, then — if a
                // page really was in flight — a Connection Complete with an
                // error status, because the host is still owed the
                // completion event its Create Connection promised.
                let target = classic_address(params).unwrap_or(Address::ANY);
                let cancelled = c
                    .classic
                    .page
                    .as_ref()
                    .is_some_and(|page| page.target == target);
                let status = if cancelled {
                    STATUS_SUCCESS
                } else {
                    STATUS_UNKNOWN_CONNECTION
                };
                let mut ret = vec![status];
                ret.extend_from_slice(&addr_le(target));
                c.outbox.push_back(command_complete(opcode, &ret));
                if cancelled {
                    c.classic.page = None;
                    // Status 0x02 Unknown Connection Identifier, which is
                    // what the spec names for the completion event of a
                    // cancelled page (Vol 4, Part E, Section 7.1.7) — not
                    // Page Timeout, and not success.
                    c.outbox
                        .push_back(connection_complete(STATUS_UNKNOWN_CONNECTION, 0, target));
                }
                true
            }
            opcode::ACCEPT_CONNECTION_REQUEST | opcode::REJECT_CONNECTION_REQUEST => {
                let peer = classic_address(params).unwrap_or(Address::ANY);
                let matches_pending = c
                    .classic
                    .inbound_page
                    .as_ref()
                    .is_some_and(|page| page.initiator == peer);
                if !matches_pending {
                    // Bumble answers this exact case with a Command Status
                    // carrying UNKNOWN_CONNECTION_IDENTIFIER, and so do we:
                    // a rejected command answered with the wrong *event type*
                    // hangs a host just as thoroughly as no answer at all.
                    c.outbox
                        .push_back(command_status(STATUS_UNKNOWN_CONNECTION, opcode));
                    return true;
                }
                c.outbox.push_back(command_status(STATUS_SUCCESS, opcode));
                let reject = (opcode == opcode::REJECT_CONNECTION_REQUEST).then(|| {
                    // Reject Connection Request carries the host's reason as
                    // its last parameter.
                    params
                        .get(6)
                        .copied()
                        .unwrap_or(STATUS_CONNECTION_REJECTED_RESOURCES)
                });
                actions.push(Action::ClassicAnswerPage {
                    from: i,
                    peer,
                    reject,
                });
                true
            }
            opcode::REMOTE_NAME_REQUEST => {
                let target = classic_address(params).unwrap_or(Address::ANY);
                c.outbox.push_back(command_status(STATUS_SUCCESS, opcode));
                c.classic.remote_name_requests.push(target);
                true
            }
            opcode::WRITE_SCAN_ENABLE => {
                c.classic.scan_enable = params.first().copied().unwrap_or(0);
                c.outbox
                    .push_back(command_complete(opcode, &[STATUS_SUCCESS]));
                true
            }
            opcode::READ_SCAN_ENABLE => {
                c.outbox.push_back(command_complete(
                    opcode,
                    &[STATUS_SUCCESS, c.classic.scan_enable],
                ));
                true
            }
            opcode::WRITE_LOCAL_NAME => {
                // The parameter is a fixed 248-byte field, NUL-padded. A host
                // that sends a short one is taken at its word for the bytes
                // it did send.
                let len = params.len().min(c.classic.local_name.len());
                c.classic.local_name = [0u8; 248];
                c.classic.local_name[..len].copy_from_slice(&params[..len]);
                c.outbox
                    .push_back(command_complete(opcode, &[STATUS_SUCCESS]));
                true
            }
            opcode::READ_LOCAL_NAME => {
                let mut ret = vec![STATUS_SUCCESS];
                ret.extend_from_slice(&c.classic.local_name);
                c.outbox.push_back(command_complete(opcode, &ret));
                true
            }
            opcode::WRITE_CLASS_OF_DEVICE => {
                if let Some(b) = params.get(0..3) {
                    c.classic.class_of_device = [b[0], b[1], b[2]];
                }
                c.outbox
                    .push_back(command_complete(opcode, &[STATUS_SUCCESS]));
                true
            }
            opcode::READ_CLASS_OF_DEVICE => {
                let mut ret = vec![STATUS_SUCCESS];
                ret.extend_from_slice(&c.classic.class_of_device);
                c.outbox.push_back(command_complete(opcode, &ret));
                true
            }

            // --- SCO / eSCO ------------------------------------------------
            //
            // All five are Command-Status commands (Vol 4, Part E, and
            // Bumble's `HCI_AsyncCommand` split agrees), so *every* answer
            // below — including every refusal — is a Command Status. A
            // Command Complete here would hang a host waiting for the
            // Synchronous Connection Complete that a Status promises.
            opcode::SETUP_SYNCHRONOUS_CONNECTION
            | opcode::ENHANCED_SETUP_SYNCHRONOUS_CONNECTION => {
                let enhanced = opcode == opcode::ENHANCED_SETUP_SYNCHRONOUS_CONNECTION;
                let Some((acl_handle, link_type, air_mode)) =
                    parse_synchronous_setup(params, enhanced)
                else {
                    c.outbox
                        .push_back(command_status(STATUS_INVALID_PARAMETERS, opcode));
                    return true;
                };
                if !c.is_connected(acl_handle) {
                    // A synchronous link is carried by an ACL. Naming a
                    // handle there is no ACL on is the commonest way to ask
                    // for call audio before the call's signalling link is up.
                    c.outbox
                        .push_back(command_status(STATUS_UNKNOWN_CONNECTION, opcode));
                    return true;
                }
                if c.sco_links.iter().any(|l| l.acl_handle == acl_handle) {
                    c.outbox
                        .push_back(command_status(STATUS_CONNECTION_ALREADY_EXISTS, opcode));
                    return true;
                }
                c.outbox.push_back(command_status(STATUS_SUCCESS, opcode));
                actions.push(Action::ScoSetup {
                    from: i,
                    acl_handle,
                    link_type,
                    air_mode,
                });
                true
            }
            opcode::ACCEPT_SYNCHRONOUS_CONNECTION_REQUEST
            | opcode::ENHANCED_ACCEPT_SYNCHRONOUS_CONNECTION_REQUEST
            | opcode::REJECT_SYNCHRONOUS_CONNECTION_REQUEST => {
                let peer = classic_address(params).unwrap_or(Address::ANY);
                let matches_pending = c
                    .classic
                    .inbound_sco
                    .as_ref()
                    .is_some_and(|request| request.initiator_address == peer);
                if !matches_pending {
                    c.outbox
                        .push_back(command_status(STATUS_UNKNOWN_CONNECTION, opcode));
                    return true;
                }
                c.outbox.push_back(command_status(STATUS_SUCCESS, opcode));
                let reject = (opcode == opcode::REJECT_SYNCHRONOUS_CONNECTION_REQUEST).then(|| {
                    // Reject Synchronous Connection Request is BD_ADDR then
                    // one reason octet.
                    params
                        .get(6)
                        .copied()
                        .unwrap_or(STATUS_CONNECTION_REJECTED_RESOURCES)
                });
                actions.push(Action::ScoAnswer {
                    from: i,
                    peer,
                    reject,
                });
                true
            }
            // --- end SCO / eSCO --------------------------------------------
            _ => false,
        }
    }

    // === security ========================================================
    //
    // Everything from here to the `=== end security ===` marker is Secure
    // Simple Pairing, link keys, BR/EDR authentication and encryption, and
    // LE encryption start. It is one block on purpose: it is the newest and
    // most-edited part of this file, and keeping it contiguous is what lets
    // two people work on the controller at once.

    /// Handles one security command — BR/EDR or LE — returning whether it was
    /// one.
    ///
    /// **The answer each command gives.** Same table as
    /// [`Self::handle_classic_command`]'s and for the same reason; the split
    /// here has a shape worth naming, because it is not arbitrary:
    ///
    /// | Command | Answer |
    /// |---|---|
    /// | Authentication Requested | Command **Status**, then Link Key Request at both hosts, then Authentication Complete at the asking host |
    /// | Set Connection Encryption | Command **Status**, then Encryption Change at both ends |
    /// | Change Connection Link Key | Command **Status**, then Link Key Notification at both, then Change Connection Link Key Complete |
    /// | Link Key Selection | Command **Status**, and nothing else — see below |
    /// | LE Enable Encryption | Command **Status**, then LE Long Term Key Request at the peer, then Encryption Change |
    /// | Link Key Request (Negative) Reply | Command **Complete** carrying status + BD_ADDR |
    /// | IO Capability Request (Negative) Reply | Command **Complete** carrying status + BD_ADDR |
    /// | User Confirmation Request (Negative) Reply | Command **Complete** carrying status + BD_ADDR |
    /// | User Passkey Request (Negative) Reply | Command **Complete** carrying status + BD_ADDR |
    /// | LE LTK Request (Negative) Reply | Command **Complete** carrying status + handle |
    /// | Read/Write Simple Pairing Mode | Command **Complete** |
    ///
    /// The shape: a command that *starts* something is answered with a
    /// Command Status, and a command that *answers a question the controller
    /// asked* is answered with a Command Complete — because the reply is
    /// itself the completion of the controller's question, and there is
    /// nothing left to promise. Every reply above is the second half of an
    /// event the controller sent first.
    ///
    /// Link Key Selection is the odd one: Core lists it as Command-Status-
    /// answered and gives it no completion event of its own, because its
    /// effect lands on the *next* encryption start rather than on anything
    /// that finishes. `scripts/check_hci_command_answers.py` is what keeps
    /// that claim honest; the arm below emits a Command Status and stops.
    fn handle_security_command(
        &mut self,
        i: usize,
        opcode: u16,
        params: &[u8],
        actions: &mut Vec<Action>,
    ) -> bool {
        let c = &mut self.controllers[i];
        match opcode {
            // --- commands that start something: Command Status ----------
            opcode::AUTHENTICATION_REQUESTED => {
                let handle = le_u16(params, 0);
                if !c.is_connected(handle) {
                    c.outbox
                        .push_back(command_status(STATUS_UNKNOWN_CONNECTION, opcode));
                    return true;
                }
                c.outbox.push_back(command_status(STATUS_SUCCESS, opcode));
                actions.push(Action::ClassicSecurity {
                    from: i,
                    step: SecurityStep::Authenticate { handle },
                });
                true
            }
            opcode::SET_CONNECTION_ENCRYPTION => {
                let handle = le_u16(params, 0);
                let enable = params.get(2).copied().unwrap_or(0x00);
                if !c.is_connected(handle) {
                    c.outbox
                        .push_back(command_status(STATUS_UNKNOWN_CONNECTION, opcode));
                    return true;
                }
                c.outbox.push_back(command_status(STATUS_SUCCESS, opcode));
                actions.push(Action::ClassicSecurity {
                    from: i,
                    step: SecurityStep::Encrypt { handle, enable },
                });
                true
            }
            opcode::CHANGE_CONNECTION_LINK_KEY => {
                let handle = le_u16(params, 0);
                let authenticated = c
                    .connections
                    .iter()
                    .any(|conn| conn.handle == handle && conn.authenticated);
                if !c.is_connected(handle) {
                    c.outbox
                        .push_back(command_status(STATUS_UNKNOWN_CONNECTION, opcode));
                } else if !authenticated {
                    // There is no key to change. An error Command Status is
                    // the end of the command, so no Change Connection Link
                    // Key Complete is owed and none is sent.
                    c.outbox
                        .push_back(command_status(STATUS_COMMAND_DISALLOWED, opcode));
                } else {
                    c.outbox.push_back(command_status(STATUS_SUCCESS, opcode));
                    actions.push(Action::ClassicSecurity {
                        from: i,
                        step: SecurityStep::ChangeLinkKey { handle },
                    });
                }
                true
            }
            opcode::LINK_KEY_SELECTION => {
                c.classic.link_key_flag = params.first().copied().unwrap_or(0x00);
                c.outbox.push_back(command_status(STATUS_SUCCESS, opcode));
                true
            }
            opcode::LE_ENABLE_ENCRYPTION => {
                // Connection_Handle(2), Random_Number(8), EDIV(2), LTK(16).
                let handle = le_u16(params, 0);
                if !c.is_connected(handle) {
                    c.outbox
                        .push_back(command_status(STATUS_UNKNOWN_CONNECTION, opcode));
                    return true;
                }
                let mut rand = [0u8; 8];
                let mut ltk = [0u8; 16];
                if let Some(b) = params.get(2..10) {
                    rand.copy_from_slice(b);
                }
                if let Some(b) = params.get(12..28) {
                    ltk.copy_from_slice(b);
                }
                let ediv = le_u16(params, 10);
                c.outbox.push_back(command_status(STATUS_SUCCESS, opcode));
                actions.push(Action::LeEnableEncryption {
                    from: i,
                    handle,
                    ltk,
                    rand,
                    ediv,
                });
                true
            }

            // --- replies to the controller's own questions: Complete ----
            opcode::LINK_KEY_REQUEST_REPLY | opcode::LINK_KEY_REQUEST_NEGATIVE_REPLY => {
                let peer = classic_address(params).unwrap_or(Address::ANY);
                let key = (opcode == opcode::LINK_KEY_REQUEST_REPLY)
                    .then(|| {
                        let mut key = [0u8; 16];
                        params.get(6..22).map(|b| {
                            key.copy_from_slice(b);
                            key
                        })
                    })
                    .flatten();
                let mut ret = vec![STATUS_SUCCESS];
                ret.extend_from_slice(&addr_le(peer));
                c.outbox.push_back(command_complete(opcode, &ret));
                actions.push(Action::ClassicSecurity {
                    from: i,
                    step: SecurityStep::LinkKeyReply { peer, key },
                });
                true
            }
            opcode::IO_CAPABILITY_REQUEST_REPLY | opcode::IO_CAPABILITY_REQUEST_NEGATIVE_REPLY => {
                let peer = classic_address(params).unwrap_or(Address::ANY);
                let positive = opcode == opcode::IO_CAPABILITY_REQUEST_REPLY;
                let capabilities = positive.then(|| IoCapabilities {
                    io: params
                        .get(6)
                        .copied()
                        .unwrap_or(io_capability::NO_INPUT_NO_OUTPUT),
                    oob: params.get(7).copied().unwrap_or(0x00),
                    auth: params.get(8).copied().unwrap_or(0x00),
                });
                // The negative reply's last parameter is the host's reason,
                // and it is the reason both hosts are given for the failure.
                let reason = params.get(6).copied().unwrap_or(STATUS_PAIRING_NOT_ALLOWED);
                let mut ret = vec![STATUS_SUCCESS];
                ret.extend_from_slice(&addr_le(peer));
                c.outbox.push_back(command_complete(opcode, &ret));
                actions.push(Action::ClassicSecurity {
                    from: i,
                    step: SecurityStep::IoCapabilityReply {
                        peer,
                        capabilities,
                        reason,
                    },
                });
                true
            }
            opcode::USER_CONFIRMATION_REQUEST_REPLY
            | opcode::USER_CONFIRMATION_REQUEST_NEGATIVE_REPLY => {
                let peer = classic_address(params).unwrap_or(Address::ANY);
                let accept = opcode == opcode::USER_CONFIRMATION_REQUEST_REPLY;
                let mut ret = vec![STATUS_SUCCESS];
                ret.extend_from_slice(&addr_le(peer));
                c.outbox.push_back(command_complete(opcode, &ret));
                actions.push(Action::ClassicSecurity {
                    from: i,
                    step: SecurityStep::ConfirmationReply { peer, accept },
                });
                true
            }
            opcode::USER_PASSKEY_REQUEST_REPLY | opcode::USER_PASSKEY_REQUEST_NEGATIVE_REPLY => {
                let peer = classic_address(params).unwrap_or(Address::ANY);
                let passkey = (opcode == opcode::USER_PASSKEY_REQUEST_REPLY).then(|| {
                    params
                        .get(6..10)
                        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                        .unwrap_or(0)
                });
                let mut ret = vec![STATUS_SUCCESS];
                ret.extend_from_slice(&addr_le(peer));
                c.outbox.push_back(command_complete(opcode, &ret));
                actions.push(Action::ClassicSecurity {
                    from: i,
                    step: SecurityStep::PasskeyReply { peer, passkey },
                });
                true
            }
            opcode::LE_LTK_REQUEST_REPLY | opcode::LE_LTK_REQUEST_NEGATIVE_REPLY => {
                let handle = le_u16(params, 0);
                let ltk = (opcode == opcode::LE_LTK_REQUEST_REPLY)
                    .then(|| {
                        let mut ltk = [0u8; 16];
                        params.get(2..18).map(|b| {
                            ltk.copy_from_slice(b);
                            ltk
                        })
                    })
                    .flatten();
                let mut ret = vec![STATUS_SUCCESS];
                ret.extend_from_slice(&handle.to_le_bytes());
                c.outbox.push_back(command_complete(opcode, &ret));
                actions.push(Action::LeLtkReply {
                    from: i,
                    handle,
                    ltk,
                });
                true
            }
            opcode::WRITE_SIMPLE_PAIRING_MODE => {
                c.classic.simple_pairing_mode = params.first().copied().unwrap_or(0x00);
                c.outbox
                    .push_back(command_complete(opcode, &[STATUS_SUCCESS]));
                true
            }
            opcode::READ_SIMPLE_PAIRING_MODE => {
                c.outbox.push_back(command_complete(
                    opcode,
                    &[STATUS_SUCCESS, c.classic.simple_pairing_mode],
                ));
                true
            }
            _ => false,
        }
    }

    /// The pairing conversation controller `from` is having with the device
    /// at `peer`, if any. Matched on the *pair* rather than on the address
    /// alone, so a reply that names the wrong peer resolves nothing instead
    /// of resolving somebody else's pairing.
    fn pairing_index(&self, from: usize, peer: Address) -> Option<usize> {
        self.pairings.iter().position(|pairing| {
            pairing
                .side_of(from)
                .is_some_and(|side| self.controllers[pairing.ends[1 - side]].address == peer)
        })
    }

    /// Applies one host's contribution to a security procedure.
    fn route_classic_security(&mut self, from: usize, step: SecurityStep) {
        match step {
            SecurityStep::Authenticate { handle } => self.route_authenticate(from, handle),
            SecurityStep::LinkKeyReply { peer, key } => {
                let Some(index) = self.pairing_index(from, peer) else {
                    return;
                };
                let Some(side) = self.pairings[index].side_of(from) else {
                    return;
                };
                if self.pairings[index].stage != PairingStage::LinkKey {
                    return;
                }
                self.pairings[index].link_key[side] = Some(key);
                self.advance_pairing(index);
            }
            SecurityStep::IoCapabilityReply {
                peer,
                capabilities,
                reason,
            } => {
                let Some(index) = self.pairing_index(from, peer) else {
                    return;
                };
                let Some(side) = self.pairings[index].side_of(from) else {
                    return;
                };
                if self.pairings[index].stage != PairingStage::IoCapability {
                    return;
                }
                let Some(capabilities) = capabilities else {
                    // An IO Capability Request Negative Reply ends the
                    // pairing there and then; there is no model to select.
                    self.finish_pairing(index, reason, true, None);
                    return;
                };
                self.pairings[index].io[side] = Some(capabilities);
                // The peer's host learns our capabilities from its own IO
                // Capability Response — the event that has no reply and
                // exists only to inform.
                let other = self.pairings[index].ends[1 - side];
                let our_address = self.controllers[from].address;
                self.controllers[other]
                    .outbox
                    .push_back(io_capability_response(our_address, capabilities));
                self.advance_pairing(index);
            }
            SecurityStep::ConfirmationReply { peer, accept } => {
                let Some(index) = self.pairing_index(from, peer) else {
                    return;
                };
                let Some(side) = self.pairings[index].side_of(from) else {
                    return;
                };
                if self.pairings[index].stage != PairingStage::UserAction {
                    return;
                }
                self.pairings[index].decision[side] = Some(accept);
                self.advance_pairing(index);
            }
            SecurityStep::PasskeyReply { peer, passkey } => {
                let Some(index) = self.pairing_index(from, peer) else {
                    return;
                };
                let Some(side) = self.pairings[index].side_of(from) else {
                    return;
                };
                if self.pairings[index].stage != PairingStage::UserAction {
                    return;
                }
                self.pairings[index].decision[side] = Some(passkey.is_some());
                self.pairings[index].entered[side] = passkey;
                self.advance_pairing(index);
            }
            SecurityStep::Encrypt { handle, enable } => {
                self.route_set_connection_encryption(from, handle, enable);
            }
            SecurityStep::ChangeLinkKey { handle } => {
                self.route_change_connection_link_key(from, handle);
            }
        }
    }

    /// Authentication Requested: open a pairing conversation on `handle` and
    /// ask **both** hosts whether they already hold a link key for the other.
    ///
    /// Asking both is the whole design. A reconnect between two hosts that
    /// each stored the key ends at the next step with no SSP at all, and a
    /// reconnect where either side has forgotten runs the full pairing —
    /// which is the observable difference a bonded device has, and the reason
    /// a link key store is not just a cache.
    fn route_authenticate(&mut self, from: usize, handle: u16) {
        let Some(peer) = self.peer_of(from, handle) else {
            return;
        };
        if self.pairings.iter().any(|p| p.handle == handle) {
            // One conversation per link. A second Authentication Requested
            // while the first is still running joins nothing and is dropped;
            // its host is still owed the Authentication Complete the first
            // one will send.
            return;
        }
        let from_address = self.controllers[from].address;
        let peer_address = self.controllers[peer].address;
        let key = derived_link_key(from_address, peer_address);
        self.pairings.push(ClassicPairing {
            handle,
            ends: [from, peer],
            link_key: [None, None],
            io: [None, None],
            decision: [None, None],
            entered: [None, None],
            stage: PairingStage::LinkKey,
            key,
            numeric_value: pairing_digits(&key, 0),
            passkey: pairing_digits(&key, 1),
            model: AssociationModel::JustWorks,
        });
        self.controllers[from]
            .outbox
            .push_back(link_key_request(peer_address));
        self.controllers[peer]
            .outbox
            .push_back(link_key_request(from_address));
    }

    /// Moves the pairing at `index` on as far as the answers in hand allow.
    /// Called after every host reply; returns quietly while an answer is
    /// still outstanding.
    fn advance_pairing(&mut self, index: usize) {
        match self.pairings[index].stage {
            PairingStage::LinkKey => {
                let (Some(ours), Some(theirs)) = (
                    self.pairings[index].link_key[0],
                    self.pairings[index].link_key[1],
                ) else {
                    return;
                };
                if let (Some(ours), Some(theirs)) = (ours, theirs)
                    && ours == theirs
                {
                    // Both hosts produced the same stored key. This is the
                    // bonded path: no SSP, no Link Key Notification, no user
                    // anywhere — just an Authentication Complete.
                    self.finish_pairing(index, STATUS_SUCCESS, false, None);
                    return;
                }
                let ends = self.pairings[index].ends;
                if ends
                    .iter()
                    .any(|end| self.controllers[*end].classic.simple_pairing_mode == 0)
                {
                    // Pairing is needed and SSP is not switched on at one of
                    // the ends. Real hardware would fall back to legacy PIN
                    // pairing; this controller does not model it, and saying
                    // so beats running SSP the host never enabled.
                    self.finish_pairing(index, STATUS_PAIRING_NOT_ALLOWED, false, None);
                    return;
                }
                self.pairings[index].stage = PairingStage::IoCapability;
                for side in 0..2 {
                    let peer_address = self.controllers[ends[1 - side]].address;
                    self.controllers[ends[side]]
                        .outbox
                        .push_back(io_capability_request(peer_address));
                }
            }
            PairingStage::IoCapability => {
                let (Some(first), Some(second)) =
                    (self.pairings[index].io[0], self.pairings[index].io[1])
                else {
                    return;
                };
                let model = association_model(first.io, first.auth, second.io, second.auth);
                self.pairings[index].model = model;
                self.pairings[index].stage = PairingStage::UserAction;
                let ends = self.pairings[index].ends;
                let numeric_value = self.pairings[index].numeric_value;
                let passkey = self.pairings[index].passkey;
                let capabilities = [first, second];
                for side in 0..2 {
                    let peer_address = self.controllers[ends[1 - side]].address;
                    match model {
                        // Passkey Entry asks the side that can type and only
                        // *tells* the side that can display: a display has
                        // nothing to answer with, so it has already agreed by
                        // the time it is told.
                        AssociationModel::PasskeyEntry
                            if capabilities[side].io == io_capability::KEYBOARD_ONLY =>
                        {
                            self.controllers[ends[side]]
                                .outbox
                                .push_back(user_passkey_request(peer_address));
                        }
                        AssociationModel::PasskeyEntry => {
                            self.controllers[ends[side]]
                                .outbox
                                .push_back(user_passkey_notification(peer_address, passkey));
                            self.pairings[index].decision[side] = Some(true);
                            self.pairings[index].entered[side] = Some(passkey);
                        }
                        // Just Works and Numeric Comparison put exactly the
                        // same bytes on the wire. The difference is whether a
                        // person is expected to look at them, which is the
                        // host's business — and it is why the two produce
                        // different *key types* below.
                        AssociationModel::JustWorks | AssociationModel::NumericComparison => {
                            self.controllers[ends[side]]
                                .outbox
                                .push_back(user_confirmation_request(peer_address, numeric_value));
                        }
                    }
                }
                self.advance_pairing(index);
            }
            PairingStage::UserAction => {
                let (Some(first), Some(second)) = (
                    self.pairings[index].decision[0],
                    self.pairings[index].decision[1],
                ) else {
                    return;
                };
                let entered = self.pairings[index].entered;
                let digits_agree = self.pairings[index].model != AssociationModel::PasskeyEntry
                    || (entered[0].is_some() && entered[0] == entered[1]);
                if !(first && second && digits_agree) {
                    self.finish_pairing(index, STATUS_AUTHENTICATION_FAILURE, true, None);
                    return;
                }
                // Only a model a person actually took part in produces an
                // authenticated key. A service that requires MITM protection
                // reads this byte and refuses a Just Works bond.
                let key_type = match self.pairings[index].model {
                    AssociationModel::JustWorks => link_key_type::UNAUTHENTICATED_P192,
                    AssociationModel::NumericComparison | AssociationModel::PasskeyEntry => {
                        link_key_type::AUTHENTICATED_P192
                    }
                };
                self.finish_pairing(index, STATUS_SUCCESS, true, Some(key_type));
            }
        }
    }

    /// Resolves the pairing at `index`: tell whoever is owed what, mark the
    /// link, and forget the conversation.
    ///
    /// `ssp` says whether Secure Simple Pairing actually ran — a link that
    /// authenticated straight off two stored keys never started one, and a
    /// host handed a Simple Pairing Complete for a pairing that did not
    /// happen would have to invent a reason for it. `key_type` is `Some`
    /// only when a *new* key was produced, which is what decides whether the
    /// two hosts get a Link Key Notification to store.
    fn finish_pairing(&mut self, index: usize, status: u8, ssp: bool, key_type: Option<u8>) {
        let pairing = self.pairings.remove(index);
        let ends = pairing.ends;
        if let Some(key_type) = key_type {
            for side in 0..2 {
                let peer_address = self.controllers[ends[1 - side]].address;
                self.controllers[ends[side]]
                    .outbox
                    .push_back(link_key_notification(peer_address, &pairing.key, key_type));
            }
        }
        if ssp {
            for side in 0..2 {
                let peer_address = self.controllers[ends[1 - side]].address;
                self.controllers[ends[side]]
                    .outbox
                    .push_back(simple_pairing_complete(status, peer_address));
            }
        }
        if status == STATUS_SUCCESS {
            for end in ends {
                if let Some(conn) = self.controllers[end]
                    .connections
                    .iter_mut()
                    .find(|c| c.handle == pairing.handle)
                {
                    conn.authenticated = true;
                }
            }
        }
        // Authentication Complete goes to the host that asked and to nobody
        // else. The peer learns what happened from Simple Pairing Complete,
        // and on the bonded path it learns nothing at all — which is exactly
        // what a real acceptor sees.
        self.controllers[ends[0]]
            .outbox
            .push_back(authentication_complete(status, pairing.handle));
    }

    /// Set Connection Encryption: flip both ends of `handle`, or refuse.
    ///
    /// Encryption is state here, not cryptography — nothing on the link is
    /// enciphered and the ACL router does not change. What is modelled is the
    /// thing a profile can actually act on: whether the link has a key and
    /// says it is encrypted.
    fn route_set_connection_encryption(&mut self, from: usize, handle: u16, enable: u8) {
        let Some(peer) = self.peer_of(from, handle) else {
            return;
        };
        let authenticated = self.controllers[from]
            .connections
            .iter()
            .any(|c| c.handle == handle && c.authenticated);
        if enable != 0 && !authenticated {
            // No key, so no encryption — and *nothing changes at either end*.
            // A link that came back encrypted on one side only would be worse
            // than one that stayed clear, because both halves would believe
            // themselves right.
            self.controllers[from].outbox.push_back(encryption_change(
                STATUS_PIN_OR_KEY_MISSING,
                handle,
                ENCRYPTION_OFF,
            ));
            return;
        }
        let enabled = if enable != 0 {
            ENCRYPTION_ON
        } else {
            ENCRYPTION_OFF
        };
        for end in [from, peer] {
            if let Some(conn) = self.controllers[end]
                .connections
                .iter_mut()
                .find(|c| c.handle == handle)
            {
                conn.encrypted = enable != 0;
            }
            self.controllers[end].outbox.push_back(encryption_change(
                STATUS_SUCCESS,
                handle,
                enabled,
            ));
        }
    }

    /// Change Connection Link Key: derive the next key for this link, notify
    /// both hosts so they can store it, and complete at the asking host.
    ///
    /// The rotation counter lives on the connection so that asking twice
    /// produces two different keys; without it the "new" key would be the
    /// same one every time, which is a rotation that rotates nothing.
    fn route_change_connection_link_key(&mut self, from: usize, handle: u16) {
        let Some(peer) = self.peer_of(from, handle) else {
            return;
        };
        let rotation = self.controllers[from]
            .connections
            .iter()
            .find(|c| c.handle == handle)
            .map(|c| c.key_rotations.wrapping_add(1))
            .unwrap_or(1);
        let base = derived_link_key(
            self.controllers[from].address,
            self.controllers[peer].address,
        );
        let key = crate::crypto::aes_cmac(&base, &[rotation]);
        for end in [from, peer] {
            if let Some(conn) = self.controllers[end]
                .connections
                .iter_mut()
                .find(|c| c.handle == handle)
            {
                conn.key_rotations = rotation;
            }
        }
        for (end, other) in [(from, peer), (peer, from)] {
            let peer_address = self.controllers[other].address;
            self.controllers[end]
                .outbox
                .push_back(link_key_notification(
                    peer_address,
                    &key,
                    link_key_type::UNAUTHENTICATED_P192,
                ));
        }
        self.controllers[from]
            .outbox
            .push_back(change_connection_link_key_complete(STATUS_SUCCESS, handle));
    }

    /// LE Enable Encryption: hold the key the central named and ask the
    /// peripheral's host for its own with an LE Long Term Key Request.
    ///
    /// This is the step `smp/pairing.rs` has been missing. SMP does the
    /// pairing maths and ends holding an LTK; until now there was no
    /// controller to hand it to, so the link never actually became encrypted
    /// and `PairingSession::on_link_encrypted` was called by nothing.
    fn route_le_enable_encryption(
        &mut self,
        from: usize,
        handle: u16,
        ltk: [u8; 16],
        rand: [u8; 8],
        ediv: u16,
    ) {
        let Some(peer) = self.peer_of(from, handle) else {
            return;
        };
        if let Some(conn) = self.controllers[from]
            .connections
            .iter_mut()
            .find(|c| c.handle == handle)
        {
            conn.pending_ltk = Some(ltk);
        }
        self.controllers[peer]
            .outbox
            .push_back(le_long_term_key_request(handle, rand, ediv));
    }

    /// The peripheral's host answered its LE Long Term Key Request. Two keys
    /// that match encrypt both ends; anything else fails at the central,
    /// which is the only side that asked for encryption in the first place.
    fn route_le_ltk_reply(&mut self, from: usize, handle: u16, ltk: Option<[u8; 16]>) {
        let Some(central) = self.peer_of(from, handle) else {
            return;
        };
        let expected = self.controllers[central]
            .connections
            .iter_mut()
            .find(|c| c.handle == handle)
            .and_then(|c| c.pending_ltk.take());
        let Some(expected) = expected else {
            // Nobody asked for encryption on this link, so this reply answers
            // a question that was never put.
            return;
        };
        if ltk != Some(expected) {
            self.controllers[central]
                .outbox
                .push_back(encryption_change(
                    STATUS_PIN_OR_KEY_MISSING,
                    handle,
                    ENCRYPTION_OFF,
                ));
            return;
        }
        for end in [central, from] {
            if let Some(conn) = self.controllers[end]
                .connections
                .iter_mut()
                .find(|c| c.handle == handle)
            {
                conn.encrypted = true;
            }
            self.controllers[end].outbox.push_back(encryption_change(
                STATUS_SUCCESS,
                handle,
                ENCRYPTION_ON,
            ));
        }
    }

    /// Drops pairing conversations whose connection is gone — a disconnect,
    /// or an HCI Reset at either end. A pairing that outlived its link would
    /// hand its Authentication Complete to a handle that now means something
    /// else.
    fn prune_pairings(&mut self) {
        let controllers = &self.controllers;
        self.pairings.retain(|pairing| {
            pairing
                .ends
                .iter()
                .all(|end| controllers[*end].is_connected(pairing.handle))
        });
    }

    // === end security ====================================================

    /// Join controller `central` to advertiser `peripheral`: allocate a shared
    /// handle, record the connection on both, stop the advertiser, and emit an
    /// LE Connection Complete to each host with the correct role.
    fn establish_connection(&mut self, central: usize, peripheral: usize) {
        let handle = self.alloc_handle();
        let central_addr = self.controllers[central].address;
        let peripheral_addr = self.controllers[peripheral].address;

        self.controllers[central]
            .connections
            .push(Connection::new(handle, peripheral));
        self.controllers[peripheral]
            .connections
            .push(Connection::new(handle, central));
        self.controllers[peripheral].advertising = false;

        // Role 0x00 = Central, 0x01 = Peripheral.
        self.controllers[central]
            .outbox
            .push_back(le_connection_complete(handle, 0x00, peripheral_addr));
        self.controllers[peripheral]
            .outbox
            .push_back(le_connection_complete(handle, 0x01, central_addr));
    }

    /// Tear down the connection on `handle` for controller `from`, notifying
    /// both ends with a Disconnection Complete.
    fn route_disconnect(&mut self, from: usize, handle: u16) {
        // A SCO handle is disconnected by the same command as an ACL handle,
        // and hanging up the audio must not take the signalling link with it.
        if self.route_sco_disconnect(from, handle) {
            return;
        }
        let Some(peer) = self.peer_of(from, handle) else {
            return;
        };
        // Every synchronous link on this ACL goes first, each with its own
        // Disconnection Complete on its own handle. A host told only about
        // the ACL keeps a SCO handle it will never hear from again — and
        // Zephyr's `bt_sco_cleanup_acl` is waiting for exactly these events.
        let sco_handles: Vec<u16> = self.controllers[from]
            .sco_links
            .iter()
            .filter(|l| l.acl_handle == handle)
            .map(|l| l.sco_handle)
            .collect();
        for sco_handle in sco_handles {
            self.route_sco_disconnect(from, sco_handle);
        }
        self.controllers[from]
            .connections
            .retain(|c| c.handle != handle);
        self.controllers[peer]
            .connections
            .retain(|c| c.handle != handle);
        // A Channel Sounding configuration lives on a connection and dies with
        // it. Leaving it behind would let a *later* connection that happened to
        // reuse the handle inherit a procedure its host never created — and
        // would leave `enabled` set on a session whose peer is gone.
        for index in [from, peer] {
            if self.controllers[index]
                .cs
                .is_some_and(|s| s.handle == handle)
            {
                self.controllers[index].cs = None;
            }
            // An isochronous stream is set up over an ACL link and cannot
            // outlive it, for the same reason.
            self.controllers[index]
                .cis_links
                .retain(|l| l.acl_handle != handle);
            // A synchronous request the host never got round to answering
            // dies with the ACL too — otherwise the next peer's Accept
            // Synchronous Connection Request answers the *last* peer's.
            if self.controllers[index]
                .classic
                .inbound_sco
                .as_ref()
                .is_some_and(|r| r.acl_handle == handle)
            {
                self.controllers[index].classic.inbound_sco = None;
            }
        }
        self.controllers[from]
            .outbox
            .push_back(disconnection_complete(handle, REASON_LOCAL_HOST));
        self.controllers[peer]
            .outbox
            .push_back(disconnection_complete(handle, REASON_REMOTE_USER));
    }

    /// Apply an LE Connection Update to both ends of `handle` and tell both
    /// hosts with an LE Connection Update Complete.
    fn route_connection_update(
        &mut self,
        from: usize,
        handle: u16,
        interval: u16,
        latency: u16,
        timeout: u16,
    ) {
        let Some(peer) = self.peer_of(from, handle) else {
            return;
        };
        for index in [from, peer] {
            if let Some(conn) = self.controllers[index]
                .connections
                .iter_mut()
                .find(|c| c.handle == handle)
            {
                conn.interval = interval;
                conn.latency = latency;
                conn.timeout = timeout;
            }
            self.controllers[index]
                .outbox
                .push_back(le_connection_update_complete(
                    handle, interval, latency, timeout,
                ));
        }
    }

    /// Apply an LE Set PHY to both ends of `handle`. The peer sees the
    /// directions swapped: one end's TX is the other end's RX.
    fn route_phy_update(&mut self, from: usize, handle: u16, tx_phy: u8, rx_phy: u8) {
        let Some(peer) = self.peer_of(from, handle) else {
            return;
        };
        for (index, (tx, rx)) in [(from, (tx_phy, rx_phy)), (peer, (rx_phy, tx_phy))] {
            if let Some(conn) = self.controllers[index]
                .connections
                .iter_mut()
                .find(|c| c.handle == handle)
            {
                conn.tx_phy = tx;
                conn.rx_phy = rx;
            }
            self.controllers[index]
                .outbox
                .push_back(le_phy_update_complete(handle, tx, rx));
        }
    }

    /// Tell the peer's host that a central wants an isochronous stream on
    /// `acl_handle`, and record the stream so its LE Accept CIS Request has
    /// something to name.
    fn route_cis_request(&mut self, from: usize, acl_handle: u16, cis_handle: u16) {
        let Some(peer) = self.peer_of(from, acl_handle) else {
            return;
        };
        self.controllers[peer].cis_links.push(CisLink {
            cis_handle,
            acl_handle,
            established: false,
        });
        self.controllers[peer]
            .outbox
            .push_back(le_cis_request(acl_handle, cis_handle));
    }

    /// The peripheral's host accepted: establish the stream at both ends.
    fn route_cis_accept(&mut self, from: usize, cis_handle: u16) {
        let Some(acl_handle) = self.controllers[from]
            .cis_links
            .iter()
            .find(|l| l.cis_handle == cis_handle)
            .map(|l| l.acl_handle)
        else {
            return;
        };
        let Some(peer) = self.peer_of(from, acl_handle) else {
            return;
        };
        for index in [from, peer] {
            if let Some(link) = self.controllers[index]
                .cis_links
                .iter_mut()
                .find(|l| l.cis_handle == cis_handle)
            {
                link.established = true;
            }
            self.controllers[index]
                .outbox
                .push_back(le_cis_established(STATUS_SUCCESS, cis_handle));
        }
    }

    /// Forward an ACL packet from `from` to the peer on `handle`.
    fn route_acl(&mut self, from: usize, handle: u16, data: &[u8]) {
        if let Some(peer) = self.peer_of(from, handle) {
            let mut pkt = vec![h4_type::HCI_ACL_DATA];
            pkt.extend_from_slice(data);
            self.controllers[peer].outbox.push_back(pkt);
        }
    }

    /// Delivers an isochronous SDU to the connection's peer — the media
    /// plane's counterpart to [`Self::route_acl`].
    ///
    /// A BIS handle is tried first: it belongs to a broadcast rather than to a
    /// connection, so its SDU goes to everyone synchronized to that BIG and to
    /// nobody in particular.
    fn route_iso(&mut self, from: usize, handle: u16, data: &[u8]) {
        if self.route_bis_iso(from, handle, data) {
            return;
        }
        if let Some(peer) = self.peer_of(from, handle) {
            let mut pkt = vec![h4_type::HCI_ISO_DATA];
            pkt.extend_from_slice(data);
            self.controllers[peer].outbox.push_back(pkt);
        }
    }

    /// Answers HCI Read RSSI (Vol 4, Part E, Section 7.5.4) for one of this
    /// controller's connections.
    ///
    /// This is how a *connected* device measures its peer's signal. A
    /// controller stops advertising once a connection is up, so there are no
    /// more advertising reports to read an RSSI out of — and a proximity
    /// feature that stopped working the moment you connected would be a
    /// strange thing to ship. Shadowing is redrawn per call, exactly as it is
    /// for an advertising report: a real Read RSSI moves between calls too.
    fn handle_read_rssi(&mut self, i: usize, handle: u16) {
        let rssi = match self.peer_of(i, handle) {
            Some(peer) => {
                let distance = self.controllers[i]
                    .position
                    .distance_to(self.controllers[peer].position);
                let shadowing = self.rng.normal_scaled(self.path_loss.shadowing_sigma_db);
                quantize_rssi(self.path_loss.rssi_dbm(distance, shadowing))
            }
            // 127 is HCI's "RSSI is not available", which is the honest
            // answer for a handle that is not connected.
            None => 127,
        };
        let mut ret = vec![STATUS_SUCCESS];
        ret.extend_from_slice(&handle.to_le_bytes());
        ret.push(rssi as u8);
        self.controllers[i]
            .outbox
            .push_back(command_complete(opcode::READ_RSSI, &ret));
    }

    /// Records a Channel Sounding configuration on `from`, and — when the
    /// host asked for it — the mirrored configuration on the peer, telling
    /// both hosts with an LE CS Config Complete.
    fn route_cs_configure(
        &mut self,
        from: usize,
        handle: u16,
        config_id: u8,
        role: u8,
        propagate_to_peer: bool,
    ) {
        self.controllers[from].cs = Some(CsSession {
            handle,
            config_id,
            role,
            enabled: false,
            procedure_counter: 0,
        });
        self.controllers[from].outbox.push_back(cs_config_complete(
            handle,
            config_id,
            role,
            cs_action::CREATED,
        ));

        if !propagate_to_peer {
            return;
        }
        let Some(peer) = self.peer_of(from, handle) else {
            return;
        };
        // The peer takes the opposite role: one end transmits the tone the
        // other measures, and both measure.
        let peer_role = u8::from(role == 0x00);
        self.controllers[peer].cs = Some(CsSession {
            handle,
            config_id,
            role: peer_role,
            enabled: false,
            procedure_counter: 0,
        });
        self.controllers[peer].outbox.push_back(cs_config_complete(
            handle,
            config_id,
            peer_role,
            cs_action::CREATED,
        ));
    }

    /// Enables or disables the configuration on both ends of `handle`.
    ///
    /// A request naming a configuration this controller never created is
    /// refused with an LE CS Procedure Enable Complete carrying Command
    /// Disallowed, rather than ignored. Silence would be worse than an error:
    /// the requesting host blocks on that event, so a controller that says
    /// nothing hangs it forever with no way to tell that apart from a
    /// procedure that is merely slow to start.
    fn route_cs_enable(&mut self, from: usize, handle: u16, config_id: u8, enable: bool) {
        let requester_has_config = self.controllers[from]
            .cs
            .is_some_and(|s| s.handle == handle && s.config_id == config_id);
        if !requester_has_config {
            let tx_power = self.path_loss.tx_power_dbm;
            self.controllers[from]
                .outbox
                .push_back(cs_procedure_enable_complete(
                    STATUS_COMMAND_DISALLOWED,
                    handle,
                    config_id,
                    false,
                    tx_power,
                ));
            return;
        }
        let peer = self.peer_of(from, handle);
        for index in [Some(from), peer].into_iter().flatten() {
            let Some(session) = self.controllers[index].cs.as_mut() else {
                continue;
            };
            if session.handle != handle || session.config_id != config_id {
                continue;
            }
            session.enabled = enable;
            let tx_power = self.path_loss.tx_power_dbm;
            self.controllers[index]
                .outbox
                .push_back(cs_procedure_enable_complete(
                    STATUS_SUCCESS,
                    handle,
                    config_id,
                    enable,
                    tx_power,
                ));
        }
    }

    /// Runs one Channel Sounding procedure per enabled initiator session and
    /// reports the tones **both** ends measured.
    ///
    /// This is where the physics happens, and where the honesty of the whole
    /// ranging demo lives. Each end measures the same propagation phase
    /// `2π·f·d/c`, but offset by its own local oscillator's phase, which it
    /// does not know:
    ///
    /// ```text
    ///   initiator sees:  φ_prop(f) + Δθ(f) + noise
    ///   reflector sees:  φ_prop(f) − Δθ(f) + noise
    /// ```
    ///
    /// `Δθ(f)` is the difference between the two local oscillators' phases,
    /// and it is redrawn **per tone**, not once per procedure: each radio
    /// re-locks its synthesizer on every hop and comes back with an arbitrary
    /// phase. So one end's measurements are, on their own, uniform noise —
    /// no amount of fitting recovers a distance from them. Their **sum** is
    /// `2·φ_prop(f)`, with `Δθ` gone. That per-tone cancellation is the whole
    /// reason Channel Sounding needs the Ranging Service: the reflector's
    /// tones have to reach the initiator's *host* before any distance exists.
    fn tick_channel_sounding(&mut self) {
        let channels = cs_plan::tone_channels();
        for i in 0..self.controllers.len() {
            let Some(session) = self.controllers[i].cs else {
                continue;
            };
            // Only the initiator's session drives a procedure; the reflector
            // is reported to as a side effect, never twice.
            if !session.enabled || session.role != 0x00 {
                continue;
            }
            let Some(peer) = self.peer_of(i, session.handle) else {
                continue;
            };
            if !self.controllers[peer].cs.is_some_and(|s| s.enabled) {
                continue;
            }

            let distance = self.controllers[i]
                .position
                .distance_to(self.controllers[peer].position);
            let sigma = phase_noise_sigma_rad(self.path_loss.snr_linear(distance));
            let reference_power = quantize_rssi(self.path_loss.rssi_dbm(distance, 0.0));

            let mut initiator_steps = Vec::with_capacity(channels.len());
            let mut reflector_steps = Vec::with_capacity(channels.len());
            for &channel in &channels {
                let phase = propagation_phase_rad(distance, channel_frequency_hz(channel));
                // A fresh oscillator-phase difference on every hop, applied
                // to the two ends with opposite sign so it cancels in the sum.
                let lo_offset = self.rng.uniform_phase();
                let initiator_phase = wrap_phase(phase + lo_offset + self.rng.normal_scaled(sigma));
                let reflector_phase = wrap_phase(phase - lo_offset + self.rng.normal_scaled(sigma));
                initiator_steps.push(pbr_step(channel, initiator_phase));
                reflector_steps.push(pbr_step(channel, reflector_phase));
            }

            let counter = session.procedure_counter;
            for (index, steps) in [(i, initiator_steps), (peer, reflector_steps)] {
                let config_id = self.controllers[index]
                    .cs
                    .map_or(session.config_id, |s| s.config_id);
                self.controllers[index].outbox.push_back(cs_subevent_result(
                    session.handle,
                    config_id,
                    counter,
                    reference_power,
                    &steps,
                ));
                if let Some(peer_session) = self.controllers[index].cs.as_mut() {
                    peer_session.procedure_counter = counter.wrapping_add(1);
                }
            }
        }
    }

    // --- broadcast (periodic advertising + BIG) ----------------------------
    //
    // What follows models the *sequencing* of an Auracast broadcast, and only
    // that. There is no radio in it: a periodic train is found the moment a
    // receiver looks for it, every report arrives intact, BIS handles are
    // whatever the handle counter says next, and an SDU written by the source
    // reaches every synchronized receiver in the same tick, in order, always.
    //
    // What it does model is the part a state machine can get wrong: which
    // command is legal when, which event answers which command, what the peer
    // is told when a BIG appears or goes away, and the fact that a broadcast
    // has no back-channel — a receiver's controller learns everything from the
    // train and tells the source nothing. The BIGInfo a receiver reads is
    // derived from the source's own LE Create BIG parameters, so a broadcaster
    // that fills that command in wrongly is contradicted by its own receiver
    // rather than agreed with.
    //
    // Not modelled: ISO data paths (an SDU is delivered whether or not
    // LE Setup ISO Data Path was issued), periodic advertising intervals,
    // sync timeouts, encryption (the broadcast code is compared, never used to
    // encrypt), and fragmented advertising data.

    /// Delivers one extended advertising report per enabled advertising set to
    /// every extended-scanning controller.
    fn tick_extended_advertising(&mut self) {
        let advertisers: Vec<(Address, u8, Vec<u8>, Position)> = self
            .controllers
            .iter()
            .flat_map(|c| {
                c.ext_adv_sets
                    .iter()
                    .filter(|s| s.enabled)
                    .map(|s| (c.address, s.advertising_sid, s.data.clone(), c.position))
                    .collect::<Vec<_>>()
            })
            .collect();
        if advertisers.is_empty() {
            return;
        }
        let path_loss = self.path_loss;
        for i in 0..self.controllers.len() {
            if !self.controllers[i].ext_scanning {
                continue;
            }
            let (scanner_address, scanner_position) =
                (self.controllers[i].address, self.controllers[i].position);
            for (address, sid, data, position) in &advertisers {
                if *address == scanner_address {
                    continue;
                }
                let shadowing = self.rng.normal_scaled(path_loss.shadowing_sigma_db);
                let rssi = quantize_rssi(
                    path_loss.rssi_dbm(position.distance_to(scanner_position), shadowing),
                );
                self.controllers[i]
                    .outbox
                    .push_back(le_extended_advertising_report(*address, *sid, data, rssi));
            }
        }
    }

    /// Joins any controller waiting on an LE Periodic Advertising Create Sync
    /// to the train it named, once that train is on air.
    fn tick_periodic_sync_requests(&mut self) {
        for i in 0..self.controllers.len() {
            let Some(pending) = self.controllers[i].pending_pa_sync else {
                continue;
            };
            let found = self.controllers.iter().enumerate().find_map(|(index, c)| {
                if c.address != pending.address {
                    return None;
                }
                c.ext_adv_sets
                    .iter()
                    .find(|s| s.periodic_enabled && s.advertising_sid == pending.advertising_sid)
                    .map(|s| (index, s.advertising_handle))
            });
            let Some((source, advertising_handle)) = found else {
                // No such train yet. A real controller gives up after
                // Sync_Timeout; nothing here does, so the request simply waits.
                continue;
            };
            let sync_handle = self.alloc_handle();
            self.controllers[i].pending_pa_sync = None;
            self.controllers[i].pa_syncs.push(PaSync {
                sync_handle,
                source,
                advertising_handle,
            });
            let (address, sid) = (pending.address, pending.advertising_sid);
            self.controllers[i]
                .outbox
                .push_back(le_periodic_sync_established(sync_handle, sid, address));
        }
    }

    /// Delivers the periodic train's contents to everyone synchronized to it:
    /// the periodic advertising data the source set, and — when a BIG hangs
    /// off that set — a BIGInfo report derived from the source's LE Create BIG.
    fn tick_periodic_advertising(&mut self) {
        let syncs: Vec<(usize, PaSync)> = self
            .controllers
            .iter()
            .enumerate()
            .flat_map(|(i, c)| c.pa_syncs.iter().map(move |s| (i, *s)))
            .collect();
        for (listener, sync) in syncs {
            let source = &self.controllers[sync.source];
            let Some(set) = source
                .ext_adv_sets
                .iter()
                .find(|s| s.advertising_handle == sync.advertising_handle)
            else {
                continue;
            };
            if !set.periodic_enabled {
                continue;
            }
            let data = set.periodic_data.clone();
            let big_info = source
                .big
                .as_ref()
                .filter(|b| b.advertising_handle == sync.advertising_handle)
                .map(|b| le_big_info_report(sync.sync_handle, b));
            if !data.is_empty() {
                self.controllers[listener]
                    .outbox
                    .push_back(le_periodic_advertising_report(sync.sync_handle, &data));
            }
            if let Some(report) = big_info {
                self.controllers[listener].outbox.push_back(report);
            }
        }
    }

    /// Creates a BIG on `from`: allocates one connection handle per BIS and
    /// answers with LE Create BIG Complete, the only place those handles are
    /// ever announced.
    fn route_create_big(&mut self, from: usize, mut source: BigSource, num_bis: u8) {
        source.bis_handles = (0..num_bis).map(|_| self.alloc_handle()).collect();
        let handles = source.bis_handles.clone();
        let big_handle = source.big_handle;
        self.controllers[from].big = Some(source);
        self.controllers[from]
            .outbox
            .push_back(le_create_big_complete(STATUS_SUCCESS, big_handle, &handles));
    }

    /// Tears down `from`'s BIG: the source's host gets LE Terminate BIG
    /// Complete, and every receiver synchronized to it gets LE BIG Sync Lost
    /// with Remote User Terminated Connection.
    ///
    /// This is the whole asymmetry of broadcast in one method. The source is
    /// not told who was listening — it has no way to know — and the receivers
    /// are not asked, they are simply informed, out of nowhere, by their own
    /// controllers.
    fn route_terminate_big(&mut self, from: usize, big_handle: u8) {
        self.controllers[from].big = None;
        self.controllers[from]
            .outbox
            .push_back(le_terminate_big_complete(big_handle, REASON_LOCAL_HOST));

        for i in 0..self.controllers.len() {
            let lost: Vec<u8> = self.controllers[i]
                .big_sinks
                .iter()
                .filter(|s| s.source == from)
                .map(|s| s.big_handle)
                .collect();
            self.controllers[i].big_sinks.retain(|s| s.source != from);
            for handle in lost {
                self.controllers[i]
                    .outbox
                    .push_back(le_big_sync_lost(handle, REASON_REMOTE_USER));
            }
        }
    }

    /// Joins `from` to the BIG on the periodic train it is synchronized to,
    /// answering with LE BIG Sync Established.
    fn route_big_create_sync(
        &mut self,
        from: usize,
        big_handle: u8,
        sync_handle: u16,
        encryption: u8,
        broadcast_code: [u8; 16],
        indices: &[u8],
    ) {
        let sync = self.controllers[from]
            .pa_syncs
            .iter()
            .find(|s| s.sync_handle == sync_handle)
            .copied();
        let source_big = sync.and_then(|s| {
            self.controllers[s.source]
                .big
                .as_ref()
                .filter(|b| b.advertising_handle == s.advertising_handle)
                .map(|b| (s.source, b.clone()))
        });
        let Some((source, big)) = source_big else {
            self.controllers[from]
                .outbox
                .push_back(le_big_sync_established(
                    STATUS_COMMAND_DISALLOWED,
                    big_handle,
                    &[],
                ));
            return;
        };
        // Every requested index has to exist in the source's BIG, and an
        // encrypted BIG has to be opened with the code it was created with.
        // A receiver that gets either wrong hears nothing; saying so with a
        // status is the difference between a stream that fails and one that
        // silently stays quiet.
        let indices_valid = !indices.is_empty()
            && indices
                .iter()
                .all(|&index| index >= 1 && usize::from(index) <= big.bis_handles.len());
        let code_valid = encryption == big.encryption
            && (encryption == 0 || broadcast_code == big.broadcast_code);
        if !indices_valid {
            self.controllers[from]
                .outbox
                .push_back(le_big_sync_established(
                    STATUS_INVALID_PARAMETERS,
                    big_handle,
                    &[],
                ));
            return;
        }
        if !code_valid {
            self.controllers[from]
                .outbox
                .push_back(le_big_sync_established(
                    STATUS_CONNECTION_FAILED,
                    big_handle,
                    &[],
                ));
            return;
        }
        let bis_handles: Vec<u16> = (0..indices.len()).map(|_| self.alloc_handle()).collect();
        self.controllers[from].big_sinks.push(BigSink {
            big_handle,
            source,
            indices: indices.to_vec(),
            bis_handles: bis_handles.clone(),
        });
        self.controllers[from]
            .outbox
            .push_back(le_big_sync_established(
                STATUS_SUCCESS,
                big_handle,
                &bis_handles,
            ));
    }

    /// Fans one SDU written on a BIS out to every receiver synchronized to
    /// that BIG, rewriting the handle to the one *that* receiver was given.
    ///
    /// Returns false if the handle is not one of this controller's BIS
    /// handles, so the caller can fall back to connection-oriented routing.
    fn route_bis_iso(&mut self, from: usize, handle: u16, data: &[u8]) -> bool {
        let Some(slot) = self.controllers[from]
            .big
            .as_ref()
            .and_then(|b| b.bis_handles.iter().position(|&h| h == handle))
        else {
            return false;
        };
        // The source's slot `n` is BIS index `n + 1`; a receiver may have
        // joined any subset of the indices, so the delivery handle is looked
        // up by index, not by position.
        let bis_index = (slot + 1) as u8;
        let deliveries: Vec<(usize, u16)> = self
            .controllers
            .iter()
            .enumerate()
            .flat_map(|(i, c)| {
                c.big_sinks
                    .iter()
                    .filter(|s| s.source == from)
                    .filter_map(move |s| {
                        let position = s.indices.iter().position(|&index| index == bis_index)?;
                        Some((i, *s.bis_handles.get(position)?))
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        for (listener, listener_handle) in deliveries {
            let mut packet = vec![h4_type::HCI_ISO_DATA];
            packet.extend_from_slice(data);
            rewrite_iso_handle(&mut packet[1..], listener_handle);
            self.controllers[listener].outbox.push_back(packet);
        }
        true
    }

    /// The peer controller index for `from`'s connection on `handle`, if any.
    fn peer_of(&self, from: usize, handle: u16) -> Option<usize> {
        self.controllers[from]
            .connections
            .iter()
            .find(|c| c.handle == handle)
            .map(|c| c.peer)
    }
}

/// A Bluetooth address as it appears on the wire in HCI (little-endian, LSB
/// first) — [`Address`] stores the big-endian display order.
fn addr_le(address: Address) -> [u8; 6] {
    let mut b = address.to_be_bytes();
    b.reverse();
    b
}

/// Wrap an event body as an H4 event packet: `0x04, code, len, body…`.
fn event_packet(code: u8, body: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(3 + body.len());
    p.push(h4_type::HCI_EVENT);
    p.push(code);
    p.push(body.len() as u8);
    p.extend_from_slice(body);
    p
}

/// Command Complete for `opcode` carrying `return_params` (status first).
fn command_complete(opcode: u16, return_params: &[u8]) -> Vec<u8> {
    let hdr = CommandCompleteHeader {
        num_hci_command_packets: 1,
        opcode: U16::new(opcode),
    };
    let mut body = hdr.as_bytes().to_vec();
    body.extend_from_slice(return_params);
    event_packet(event::COMMAND_COMPLETE, &body)
}

/// The BD_ADDR at the front of a BR/EDR command's parameters. `None` if the
/// host sent too few bytes, so a truncated command names nobody rather than
/// silently naming the all-zero address.
fn classic_address(params: &[u8]) -> Option<Address> {
    let b = params.get(0..6)?;
    Some(Address::new([b[0], b[1], b[2], b[3], b[4], b[5]]))
}

/// Inquiry Result event (Vol 4, Part E, Section 7.7.2) carrying one response
/// per discovered device: BD_ADDR, Page_Scan_Repetition_Mode, two reserved
/// octets, Class_of_Device, Clock_Offset.
fn inquiry_result(devices: &[(Address, [u8; 3])]) -> Vec<u8> {
    let mut body = vec![devices.len() as u8];
    for (address, class_of_device) in devices {
        body.extend_from_slice(&addr_le(*address));
        body.push(0x01); // Page_Scan_Repetition_Mode R1
        body.extend_from_slice(&[0x00, 0x00]); // Reserved
        body.extend_from_slice(class_of_device);
        body.extend_from_slice(&[0x00, 0x00]); // Clock_Offset
    }
    event_packet(event::INQUIRY_RESULT, &body)
}

/// Inquiry Complete event (Vol 4, Part E, Section 7.7.1).
fn inquiry_complete(status: u8) -> Vec<u8> {
    event_packet(event::INQUIRY_COMPLETE, &[status])
}

/// Connection Request event (Vol 4, Part E, Section 7.7.4): a peer is paging
/// us and the host must answer with Accept or Reject Connection Request.
///
/// `link_type` is load-bearing, not decoration. The same event code announces
/// an inbound ACL and an inbound SCO/eSCO, and the two are answered with
/// **different commands** — Accept Connection Request for one, Accept
/// Synchronous Connection Request for the other. A host that ignores the
/// field answers the wrong one and gets silence.
fn connection_request(from: Address, class_of_device: [u8; 3], link_type: u8) -> Vec<u8> {
    let mut body = addr_le(from).to_vec();
    body.extend_from_slice(&class_of_device);
    body.push(link_type);
    event_packet(event::CONNECTION_REQUEST, &body)
}

/// Synchronous Connection Complete event (Vol 4, Part E, Section 7.7.35) —
/// the completion event every SCO/eSCO setup command promises.
///
/// `Transmission_Interval` and `Retransmission_Window` are reported as zero:
/// this controller schedules no reserved slots and runs no retransmission
/// window, so any other number would be an invented air-interface fact. The
/// packet lengths are the 60-octet payload both CVSD over HV3 and mSBC over
/// EV3 actually use, which is a number a host sizes its buffers from.
fn synchronous_connection_complete(
    status: u8,
    handle: u16,
    peer: Address,
    link_type: u8,
    air_mode: u8,
) -> Vec<u8> {
    let mut body = vec![status];
    body.extend_from_slice(&handle.to_le_bytes());
    body.extend_from_slice(&addr_le(peer));
    body.push(link_type);
    body.push(0x00); // Transmission_Interval — no slot scheduling is modelled
    body.push(0x00); // Retransmission_Window — likewise
    body.extend_from_slice(&SCO_PACKET_LENGTH.to_le_bytes()); // Rx_Packet_Length
    body.extend_from_slice(&SCO_PACKET_LENGTH.to_le_bytes()); // Tx_Packet_Length
    body.push(air_mode);
    event_packet(event::SYNCHRONOUS_CONNECTION_COMPLETE, &body)
}

/// The synchronous payload size reported at connection setup, in octets. 60
/// is what both HV3/CVSD and EV3/mSBC carry per packet on real hardware, and
/// it is what an HFP host sizes its audio frames to.
const SCO_PACKET_LENGTH: u16 = 60;

/// The ACL handle, link type and air mode a Setup Synchronous Connection —
/// or, with `enhanced`, an Enhanced Setup Synchronous Connection — asks for.
///
/// `None` when the parameters are too short to read, which is a Command
/// Status carrying Invalid HCI Command Parameters rather than a guess.
///
/// Layouts (Vol 4, Part E, Sections 7.1.26 and 7.1.45), both verified
/// against Zephyr's `bt_hci_cp_setup_sync_conn` and Bumble's command
/// dataclasses — the plain form's Voice_Setting sits *before*
/// Retransmission_Effort and Packet_Type, which is the field order a
/// hand-transcribed table usually gets wrong.
fn parse_synchronous_setup(params: &[u8], enhanced: bool) -> Option<(u16, u8, u8)> {
    let acl_handle = le_u16(params.get(0..2)?, 0);
    let (packet_type, air_mode) = if enhanced {
        // handle(2) tx_bw(4) rx_bw(4) tx_coding_format(5) … packet_type at 56
        let transmit_coding_format = *params.get(10)?;
        let packet_type = le_u16(params.get(56..58)?, 0);
        (
            packet_type,
            air_mode_of_coding_format(transmit_coding_format),
        )
    } else {
        // handle(2) tx_bw(4) rx_bw(4) max_latency(2) voice_setting(2)
        // retransmission_effort(1) packet_type(2)
        let voice_setting = le_u16(params.get(12..14)?, 0);
        let packet_type = le_u16(params.get(15..17)?, 0);
        (packet_type, air_mode_of_voice_setting(voice_setting))
    };
    let link_type = if packet_type & ESCO_PACKET_TYPES != 0 {
        LINK_TYPE_ESCO
    } else {
        LINK_TYPE_SCO
    };
    Some((acl_handle, link_type, air_mode))
}

/// Connection Complete event (Vol 4, Part E, Section 7.7.3), the BR/EDR
/// counterpart of LE Connection Complete.
fn connection_complete(status: u8, handle: u16, peer: Address) -> Vec<u8> {
    let mut body = vec![status];
    body.extend_from_slice(&handle.to_le_bytes());
    body.extend_from_slice(&addr_le(peer));
    body.push(LINK_TYPE_ACL);
    body.push(0x00); // Encryption_Enabled — this controller models no security
    event_packet(event::CONNECTION_COMPLETE, &body)
}

/// Remote Name Request Complete event (Vol 4, Part E, Section 7.7.7): status,
/// the address asked about, and a fixed 248-byte NUL-padded name.
fn remote_name_request_complete(status: u8, peer: Address, name: &[u8; 248]) -> Vec<u8> {
    let mut body = vec![status];
    body.extend_from_slice(&addr_le(peer));
    body.extend_from_slice(name);
    event_packet(event::REMOTE_NAME_REQUEST_COMPLETE, &body)
}

// --- security events -------------------------------------------------------
//
// Every layout below is Vol 4, Part E, Section 7.7, cross-checked against
// Bumble's `bumble/hci.py` event codes rather than transcribed by eye: a
// hand-copied HCI table has already been wrong once in this project.

/// Link Key Request event (7.7.23): the peer's BD_ADDR, and nothing else.
fn link_key_request(peer: Address) -> Vec<u8> {
    event_packet(event::LINK_KEY_REQUEST, &addr_le(peer))
}

/// Link Key Notification event (7.7.24): BD_ADDR, Link_Key(16), Key_Type.
fn link_key_notification(peer: Address, key: &[u8; 16], key_type: u8) -> Vec<u8> {
    let mut body = addr_le(peer).to_vec();
    body.extend_from_slice(key);
    body.push(key_type);
    event_packet(event::LINK_KEY_NOTIFICATION, &body)
}

/// IO Capability Request event (7.7.40): the peer's BD_ADDR.
fn io_capability_request(peer: Address) -> Vec<u8> {
    event_packet(event::IO_CAPABILITY_REQUEST, &addr_le(peer))
}

/// IO Capability Response event (7.7.41): BD_ADDR, IO_Capability,
/// OOB_Data_Present, Authentication_Requirements — the peer's answer,
/// forwarded verbatim.
fn io_capability_response(peer: Address, capabilities: IoCapabilities) -> Vec<u8> {
    let mut body = addr_le(peer).to_vec();
    body.push(capabilities.io);
    body.push(capabilities.oob);
    body.push(capabilities.auth);
    event_packet(event::IO_CAPABILITY_RESPONSE, &body)
}

/// User Confirmation Request event (7.7.42): BD_ADDR and the six-digit value,
/// little-endian in four octets.
fn user_confirmation_request(peer: Address, numeric_value: u32) -> Vec<u8> {
    let mut body = addr_le(peer).to_vec();
    body.extend_from_slice(&numeric_value.to_le_bytes());
    event_packet(event::USER_CONFIRMATION_REQUEST, &body)
}

/// User Passkey Request event (7.7.43): the peer's BD_ADDR.
fn user_passkey_request(peer: Address) -> Vec<u8> {
    event_packet(event::USER_PASSKEY_REQUEST, &addr_le(peer))
}

/// User Passkey Notification event (7.7.48): BD_ADDR and the passkey to show.
fn user_passkey_notification(peer: Address, passkey: u32) -> Vec<u8> {
    let mut body = addr_le(peer).to_vec();
    body.extend_from_slice(&passkey.to_le_bytes());
    event_packet(event::USER_PASSKEY_NOTIFICATION, &body)
}

/// Simple Pairing Complete event (7.7.45): status, then the peer's BD_ADDR.
fn simple_pairing_complete(status: u8, peer: Address) -> Vec<u8> {
    let mut body = vec![status];
    body.extend_from_slice(&addr_le(peer));
    event_packet(event::SIMPLE_PAIRING_COMPLETE, &body)
}

/// Authentication Complete event (7.7.6): status and connection handle.
fn authentication_complete(status: u8, handle: u16) -> Vec<u8> {
    let mut body = vec![status];
    body.extend_from_slice(&handle.to_le_bytes());
    event_packet(event::AUTHENTICATION_COMPLETE, &body)
}

/// Encryption Change event (7.7.8): status, handle, Encryption_Enabled. The
/// same event answers BR/EDR's Set Connection Encryption and LE's Enable
/// Encryption — one of the few places the two transports share a completion.
fn encryption_change(status: u8, handle: u16, enabled: u8) -> Vec<u8> {
    let mut body = vec![status];
    body.extend_from_slice(&handle.to_le_bytes());
    body.push(enabled);
    event_packet(event::ENCRYPTION_CHANGE, &body)
}

/// Change Connection Link Key Complete event (7.7.9): status and handle.
fn change_connection_link_key_complete(status: u8, handle: u16) -> Vec<u8> {
    let mut body = vec![status];
    body.extend_from_slice(&handle.to_le_bytes());
    event_packet(event::CHANGE_CONNECTION_LINK_KEY_COMPLETE, &body)
}

/// LE Long Term Key Request subevent (7.7.65.5): handle, Random_Number(8),
/// Encrypted_Diversifier(2).
fn le_long_term_key_request(handle: u16, rand: [u8; 8], ediv: u16) -> Vec<u8> {
    let mut body = vec![event::LE_LONG_TERM_KEY_REQUEST];
    body.extend_from_slice(&handle.to_le_bytes());
    body.extend_from_slice(&rand);
    body.extend_from_slice(&ediv.to_le_bytes());
    event_packet(event::LE_META, &body)
}

/// Command Status for `opcode` with `status`.
fn command_status(status: u8, opcode: u16) -> Vec<u8> {
    let body = CommandStatusBody {
        status,
        num_hci_command_packets: 1,
        opcode: U16::new(opcode),
    };
    event_packet(event::COMMAND_STATUS, body.as_bytes())
}

/// LE Connection Complete subevent for the given handle, role, and peer.
fn le_connection_complete(handle: u16, role: u8, peer: Address) -> Vec<u8> {
    let body = LeConnectionCompleteBody {
        subevent_code: event::LE_CONNECTION_COMPLETE,
        status: STATUS_SUCCESS,
        connection_handle: U16::new(handle),
        role,
        peer_address_type: 0x00, // public
        peer_address: addr_le(peer),
        connection_interval: U16::new(DEFAULT_CONN_INTERVAL),
        peripheral_latency: U16::new(0),
        supervision_timeout: U16::new(DEFAULT_SUPERVISION_TIMEOUT),
        central_clock_accuracy: 0x00,
    };
    event_packet(event::LE_META, body.as_bytes())
}

/// LE Connection Update Complete subevent (Vol 4, Part E, Section 7.7.65.3) —
/// the event LE Connection Update's Command Status promises, carrying what the
/// connection's parameters now *are*.
fn le_connection_update_complete(
    handle: u16,
    interval: u16,
    latency: u16,
    timeout: u16,
) -> Vec<u8> {
    let mut body = vec![event::LE_CONNECTION_UPDATE_COMPLETE, STATUS_SUCCESS];
    body.extend_from_slice(&handle.to_le_bytes());
    body.extend_from_slice(&interval.to_le_bytes());
    body.extend_from_slice(&latency.to_le_bytes());
    body.extend_from_slice(&timeout.to_le_bytes());
    event_packet(event::LE_META, &body)
}

/// LE PHY Update Complete subevent (Vol 4, Part E, Section 7.7.65.12). Sent
/// even when neither PHY changed — the spec is explicit about that, and a host
/// that asked for the PHY it already had would otherwise wait forever.
fn le_phy_update_complete(handle: u16, tx_phy: u8, rx_phy: u8) -> Vec<u8> {
    let mut body = vec![event::LE_PHY_UPDATE_COMPLETE, STATUS_SUCCESS];
    body.extend_from_slice(&handle.to_le_bytes());
    body.push(tx_phy);
    body.push(rx_phy);
    event_packet(event::LE_META, &body)
}

/// LE CIS Request subevent (Vol 4, Part E, Section 7.7.65.26) — a central is
/// opening a stream on an existing ACL link.
///
/// CIG_ID and CIS_ID are reported as 0: this controller does not model
/// isochronous groups, so there is only ever one group and one stream in it,
/// and inventing identifiers a host could not have chosen would be worse than
/// naming the only ones there are.
fn le_cis_request(acl_handle: u16, cis_handle: u16) -> Vec<u8> {
    let mut body = vec![event::LE_CIS_REQUEST];
    body.extend_from_slice(&acl_handle.to_le_bytes());
    body.extend_from_slice(&cis_handle.to_le_bytes());
    body.push(0x00); // CIG_ID
    body.push(0x00); // CIS_ID
    event_packet(event::LE_META, &body)
}

/// LE CIS Established \[v1\] subevent (Vol 4, Part E, Section 7.7.65.25).
///
/// The timing parameters are stated, not simulated: sync delays and transport
/// latencies are the values a 10 ms ISO interval would plausibly produce, and
/// no part of this controller schedules against them. What is real is the
/// handle and the status — which is all a host's state machine advances on.
fn le_cis_established(status: u8, cis_handle: u16) -> Vec<u8> {
    let mut body = vec![event::LE_CIS_ESTABLISHED, status];
    body.extend_from_slice(&cis_handle.to_le_bytes());
    body.extend_from_slice(&[0x40, 0x9C, 0x00]); // CIG_Sync_Delay, 40 ms
    body.extend_from_slice(&[0x40, 0x9C, 0x00]); // CIS_Sync_Delay
    body.extend_from_slice(&[0x40, 0x9C, 0x00]); // Transport_Latency_C_To_P
    body.extend_from_slice(&[0x40, 0x9C, 0x00]); // Transport_Latency_P_To_C
    body.push(le_phy::LE_2M); // PHY_C_To_P
    body.push(le_phy::LE_2M); // PHY_P_To_C
    body.push(1); // NSE
    body.push(1); // BN_C_To_P
    body.push(1); // BN_P_To_C
    body.push(1); // FT_C_To_P
    body.push(1); // FT_P_To_C
    body.extend_from_slice(&100u16.to_le_bytes()); // Max_PDU_C_To_P
    body.extend_from_slice(&100u16.to_le_bytes()); // Max_PDU_P_To_C
    body.extend_from_slice(&8u16.to_le_bytes()); // ISO_Interval, 10 ms
    event_packet(event::LE_META, &body)
}

/// LE Advertising Report subevent carrying one report for `addr`, stamped
/// with the RSSI the scanner measured.
fn le_advertising_report(
    event_type: u8,
    addr_type: u8,
    addr: Address,
    data: &[u8],
    rssi_dbm: i8,
) -> Vec<u8> {
    let hdr = LeAdvertisingReportHeader {
        subevent_code: event::LE_ADVERTISING_REPORT,
        num_reports: 1,
        event_type,
        address_type: addr_type,
        address: addr_le(addr),
        data_length: data.len() as u8,
    };
    let mut body = hdr.as_bytes().to_vec();
    body.extend_from_slice(data);
    body.push(rssi_dbm as u8);
    event_packet(event::LE_META, &body)
}

// --- broadcast events ------------------------------------------------------

/// LE Extended Advertising Report carrying one non-connectable report, which
/// is the only shape a broadcast source advertises in.
fn le_extended_advertising_report(
    address: Address,
    advertising_sid: u8,
    data: &[u8],
    rssi_dbm: i8,
) -> Vec<u8> {
    let header = ExtendedAdvertisingReportHeader {
        // No event-type bits: not connectable, not scannable, not a legacy
        // PDU, data complete.
        event_type: U16::new(0),
        address_type: 0x00, // public
        address: addr_le(address),
        primary_phy: adv_phy::LE_1M,
        secondary_phy: adv_phy::LE_2M,
        advertising_sid,
        tx_power: 0x7F, // not available
        rssi: rssi_dbm,
        periodic_advertising_interval: U16::new(0),
        direct_address_type: 0x00,
        direct_address: [0; 6],
        data_length: data.len() as u8,
    };
    let mut body = vec![ext_adv_subevent_code::LE_EXTENDED_ADVERTISING_REPORT];
    body.extend_from_slice(&LeExtendedAdvertisingReportEvent::serialize(&[(
        header, data,
    )]));
    event_packet(event::LE_META, &body)
}

/// LE Periodic Advertising Sync Established, the event that hands a receiver
/// the sync handle everything else about a broadcast is addressed by.
fn le_periodic_sync_established(
    sync_handle: u16,
    advertising_sid: u8,
    address: Address,
) -> Vec<u8> {
    let body = LePeriodicAdvertisingSyncEstablishedEvent {
        status: STATUS_SUCCESS,
        sync_handle: U16::new(sync_handle),
        advertising_sid,
        advertiser_address_type: 0x00,
        advertiser_address: addr_le(address),
        advertiser_phy: adv_phy::LE_2M,
        periodic_advertising_interval: U16::new(0),
        advertiser_clock_accuracy: 0x00,
    };
    let mut packet_body = vec![ext_adv_subevent_code::LE_PERIODIC_ADVERTISING_SYNC_ESTABLISHED];
    packet_body.extend_from_slice(body.as_bytes());
    event_packet(event::LE_META, &packet_body)
}

/// LE Periodic Advertising Report carrying the whole train payload in one
/// complete report — nothing here fragments.
fn le_periodic_advertising_report(sync_handle: u16, data: &[u8]) -> Vec<u8> {
    let mut body = vec![ext_adv_subevent_code::LE_PERIODIC_ADVERTISING_REPORT];
    body.extend_from_slice(&LePeriodicAdvertisingReportEventHeader::serialize(
        sync_handle,
        0x7F, // TX power not available
        0x7F, // RSSI not available
        0xFF, // no CTE
        0x00, // data status: complete
        data,
    ));
    event_packet(event::LE_META, &body)
}

/// LE BIGInfo Advertising Report, derived from the source's own LE Create BIG.
///
/// Every field a receiver acts on — the BIS count and the encryption flag
/// above all — is read back out of what the broadcaster's host asked for,
/// never chosen here. The scheduling fields (`nse`, `bn`, `pto`, `irc`,
/// `max_pdu`, `iso_interval`) *are* chosen here, because nothing schedules
/// anything: they are plausible constants, not a plan the radio will follow.
fn le_big_info_report(sync_handle: u16, big: &BigSource) -> Vec<u8> {
    let report = LeBigInfoAdvertisingReportEvent {
        sync_handle: U16::new(sync_handle),
        num_bis: big.bis_handles.len() as u8,
        nse: 3,
        iso_interval: U16::new(8),
        bn: 1,
        pto: 0,
        irc: 2,
        max_pdu: U16::new(big.max_sdu),
        sdu_interval: ExtU24::new(big.sdu_interval_us),
        max_sdu: U16::new(big.max_sdu),
        phy: big.phy,
        framing: big.framing,
        encryption: big.encryption,
    };
    let mut body = vec![big_subevent_code::LE_BIGINFO_ADVERTISING_REPORT];
    body.extend_from_slice(report.as_bytes());
    event_packet(event::LE_META, &body)
}

/// LE Create BIG Complete, which is the only announcement of the BIS
/// connection handles a source may write SDUs on.
fn le_create_big_complete(status: u8, big_handle: u8, handles: &[u16]) -> Vec<u8> {
    let mut body = vec![big_subevent_code::LE_CREATE_BIG_COMPLETE];
    body.extend_from_slice(&LeCreateBigCompleteEventHeader::serialize(
        status,
        big_handle,
        0x0186A0,
        0x0124F8,
        adv_phy::LE_2M,
        3,
        1,
        0,
        2,
        100,
        8,
        handles,
    ));
    event_packet(event::LE_META, &body)
}

/// LE Terminate BIG Complete — the source's own confirmation, echoing the
/// reason it gave.
fn le_terminate_big_complete(big_handle: u8, reason: u8) -> Vec<u8> {
    let body = LeTerminateBigCompleteEvent { big_handle, reason };
    let mut packet_body = vec![big_subevent_code::LE_TERMINATE_BIG_COMPLETE];
    packet_body.extend_from_slice(body.as_bytes());
    event_packet(event::LE_META, &packet_body)
}

/// LE BIG Sync Established, carrying the receiver's own BIS handles.
fn le_big_sync_established(status: u8, big_handle: u8, handles: &[u16]) -> Vec<u8> {
    let mut body = vec![big_subevent_code::LE_BIG_SYNC_ESTABLISHED];
    body.extend_from_slice(&LeBigSyncEstablishedEventHeader::serialize(
        status, big_handle, 0x0124F8, 3, 1, 0, 2, 100, 8, handles,
    ));
    event_packet(event::LE_META, &body)
}

/// LE BIG Sync Lost — what a receiver is told when the stream ends without it
/// asking. A receiver that left the BIG itself gets a Command Complete
/// instead, and no event at all.
fn le_big_sync_lost(big_handle: u8, reason: u8) -> Vec<u8> {
    let body = LeBigSyncLostEvent { big_handle, reason };
    let mut packet_body = vec![big_subevent_code::LE_BIG_SYNC_LOST];
    packet_body.extend_from_slice(body.as_bytes());
    event_packet(event::LE_META, &packet_body)
}

/// Replaces the connection handle in an HCI ISO packet body (handle+flags,
/// then length, then payload), keeping the packet-boundary flags.
fn rewrite_iso_handle(body: &mut [u8], handle: u16) {
    if body.len() < 2 {
        return;
    }
    let flags = u16::from_le_bytes([body[0], body[1]]) & 0xF000;
    let value = (handle & 0x0FFF) | flags;
    body[0..2].copy_from_slice(&value.to_le_bytes());
}

/// Rounds a received power to the signed whole dBm an HCI report carries.
///
/// A controller reports RSSI as one byte, so a demo cannot show more
/// precision than that however finely the model computes it. Clamped to the
/// range HCI defines as meaningful (Vol 4, Part E, Section 7.7.65.2); 127
/// means "not available" and is never produced here.
fn quantize_rssi(dbm: f64) -> i8 {
    dbm.round().clamp(-127.0, 20.0) as i8
}

/// Reads a little-endian u16 at `offset`, or 0 if the parameters are short.
fn le_u16(params: &[u8], offset: usize) -> u16 {
    params
        .get(offset..offset + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .unwrap_or(0)
}

/// The Action field of LE CS Config Complete (Vol 4, Part E, 7.7.65.47).
mod cs_action {
    /// The configuration was created.
    pub const CREATED: u8 = 0x01;
    /// The configuration was removed.
    pub const REMOVED: u8 = 0x00;
}

/// LE CS Config Complete (Vol 4, Part E, Section 7.7.65.47).
///
/// `action` is 0x01 for a configuration that was created and 0x00 for one
/// that was removed. Both are the same event; only this byte differs.
fn cs_config_complete(handle: u16, config_id: u8, role: u8, action: u8) -> Vec<u8> {
    let body = crate::packets::hci::LeCsConfigCompleteEvent {
        status: STATUS_SUCCESS,
        connection_handle: U16::new(handle),
        config_id,
        action,
        main_mode_type: cs_plan::STEP_MODE_PBR,
        sub_mode_type: 0xFF, // unused
        min_main_mode_steps: cs_plan::TONES_PER_SUBEVENT as u8,
        max_main_mode_steps: cs_plan::TONES_PER_SUBEVENT as u8,
        main_mode_repetition: 0,
        mode_0_steps: 0,
        role,
        rtt_type: 0x00,
        cs_sync_phy: 0x01, // LE 1M
        channel_map: cs_channel_map(),
        channel_map_repetition: 1,
        channel_selection_type: 0x00,
        ch3c_shape: 0x00,
        ch3c_jump: 0x00,
        companion_signal_status: 0x00,
    };
    let mut packet_body = vec![event::LE_CS_CONFIG_COMPLETE];
    packet_body.extend_from_slice(body.as_bytes());
    event_packet(event::LE_META, &packet_body)
}

/// The 79-bit channel map, as a bitmask over channel indices, marking the
/// channels [`cs_plan::tone_channels`] places tones on.
fn cs_channel_map() -> [u8; 10] {
    let mut map = [0u8; 10];
    for channel in cs_plan::tone_channels() {
        map[channel as usize / 8] |= 1 << (channel % 8);
    }
    map
}

/// LE CS Procedure Enable Complete (Vol 4, Part E, Section 7.7.65.48).
fn cs_procedure_enable_complete(
    status: u8,
    handle: u16,
    config_id: u8,
    enabled: bool,
    tx_power_dbm: f64,
) -> Vec<u8> {
    let body = LeCsProcedureEnableCompleteBody {
        status,
        connection_handle: U16::new(handle),
        config_id,
        state: u8::from(enabled),
        tone_antenna_config_selection: 0x00, // 1:1
        selected_tx_power: quantize_rssi(tx_power_dbm),
        subevent_len: [0x40, 0x0D, 0x00], // 3392 µs
        subevents_per_event: 1,
        subevent_interval: U16::new(0),
        event_interval: U16::new(1),
        procedure_interval: U16::new(1),
        procedure_count: U16::new(0), // repeat until disabled
        max_procedure_len: U16::new(0x0040),
    };
    let mut packet_body = vec![event::LE_CS_PROCEDURE_ENABLE_COMPLETE];
    packet_body.extend_from_slice(body.as_bytes());
    event_packet(event::LE_META, &packet_body)
}

/// One mode-2 (Phase-Based Ranging) step, as it appears in a subevent
/// result: step mode, step channel, step data length, then the step data.
///
/// Mode-2 step data is an antenna permutation index followed by one
/// `Tone_PCT` + `Tone_Quality_Indicator` pair per antenna path *plus one*
/// for the extension slot (Vol 4, Part E, Section 7.7.65.44). The PCT is a
/// 24-bit value: 12-bit signed I in the low bits, 12-bit signed Q above.
fn pbr_step(channel: u8, phase_rad: f64) -> Vec<u8> {
    let pct = pct_bytes(phase_rad);
    let mut step_data = vec![0x00]; // antenna permutation index
    for _ in 0..=cs_plan::NUM_ANTENNA_PATHS {
        step_data.extend_from_slice(&pct);
        step_data.push(cs_plan::TONE_QUALITY_HIGH);
    }
    let mut step = vec![cs_plan::STEP_MODE_PBR, channel, step_data.len() as u8];
    step.extend_from_slice(&step_data);
    step
}

/// Packs a unit-amplitude phasor at `phase_rad` into the three bytes of a
/// Phase Correction Term: I in bits 0–11, Q in bits 12–23, both 12-bit
/// two's-complement.
fn pct_bytes(phase_rad: f64) -> [u8; 3] {
    /// The largest magnitude a 12-bit two's-complement sample can carry.
    const FULL_SCALE: f64 = 2047.0;
    let quantize = |v: f64| ((v * FULL_SCALE).round() as i32).clamp(-2048, 2047) as u32 & 0x0FFF;
    let packed = quantize(phase_rad.cos()) | (quantize(phase_rad.sin()) << 12);
    [packed as u8, (packed >> 8) as u8, (packed >> 16) as u8]
}

/// LE CS Subevent Result (Vol 4, Part E, Section 7.7.65.49) carrying `steps`.
fn cs_subevent_result(
    handle: u16,
    config_id: u8,
    procedure_counter: u16,
    reference_power_level: i8,
    steps: &[Vec<u8>],
) -> Vec<u8> {
    let header = crate::packets::hci::LeCsSubeventResultHeader {
        connection_handle: U16::new(handle),
        config_id,
        start_acl_conn_event: U16::new(procedure_counter),
        procedure_counter: U16::new(procedure_counter),
        // No crystal-offset compensation is modelled; 0xFFFF is the spec's
        // "not available".
        frequency_compensation: U16::new(0xFFFF),
        reference_power_level,
        procedure_done_status: cs_plan::DONE_STATUS_COMPLETE,
        subevent_done_status: cs_plan::DONE_STATUS_COMPLETE,
        procedure_abort_reason: 0x00,
        subevent_abort_reason: 0x00,
        num_antenna_paths: cs_plan::NUM_ANTENNA_PATHS,
        num_steps_reported: steps.len() as u8,
    };
    let mut body = vec![event::LE_CS_SUBEVENT_RESULT];
    body.extend_from_slice(header.as_bytes());
    for step in steps {
        body.extend_from_slice(step);
    }
    debug_assert!(
        body.len() <= 255,
        "an HCI event carries at most 255 parameter bytes; got {}",
        body.len()
    );
    event_packet(event::LE_META, &body)
}

/// Disconnection Complete event for `handle` with the given reason.
fn disconnection_complete(handle: u16, reason: u8) -> Vec<u8> {
    let body = DisconnectionCompleteBody {
        status: STATUS_SUCCESS,
        connection_handle: U16::new(handle),
        reason,
    };
    event_packet(event::DISCONNECTION_COMPLETE, body.as_bytes())
}

#[cfg(test)]
#[path = "sim_tests.rs"]
mod tests;
