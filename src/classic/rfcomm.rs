// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! RFCOMM (serial port emulation over an L2CAP Classic channel), ETSI TS
//! 07.10 as adapted by the Bluetooth Core Spec (Vol 3, Part F).
//!
//! Wire format: a 2-byte fixed address/control header (see
//! `RfcommFrameHeader`), a 1- or 2-byte EA-encoded length field, a
//! variable-length information payload, and a trailing 1-byte FCS —
//! variable-length like `sdp::DataElement`, so only the fixed 2-byte prefix
//! is zerocopy and the rest is hand-parsed in [`RfcommFrame::parse`].
//!
//! [`Multiplexer`] is the session state machine over one Classic L2CAP
//! channel (DLCI 0 is its control channel); it owns zero or more [`Dlc`]s.
//! This is a plain synchronous state machine: [`Multiplexer::receive`] takes
//! one incoming frame and returns `(outgoing frame bytes, events)`, the same
//! style as `lmp::LmpLink::receive`.
//!
//! Credit-based flow control (RFCOMM-level, distinct from and layered above
//! L2CAP's own) is negotiated per session by the PN convergence-layer field
//! and tracked in [`CreditFlowControl`]. It is not assumed: a peer that asks
//! for the pre-1.1 convergence layer, or that opens a DLC by bare SABM with
//! no PN at all, gets frames with no credit octet.
//!
//! PN (Parameter Negotiation) and MSC (Modem Status Command) multiplexer
//! control commands are implemented. RPN (Remote Port Negotiation) is not:
//! it negotiates serial port line settings (baud rate, parity, flow control
//! lines) that this simulator has no use for, and real peers treat it as
//! optional, so incoming RPN is silently ignored rather than answered.

use crate::classic::sdp::{
    AttributeIdSpec, DataElement, SdpClient, SdpUuid, ServiceAttribute, attribute_id,
};
use crate::l2cap::classic::{ClassicChannel, ClassicChannelManager, ClassicChannelSpec};
use crate::packets::l2cap_signaling::ConnectionRequestHeader;
use crate::types::SimbleError;
use std::collections::HashMap;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Ref, Unaligned};

/// Well-known PSM for RFCOMM (Bluetooth Assigned Numbers).
pub const RFCOMM_PSM: u16 = 0x0003;
/// Default L2CAP MTU used for the RFCOMM channel.
pub const DEFAULT_L2CAP_MTU: u16 = 2048;
/// Default initial RFCOMM credit grant per DLC.
pub const DEFAULT_INITIAL_CREDITS: u8 = 7;
/// Maximum RFCOMM credits held per DLC.
pub(crate) const DEFAULT_MAX_CREDITS: u8 = 32;
/// Credit low-water mark that triggers a credit top-up.
pub(crate) const DEFAULT_CREDIT_THRESHOLD: u8 = DEFAULT_MAX_CREDITS / 2;
/// Default negotiated maximum RFCOMM frame size.
pub const DEFAULT_MAX_FRAME_SIZE: u16 = 1000;
/// The frame size a DLC opened without PN must assume for the peer: N1's
/// default value (RFCOMM 1.1 5.5.3, TS 07.10 5.7.2). PN is optional before
/// SABM, so a peer that skips it has agreed to nothing but this.
pub const DEFAULT_FRAME_SIZE_WITHOUT_PN: u16 = 127;
/// Lowest assignable RFCOMM server channel number.
pub const DYNAMIC_CHANNEL_NUMBER_START: u8 = 1;
/// Highest assignable RFCOMM server channel number.
pub const DYNAMIC_CHANNEL_NUMBER_END: u8 = 30;

/// Control-field frame types (low 5/high-minus-P/F bits; bit 4 is the P/F
/// bit and is masked out of these constants).
pub mod frame_type {
    /// SABM: sets up a DLC (or the multiplexer on DLCI 0).
    pub const SABM: u8 = 0x2F;
    /// UA: unnumbered acknowledgement of SABM/DISC.
    pub const UA: u8 = 0x63;
    /// DM: disconnected mode (rejects a connection).
    pub const DM: u8 = 0x0F;
    /// DISC: tears down a DLC or the multiplexer.
    pub const DISC: u8 = 0x43;
    /// UIH: unnumbered information with header check (data and MCC frames).
    pub const UIH: u8 = 0xEF;
    /// UI: unnumbered information (unused here).
    pub const UI: u8 = 0x03;
}

/// Multiplexer Control Channel (MCC) command types (ETSI TS 07.10, 5.4.6.3).
/// These are the 6-bit type values before the `<< 2 | C/R | EA` shift that
/// `make_mcc` applies, matching how Bumble and Zephyr spell them.
pub(crate) mod mcc_type {
    /// PN: Parameter Negotiation command.
    pub const PN: u8 = 0x20;
    /// MSC: Modem Status Command.
    pub const MSC: u8 = 0x38;
}

/// PN convergence-layer (CL) field values that negotiate credit-based flow
/// control, RFCOMM 1.1 5.5.3. A command asks with `CFC_COMMAND` and the
/// responder agrees with `CFC_RESPONSE`; any other value means the peer wants
/// the pre-1.1 (no credit octet) convergence layer, and answering `0xE0`
/// anyway would put our credit octets into its application byte stream.
pub(crate) mod pn_cl {
    /// Command asking for credit-based flow control.
    pub const CFC_COMMAND: u8 = 0xF0;
    /// Response accepting credit-based flow control.
    pub const CFC_RESPONSE: u8 = 0xE0;
    /// Neither: convergence layer 1, no credit octets.
    pub const NO_CFC: u8 = 0x00;
}

// ---------------------------------------------------------------------------
// FCS (Frame Check Sequence)
// ---------------------------------------------------------------------------

/// Reversed CRC-8 lookup table, ETSI TS 07.10 Annex B (polynomial x^8 + x^2 +
/// x + 1, i.e. 0x07, reflected).
#[rustfmt::skip]
const FCS_TABLE: [u8; 256] = [
    0x00, 0x91, 0xE3, 0x72, 0x07, 0x96, 0xE4, 0x75,
    0x0E, 0x9F, 0xED, 0x7C, 0x09, 0x98, 0xEA, 0x7B,
    0x1C, 0x8D, 0xFF, 0x6E, 0x1B, 0x8A, 0xF8, 0x69,
    0x12, 0x83, 0xF1, 0x60, 0x15, 0x84, 0xF6, 0x67,
    0x38, 0xA9, 0xDB, 0x4A, 0x3F, 0xAE, 0xDC, 0x4D,
    0x36, 0xA7, 0xD5, 0x44, 0x31, 0xA0, 0xD2, 0x43,
    0x24, 0xB5, 0xC7, 0x56, 0x23, 0xB2, 0xC0, 0x51,
    0x2A, 0xBB, 0xC9, 0x58, 0x2D, 0xBC, 0xCE, 0x5F,
    0x70, 0xE1, 0x93, 0x02, 0x77, 0xE6, 0x94, 0x05,
    0x7E, 0xEF, 0x9D, 0x0C, 0x79, 0xE8, 0x9A, 0x0B,
    0x6C, 0xFD, 0x8F, 0x1E, 0x6B, 0xFA, 0x88, 0x19,
    0x62, 0xF3, 0x81, 0x10, 0x65, 0xF4, 0x86, 0x17,
    0x48, 0xD9, 0xAB, 0x3A, 0x4F, 0xDE, 0xAC, 0x3D,
    0x46, 0xD7, 0xA5, 0x34, 0x41, 0xD0, 0xA2, 0x33,
    0x54, 0xC5, 0xB7, 0x26, 0x53, 0xC2, 0xB0, 0x21,
    0x5A, 0xCB, 0xB9, 0x28, 0x5D, 0xCC, 0xBE, 0x2F,
    0xE0, 0x71, 0x03, 0x92, 0xE7, 0x76, 0x04, 0x95,
    0xEE, 0x7F, 0x0D, 0x9C, 0xE9, 0x78, 0x0A, 0x9B,
    0xFC, 0x6D, 0x1F, 0x8E, 0xFB, 0x6A, 0x18, 0x89,
    0xF2, 0x63, 0x11, 0x80, 0xF5, 0x64, 0x16, 0x87,
    0xD8, 0x49, 0x3B, 0xAA, 0xDF, 0x4E, 0x3C, 0xAD,
    0xD6, 0x47, 0x35, 0xA4, 0xD1, 0x40, 0x32, 0xA3,
    0xC4, 0x55, 0x27, 0xB6, 0xC3, 0x52, 0x20, 0xB1,
    0xCA, 0x5B, 0x29, 0xB8, 0xCD, 0x5C, 0x2E, 0xBF,
    0x90, 0x01, 0x73, 0xE2, 0x97, 0x06, 0x74, 0xE5,
    0x9E, 0x0F, 0x7D, 0xEC, 0x99, 0x08, 0x7A, 0xEB,
    0x8C, 0x1D, 0x6F, 0xFE, 0x8B, 0x1A, 0x68, 0xF9,
    0x82, 0x13, 0x61, 0xF0, 0x85, 0x14, 0x66, 0xF7,
    0xA8, 0x39, 0x4B, 0xDA, 0xAF, 0x3E, 0x4C, 0xDD,
    0xA6, 0x37, 0x45, 0xD4, 0xA1, 0x30, 0x42, 0xD3,
    0xB4, 0x25, 0x57, 0xC6, 0xB3, 0x22, 0x50, 0xC1,
    0xBA, 0x2B, 0x59, 0xC8, 0xBD, 0x2C, 0x5E, 0xCF,
];

fn compute_fcs(buffer: &[u8]) -> u8 {
    let mut result: u8 = 0xFF;
    for &byte in buffer {
        result = FCS_TABLE[(result ^ byte) as usize];
    }
    0xFF - result
}

// ---------------------------------------------------------------------------
// Frame
// ---------------------------------------------------------------------------

/// The fixed 2-byte address/control prefix of every RFCOMM frame.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub(crate) struct RfcommFrameHeader {
    /// Address octet: DLCI, C/R, and EA bits.
    pub address: u8,
    /// Control octet: frame type and P/F bit.
    pub control: u8,
}

/// One RFCOMM frame (ETSI TS 07.10, 5.2). `with_credits` handles the case
/// where a UIH frame carries a leading rx-credit octet (P/F=1): that octet
/// counts in `information` but is excluded from the encoded length field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RfcommFrame {
    /// Frame type (`frame_type::*`), with the P/F bit masked out.
    pub(crate) frame_type: u8,
    /// Command/response bit.
    pub c_r: u8,
    /// DLCI (data link connection identifier).
    pub dlci: u8,
    /// Poll/Final bit.
    pub p_f: u8,
    /// Information payload (includes a leading credit octet on credit-bearing UIH frames).
    pub information: Vec<u8>,
    with_credits: bool,
}

impl RfcommFrame {
    fn new(
        frame_type: u8,
        c_r: u8,
        dlci: u8,
        p_f: u8,
        information: Vec<u8>,
        with_credits: bool,
    ) -> Self {
        Self {
            frame_type,
            c_r,
            dlci,
            p_f,
            information,
            with_credits,
        }
    }

    /// Builds a SABM frame.
    pub fn sabm(c_r: u8, dlci: u8) -> Self {
        Self::new(frame_type::SABM, c_r, dlci, 1, Vec::new(), false)
    }

    /// Builds a UA (unnumbered acknowledgement) frame.
    pub(crate) fn ua(c_r: u8, dlci: u8) -> Self {
        Self::new(frame_type::UA, c_r, dlci, 1, Vec::new(), false)
    }

    /// Builds a DM (disconnected mode) frame.
    pub(crate) fn dm(c_r: u8, dlci: u8) -> Self {
        Self::new(frame_type::DM, c_r, dlci, 1, Vec::new(), false)
    }

    /// Builds a DISC frame.
    pub fn disc(c_r: u8, dlci: u8) -> Self {
        Self::new(frame_type::DISC, c_r, dlci, 1, Vec::new(), false)
    }

    /// Builds a UIH frame; `p_f = 1` marks a leading credit octet.
    pub fn uih(c_r: u8, dlci: u8, information: Vec<u8>, p_f: u8) -> Self {
        Self::new(frame_type::UIH, c_r, dlci, p_f, information, p_f == 1)
    }

    fn address_byte(&self) -> u8 {
        (self.dlci << 2) | (self.c_r << 1) | 1
    }

    fn control_byte(&self) -> u8 {
        self.frame_type | (self.p_f << 4)
    }

    fn length_value(&self) -> usize {
        let len = self.information.len();
        if self.with_credits {
            len.saturating_sub(1)
        } else {
            len
        }
    }

    /// Encodes the length field without allocating: returns a 2-byte buffer
    /// plus how many of its bytes are used (1 or 2).
    fn length_bytes(&self) -> ([u8; 2], usize) {
        let len = self.length_value();
        if len > 0x7F {
            ([((len & 0x7F) << 1) as u8, ((len >> 7) & 0xFF) as u8], 2)
        } else {
            ([((len << 1) | 1) as u8, 0], 1)
        }
    }

    fn fcs(&self) -> u8 {
        let address = self.address_byte();
        let control = self.control_byte();
        if self.frame_type == frame_type::UIH {
            compute_fcs(&[address, control])
        } else {
            let (length, length_len) = self.length_bytes();
            let mut buf = [address, control, 0, 0];
            buf[2..2 + length_len].copy_from_slice(&length[..length_len]);
            compute_fcs(&buf[..2 + length_len])
        }
    }

    /// Serializes the full frame: header + length + information + FCS.
    pub fn to_bytes(&self) -> Vec<u8> {
        let (length, length_len) = self.length_bytes();
        let mut out = Vec::with_capacity(2 + length_len + self.information.len() + 1);
        out.push(self.address_byte());
        out.push(self.control_byte());
        out.extend_from_slice(&length[..length_len]);
        out.extend_from_slice(&self.information);
        out.push(self.fcs());
        out
    }

    /// Parses a full frame, verifying the FCS. Returns `None` on a truncated
    /// buffer or an FCS mismatch (corrupt frame).
    pub fn parse(data: &[u8]) -> Option<Self> {
        let (header, rest) = Ref::<&[u8], RfcommFrameHeader>::from_prefix(data).ok()?;
        let dlci = (header.address >> 2) & 0x3F;
        let c_r = (header.address >> 1) & 1;
        let frame_type = header.control & 0xEF;
        let p_f = (header.control >> 4) & 1;

        let length_byte0 = *rest.first()?;
        let information_start = if length_byte0 & 1 != 0 { 1 } else { 2 };
        if rest.len() < information_start + 1 {
            return None;
        }
        let information = rest.get(information_start..rest.len() - 1)?.to_vec();
        let fcs_byte = *rest.last()?;

        let frame = RfcommFrame::new(frame_type, c_r, dlci, p_f, information, false);
        if frame.fcs() != fcs_byte {
            return None;
        }
        Some(frame)
    }
}

// ---------------------------------------------------------------------------
// Multiplexer Control Channel (MCC) sub-frames
// ---------------------------------------------------------------------------

fn parse_mcc(data: &[u8]) -> Option<(u8, bool, &[u8])> {
    let byte0 = *data.first()?;
    let mcc_type = byte0 >> 2;
    let c_r = ((byte0 >> 1) & 1) != 0;
    let len_byte0 = *data.get(1)?;
    if len_byte0 & 1 != 0 {
        let length = (len_byte0 >> 1) as usize;
        Some((mcc_type, c_r, data.get(2..2 + length)?))
    } else {
        let len_byte1 = *data.get(2)?;
        let length = ((len_byte0 >> 1) as usize) | ((len_byte1 as usize) << 7);
        Some((mcc_type, c_r, data.get(3..3 + length)?))
    }
}

/// Always encodes a 1-byte length field: MCC payloads used here (PN, MSC)
/// are 8 and 2 bytes respectively, well under the 127-byte EA limit.
fn make_mcc(mcc_type: u8, c_r: bool, data: &[u8]) -> Vec<u8> {
    let mut out = vec![
        (mcc_type << 2) | ((c_r as u8) << 1) | 1,
        ((data.len() as u8 & 0x7F) << 1) | 1,
    ];
    out.extend_from_slice(data);
    out
}

/// PN (Parameter Negotiation): negotiates a DLC's max frame size and initial
/// credit count before it opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MccPn {
    /// DLCI this negotiation applies to.
    pub dlci: u8,
    /// Convergence layer / frame type field.
    pub cl: u8,
    /// DLC priority.
    pub priority: u8,
    /// Acknowledgement timer (unused; 0).
    pub ack_timer: u8,
    /// Negotiated maximum frame size.
    pub max_frame_size: u16,
    /// Maximum retransmissions (0 for UIH).
    pub max_retransmissions: u8,
    /// Only the low 3 bits are meaningful (credits range [1, 7]).
    pub initial_credits: u8,
}

impl MccPn {
    /// Encodes the 8-byte PN value field.
    pub fn to_bytes(self) -> [u8; 8] {
        [
            self.dlci,
            self.cl,
            self.priority,
            self.ack_timer,
            (self.max_frame_size & 0xFF) as u8,
            ((self.max_frame_size >> 8) & 0xFF) as u8,
            self.max_retransmissions,
            self.initial_credits & 0x07,
        ]
    }

    /// Decodes the 8-byte PN value field.
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 8 {
            return None;
        }
        Some(Self {
            dlci: data[0],
            cl: data[1],
            priority: data[2],
            ack_timer: data[3],
            max_frame_size: (data[4] as u16) | ((data[5] as u16) << 8),
            max_retransmissions: data[6],
            initial_credits: data[7] & 0x07,
        })
    }
}

/// MSC (Modem Status Command): carries V.24 signal state (RTS/CTS/DTR/DSR
/// equivalents) for a DLC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MccMsc {
    /// DLCI this status applies to.
    pub dlci: u8,
    /// Flow-control (legacy aggregate) bit.
    pub fc: bool,
    /// Ready To Communicate (DSR equivalent).
    pub rtc: bool,
    /// Ready To Receive (RTS/CTS equivalent).
    pub rtr: bool,
    /// Incoming Call (RI) indicator.
    pub ic: bool,
    /// Data Valid (DCD equivalent).
    pub dv: bool,
}

impl MccMsc {
    /// Encodes the 2-byte MSC value field.
    pub fn to_bytes(self) -> [u8; 2] {
        [
            (self.dlci << 2) | 3,
            1 | ((self.fc as u8) << 1)
                | ((self.rtc as u8) << 2)
                | ((self.rtr as u8) << 3)
                | ((self.ic as u8) << 6)
                | ((self.dv as u8) << 7),
        ]
    }

    /// Decodes the 2-byte MSC value field.
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 2 {
            return None;
        }
        Some(Self {
            dlci: data[0] >> 2,
            fc: (data[1] >> 1) & 1 != 0,
            rtc: (data[1] >> 2) & 1 != 0,
            rtr: (data[1] >> 3) & 1 != 0,
            ic: (data[1] >> 6) & 1 != 0,
            dv: (data[1] >> 7) & 1 != 0,
        })
    }

    fn default_response(dlci: u8) -> Self {
        Self {
            dlci,
            fc: false,
            rtc: true,
            rtr: true,
            ic: false,
            dv: true,
        }
    }
}

// ---------------------------------------------------------------------------
// DLC (Data Link Connection)
// ---------------------------------------------------------------------------

/// Lifecycle of one DLC, ETSI TS 07.10 5.4.6.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DlcState {
    /// Created, not yet negotiated.
    Init,
    /// SABM sent, awaiting UA.
    Connecting,
    /// Open and carrying data.
    Connected,
    /// DISC sent, awaiting UA.
    Disconnecting,
    /// Closed.
    Disconnected,
}

/// One Data Link Connection within a [`Multiplexer`]: a byte-stream channel
/// with its own RFCOMM-level credit-based flow control, distinct from and
/// layered above L2CAP's own flow control on the underlying Basic Mode channel.
#[derive(Debug, Clone)]
pub struct Dlc {
    /// DLCI of this connection.
    pub dlci: u8,
    /// Current lifecycle state.
    pub state: DlcState,
    c_r: u8,
    /// Maximum frame size we accept.
    pub rx_max_frame_size: u16,
    /// Initial credits we grant the peer.
    pub rx_initial_credits: u8,
    /// Maximum credits we will hold for the peer.
    pub(crate) rx_max_credits: u8,
    /// Credits currently granted to the peer.
    pub(crate) rx_credits: u8,
    /// Low-water mark for topping up peer credits.
    pub(crate) rx_credits_threshold: u8,
    /// Maximum frame size the peer accepts.
    pub tx_max_frame_size: u16,
    /// Credits currently available to us for sending.
    pub(crate) tx_credits: u8,
    /// Whether credit-based flow control was negotiated for this DLC
    /// (RFCOMM 1.1 5.5.2). When false, frames carry no credit octet and
    /// transmission is not credit-gated.
    pub(crate) cfc: bool,
    mtu: usize,
    tx_buffer: Vec<u8>,
}

impl Dlc {
    #[allow(clippy::too_many_arguments)]
    fn new(
        dlci: u8,
        c_r: u8,
        tx_max_frame_size: u16,
        tx_initial_credits: u8,
        rx_max_frame_size: u16,
        rx_initial_credits: u8,
        peer_l2cap_mtu: u16,
        cfc: bool,
    ) -> Self {
        // Header (2) + worst-case 2-byte length + trailing FCS (1).
        const MAX_OVERHEAD: u16 = 5;
        let mtu = tx_max_frame_size.min(peer_l2cap_mtu.saturating_sub(MAX_OVERHEAD)) as usize;
        Self {
            dlci,
            state: DlcState::Init,
            c_r,
            rx_max_frame_size,
            rx_initial_credits,
            rx_max_credits: DEFAULT_MAX_CREDITS,
            rx_credits: rx_initial_credits,
            rx_credits_threshold: DEFAULT_CREDIT_THRESHOLD,
            tx_max_frame_size,
            tx_credits: tx_initial_credits,
            cfc,
            mtu,
            tx_buffer: Vec::new(),
        }
    }

    /// Whether the DLC is in the Connected state.
    pub fn is_open(&self) -> bool {
        self.state == DlcState::Connected
    }

    /// Queues `data` for transmission (does not send yet: call
    /// [`Multiplexer::write`] to both queue and flush).
    fn queue(&mut self, data: &[u8]) {
        self.tx_buffer.extend_from_slice(data);
    }

    fn rx_credits_needed(&self) -> u8 {
        if !self.cfc {
            return 0;
        }
        if self.rx_credits <= self.rx_credits_threshold {
            self.rx_max_credits - self.rx_credits
        } else {
            0
        }
    }

    /// Drains `tx_buffer` into UIH frames respecting `tx_credits` and MTU,
    /// opportunistically piggy-backing an rx-credit refill (P/F=1, leading
    /// credit octet) when `rx_credits` has dropped to the low-water mark.
    fn process_tx(&mut self) -> Vec<RfcommFrame> {
        if !self.cfc {
            // No CFC negotiated: the peer is not expecting a credit octet and
            // grants no credits, so send everything with P/F=0 and no gating
            // (RFCOMM 1.1 5.5.2 — flow control then falls to MSC FC, which
            // this stack does not drive).
            let mut frames = Vec::new();
            while !self.tx_buffer.is_empty() {
                let take = self.mtu.min(self.tx_buffer.len());
                let chunk: Vec<u8> = self.tx_buffer.drain(..take).collect();
                frames.push(RfcommFrame::uih(self.c_r, self.dlci, chunk, 0));
            }
            return frames;
        }
        let mut frames = Vec::new();
        let mut rx_credits_needed = self.rx_credits_needed();
        while (!self.tx_buffer.is_empty() && self.tx_credits > 0) || rx_credits_needed > 0 {
            let (chunk, tx_credit_spent) = if rx_credits_needed > 0 {
                self.rx_credits += rx_credits_needed;
                let mut chunk = vec![rx_credits_needed];
                let spent = if !self.tx_buffer.is_empty() && self.tx_credits > 0 {
                    let take = self.mtu.saturating_sub(1).min(self.tx_buffer.len());
                    chunk.extend(self.tx_buffer.drain(..take));
                    true
                } else {
                    false
                };
                (chunk, spent)
            } else {
                let take = self.mtu.min(self.tx_buffer.len());
                (self.tx_buffer.drain(..take).collect(), true)
            };
            if tx_credit_spent {
                self.tx_credits -= 1;
            }
            let p_f = if rx_credits_needed > 0 { 1 } else { 0 };
            frames.push(RfcommFrame::uih(self.c_r, self.dlci, chunk, p_f));
            rx_credits_needed = 0;
        }
        frames
    }
}

// ---------------------------------------------------------------------------
// Multiplexer
// ---------------------------------------------------------------------------

/// Which side opened the RFCOMM multiplexer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The side that started the multiplexer (SABM on DLCI 0).
    Initiator,
    /// The side that accepted the multiplexer.
    Responder,
}

/// Multiplexer session state, ETSI TS 07.10 5.4.6.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MultiplexerState {
    /// Created, multiplexer not yet started.
    Init,
    /// SABM(0) sent, awaiting UA.
    Connecting,
    /// Multiplexer session established.
    Connected,
    /// A DLC open (PN) is in progress.
    Opening,
    /// DISC(0) sent, awaiting UA.
    Disconnecting,
    /// Multiplexer closed.
    Disconnected,
}

/// Whether credit-based flow control is in use on this session, RFCOMM 1.1
/// 5.5.2. It is a session-wide property settled by the convergence-layer
/// field of the first PN exchange; a session whose DLCs all open by bare
/// SABM never negotiates it and stays without. (Tri-state modelled on
/// Zephyr's `session->cfc`, `subsys/bluetooth/host/classic/rfcomm.c`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreditFlowControl {
    /// No PN has been exchanged yet; still open to either answer.
    Unknown,
    /// Negotiated: frames carry credit octets and sending is credit-gated.
    Supported,
    /// Declined by the peer, or never negotiated: no credit octets.
    NotSupported,
}

/// Result of feeding one incoming frame into [`Multiplexer::receive`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MultiplexerEvent {
    /// The multiplexer session (DLCI 0) is up.
    Started,
    /// The multiplexer session has closed.
    Disconnected,
    /// The DLC at this DLCI finished opening (SABM/UA handshake complete).
    DlcOpened(u8),
    /// A local `open_dlc` was refused by the peer (DM response to PN/SABM).
    DlcOpenRejected(u8),
    /// The DLC at this DLCI closed (DISC/UA exchange complete).
    DlcClosed(u8),
    /// Application data arrived on this DLCI.
    DataReceived(u8, Vec<u8>),
}

/// Reserves `channel` in a listen-config map (shared by `Multiplexer::listen`
/// and `RfcommServer::listen`). `false` if already reserved.
fn reserve_listen_channel(
    listen_configs: &mut HashMap<u8, (u16, u8)>,
    channel: u8,
    max_frame_size: u16,
    initial_credits: u8,
) -> bool {
    if listen_configs.contains_key(&channel) {
        return false;
    }
    listen_configs.insert(channel, (max_frame_size, initial_credits));
    true
}

/// The session-level state machine over one Classic L2CAP channel (PSM
/// [`RFCOMM_PSM`]): manages DLCI 0 (the multiplexer control channel) plus
/// zero or more data [`Dlc`]s. Synchronous: [`Multiplexer::receive`] takes
/// one already-demultiplexed L2CAP SDU and returns the frames to send back
/// plus any events for the caller to act on, mirroring `lmp::LmpLink`.
#[derive(Debug)]
pub struct Multiplexer {
    /// Whether this endpoint is the initiator or responder.
    pub role: Role,
    /// Current multiplexer session state.
    pub state: MultiplexerState,
    /// Open data link connections, keyed by DLCI.
    pub dlcs: HashMap<u8, Dlc>,
    /// Whether credit-based flow control was negotiated for this session.
    pub cfc: CreditFlowControl,
    peer_mtu: u16,
    pending_open: Option<MccPn>,
    listen_configs: HashMap<u8, (u16, u8)>,
}

impl Multiplexer {
    /// Creates a multiplexer for `role`, sized to the peer's L2CAP MTU.
    pub fn new(role: Role, peer_mtu: u16) -> Self {
        Self {
            role,
            state: MultiplexerState::Init,
            dlcs: HashMap::new(),
            cfc: CreditFlowControl::Unknown,
            peer_mtu,
            pending_open: None,
            listen_configs: HashMap::new(),
        }
    }

    /// Whether the multiplexer session (DLCI 0) is up.
    pub fn is_connected(&self) -> bool {
        self.state == MultiplexerState::Connected
    }

    fn role_c_r(&self) -> u8 {
        if self.role == Role::Initiator { 1 } else { 0 }
    }

    /// Initiator only: builds the `SABM(dlci=0)` that starts the multiplexer.
    pub fn start(&mut self) -> Result<Vec<u8>, SimbleError> {
        if self.role != Role::Initiator {
            return Err(SimbleError::DeviceError(
                "RFCOMM: only the initiator starts the multiplexer".into(),
            ));
        }
        if self.state != MultiplexerState::Init {
            return Err(SimbleError::DeviceError(
                "RFCOMM: multiplexer already starting".into(),
            ));
        }
        self.state = MultiplexerState::Connecting;
        Ok(RfcommFrame::sabm(1, 0).to_bytes())
    }

    /// Reserves `channel` (a server channel number, 1-30) to be accepted the
    /// next time a peer's PN command requests it. `false` if already reserved.
    pub fn listen(&mut self, channel: u8, max_frame_size: u16, initial_credits: u8) -> bool {
        reserve_listen_channel(
            &mut self.listen_configs,
            channel,
            max_frame_size,
            initial_credits,
        )
    }

    /// Starts opening a new DLC on `channel` (a server channel number,
    /// 1-30): sends the PN command. Either role may call this once connected.
    pub fn open_dlc(
        &mut self,
        channel: u8,
        max_frame_size: u16,
        initial_credits: u8,
    ) -> Result<Vec<u8>, SimbleError> {
        if self.state != MultiplexerState::Connected {
            return Err(SimbleError::DeviceError(
                "RFCOMM: multiplexer not connected".into(),
            ));
        }
        let pn = MccPn {
            dlci: channel << 1,
            cl: pn_cl::CFC_COMMAND,
            priority: 7,
            ack_timer: 0,
            max_frame_size,
            max_retransmissions: 0,
            initial_credits,
        };
        self.pending_open = Some(pn);
        self.state = MultiplexerState::Opening;
        let mcc = make_mcc(mcc_type::PN, true, &pn.to_bytes());
        Ok(RfcommFrame::uih(self.role_c_r(), 0, mcc, 0).to_bytes())
    }

    /// Queues `data` on an open DLC and returns the UIH frame(s) to send.
    pub fn write(&mut self, dlci: u8, data: &[u8]) -> Result<Vec<Vec<u8>>, SimbleError> {
        let dlc = self.dlcs.get_mut(&dlci).ok_or_else(|| unknown_dlci(dlci))?;
        if dlc.state != DlcState::Connected {
            return Err(SimbleError::DeviceError(format!(
                "RFCOMM: DLC {dlci} is not connected"
            )));
        }
        dlc.queue(data);
        Ok(dlc.process_tx().into_iter().map(|f| f.to_bytes()).collect())
    }

    /// Starts closing an open DLC: sends `DISC`.
    pub fn close_dlc(&mut self, dlci: u8) -> Result<Vec<u8>, SimbleError> {
        let dlc = self.dlcs.get_mut(&dlci).ok_or_else(|| unknown_dlci(dlci))?;
        if dlc.state != DlcState::Connected {
            return Err(SimbleError::DeviceError(format!(
                "RFCOMM: DLC {dlci} is not connected"
            )));
        }
        dlc.state = DlcState::Disconnecting;
        Ok(RfcommFrame::disc(self.role_c_r(), dlci).to_bytes())
    }

    /// Starts closing the multiplexer session: sends `DISC(dlci=0)`.
    pub fn disconnect(&mut self) -> Result<Vec<u8>, SimbleError> {
        if self.state != MultiplexerState::Connected {
            return Err(SimbleError::DeviceError(
                "RFCOMM: multiplexer not connected".into(),
            ));
        }
        self.state = MultiplexerState::Disconnecting;
        Ok(RfcommFrame::disc(self.role_c_r(), 0).to_bytes())
    }

    /// Processes one incoming RFCOMM frame (an already-demultiplexed L2CAP
    /// SDU on the RFCOMM channel), returning outgoing frame bytes plus any
    /// events. Malformed frames or FCS mismatches are reported as an error.
    pub fn receive(
        &mut self,
        bytes: &[u8],
    ) -> Result<(Vec<Vec<u8>>, Vec<MultiplexerEvent>), SimbleError> {
        let frame = RfcommFrame::parse(bytes).ok_or_else(|| {
            SimbleError::PacketParseError("RFCOMM: malformed frame or FCS mismatch".into())
        })?;
        if frame.dlci == 0 {
            Ok(self.on_control_frame(frame))
        } else if frame.frame_type == frame_type::DM {
            // DM responses target a DLCI, but the DLC isn't created until a
            // PN response arrives, so a rejection is handled here rather
            // than by a (nonexistent) DLC.
            Ok(self.on_dm_frame(frame))
        } else {
            Ok(self.on_dlc_frame(frame))
        }
    }

    fn on_control_frame(&mut self, frame: RfcommFrame) -> (Vec<Vec<u8>>, Vec<MultiplexerEvent>) {
        match frame.frame_type {
            frame_type::SABM => self.on_sabm(),
            frame_type::UA => self.on_ua(),
            frame_type::DISC => self.on_disc(),
            frame_type::UIH => self.on_uih_control(&frame),
            _ => (Vec::new(), Vec::new()),
        }
    }

    fn on_sabm(&mut self) -> (Vec<Vec<u8>>, Vec<MultiplexerEvent>) {
        if self.state != MultiplexerState::Init {
            return (Vec::new(), Vec::new());
        }
        self.state = MultiplexerState::Connected;
        (
            vec![RfcommFrame::ua(1, 0).to_bytes()],
            vec![MultiplexerEvent::Started],
        )
    }

    fn on_ua(&mut self) -> (Vec<Vec<u8>>, Vec<MultiplexerEvent>) {
        match self.state {
            MultiplexerState::Connecting => {
                self.state = MultiplexerState::Connected;
                (Vec::new(), vec![MultiplexerEvent::Started])
            }
            MultiplexerState::Disconnecting => {
                self.state = MultiplexerState::Disconnected;
                (Vec::new(), vec![MultiplexerEvent::Disconnected])
            }
            _ => (Vec::new(), Vec::new()),
        }
    }

    fn on_disc(&mut self) -> (Vec<Vec<u8>>, Vec<MultiplexerEvent>) {
        self.state = MultiplexerState::Disconnected;
        let c_r = if self.role == Role::Initiator { 0 } else { 1 };
        (
            vec![RfcommFrame::ua(c_r, 0).to_bytes()],
            vec![MultiplexerEvent::Disconnected],
        )
    }

    fn on_dm_frame(&mut self, frame: RfcommFrame) -> (Vec<Vec<u8>>, Vec<MultiplexerEvent>) {
        if self.state == MultiplexerState::Opening {
            self.state = MultiplexerState::Connected;
            self.pending_open = None;
            (
                Vec::new(),
                vec![MultiplexerEvent::DlcOpenRejected(frame.dlci >> 1)],
            )
        } else {
            (Vec::new(), Vec::new())
        }
    }

    fn on_uih_control(&mut self, frame: &RfcommFrame) -> (Vec<Vec<u8>>, Vec<MultiplexerEvent>) {
        let Some((mcc, c_r, value)) = parse_mcc(&frame.information) else {
            return (Vec::new(), Vec::new());
        };
        match mcc {
            mcc_type::PN => self.on_mcc_pn(c_r, value),
            mcc_type::MSC => self.on_mcc_msc(c_r, value),
            // RPN and any other MCC command: not implemented, ignored (see
            // module doc comment).
            _ => (Vec::new(), Vec::new()),
        }
    }

    fn on_mcc_pn(&mut self, c_r: bool, value: &[u8]) -> (Vec<Vec<u8>>, Vec<MultiplexerEvent>) {
        let Some(pn) = MccPn::parse(value) else {
            return (Vec::new(), Vec::new());
        };
        if c_r {
            self.on_mcc_pn_command(pn)
        } else {
            self.on_mcc_pn_response(pn)
        }
    }

    fn on_mcc_pn_command(&mut self, pn: MccPn) -> (Vec<Vec<u8>>, Vec<MultiplexerEvent>) {
        if pn.dlci & 1 != 0 {
            // Odd DLCI is initiator-side numbering; not expected in a command.
            return (Vec::new(), Vec::new());
        }
        let channel_number = pn.dlci >> 1;
        let Some(&(rx_max_frame_size, rx_initial_credits)) =
            self.listen_configs.get(&channel_number)
        else {
            return (vec![RfcommFrame::dm(1, pn.dlci).to_bytes()], Vec::new());
        };
        // RFCOMM 1.1 5.5.3: answer CFC only if the command asked for it.
        // Answering 0xE0 to a 0x00 request would make every P/F=1 frame's
        // leading credit octet look like application data to the peer.
        self.negotiate_cfc(pn.cl == pn_cl::CFC_COMMAND);
        let cfc = self.cfc == CreditFlowControl::Supported;
        let (response_cl, response_credits) = if cfc {
            (pn_cl::CFC_RESPONSE, rx_initial_credits)
        } else {
            (pn_cl::NO_CFC, 0)
        };
        let mut dlc = Dlc::new(
            pn.dlci,
            self.role_c_r(),
            pn.max_frame_size,
            if cfc { pn.initial_credits } else { 0 },
            rx_max_frame_size,
            response_credits,
            self.peer_mtu,
            cfc,
        );
        dlc.state = DlcState::Connecting;
        self.dlcs.insert(pn.dlci, dlc);
        let response_pn = MccPn {
            dlci: pn.dlci,
            cl: response_cl,
            priority: 7,
            ack_timer: 0,
            max_frame_size: rx_max_frame_size,
            max_retransmissions: 0,
            initial_credits: response_credits,
        };
        let mcc = make_mcc(mcc_type::PN, false, &response_pn.to_bytes());
        (
            vec![RfcommFrame::uih(self.role_c_r(), 0, mcc, 0).to_bytes()],
            Vec::new(),
        )
    }

    /// Settles the session's CFC state from one PN exchange. The first PN
    /// decides it for the whole session (RFCOMM 1.1 5.5.2); a later PN cannot
    /// re-enable CFC once a peer has declined it.
    fn negotiate_cfc(&mut self, peer_wants_cfc: bool) {
        if !peer_wants_cfc {
            self.cfc = CreditFlowControl::NotSupported;
        } else if self.cfc == CreditFlowControl::Unknown {
            self.cfc = CreditFlowControl::Supported;
        }
    }

    fn on_mcc_pn_response(&mut self, pn: MccPn) -> (Vec<Vec<u8>>, Vec<MultiplexerEvent>) {
        if self.state != MultiplexerState::Opening {
            return (Vec::new(), Vec::new());
        }
        let Some(requested) = self.pending_open.take() else {
            return (Vec::new(), Vec::new());
        };
        self.negotiate_cfc(pn.cl == pn_cl::CFC_RESPONSE);
        let cfc = self.cfc == CreditFlowControl::Supported;
        let mut dlc = Dlc::new(
            pn.dlci,
            self.role_c_r(),
            pn.max_frame_size,
            if cfc { pn.initial_credits } else { 0 },
            requested.max_frame_size,
            if cfc { requested.initial_credits } else { 0 },
            self.peer_mtu,
            cfc,
        );
        dlc.state = DlcState::Connecting;
        self.dlcs.insert(pn.dlci, dlc);
        (
            vec![RfcommFrame::sabm(self.role_c_r(), pn.dlci).to_bytes()],
            Vec::new(),
        )
    }

    fn on_mcc_msc(&mut self, c_r: bool, value: &[u8]) -> (Vec<Vec<u8>>, Vec<MultiplexerEvent>) {
        let Some(msc) = MccMsc::parse(value) else {
            return (Vec::new(), Vec::new());
        };
        let Some(dlc) = self.dlcs.get(&msc.dlci) else {
            return (Vec::new(), Vec::new());
        };
        if !c_r {
            return (Vec::new(), Vec::new());
        }
        let dlc_c_r = dlc.c_r;
        let response = MccMsc::default_response(msc.dlci);
        let mcc = make_mcc(mcc_type::MSC, false, &response.to_bytes());
        (
            vec![RfcommFrame::uih(dlc_c_r, 0, mcc, 0).to_bytes()],
            Vec::new(),
        )
    }

    fn on_dlc_frame(&mut self, frame: RfcommFrame) -> (Vec<Vec<u8>>, Vec<MultiplexerEvent>) {
        let dlci = frame.dlci;

        enum Action {
            None,
            Opened(Vec<Vec<u8>>),
            ClosedByPeerDisc(Vec<u8>),
            ClosedAfterOurDisc,
            Uih,
        }

        if !self.dlcs.contains_key(&dlci) {
            return self.on_frame_for_unknown_dlci(&frame);
        }

        let action = {
            let Some(dlc) = self.dlcs.get_mut(&dlci) else {
                return (Vec::new(), Vec::new());
            };
            let dlc_c_r = dlc.c_r;

            match frame.frame_type {
                frame_type::SABM => {
                    if dlc.state != DlcState::Connecting {
                        Action::None
                    } else {
                        dlc.state = DlcState::Connected;
                        let ua = RfcommFrame::ua(1 - dlc_c_r, dlci).to_bytes();
                        Action::Opened(vec![ua, msc_command(dlc_c_r, dlci)])
                    }
                }
                frame_type::UA => match dlc.state {
                    DlcState::Connecting => {
                        dlc.state = DlcState::Connected;
                        Action::Opened(vec![msc_command(dlc_c_r, dlci)])
                    }
                    DlcState::Disconnecting => Action::ClosedAfterOurDisc,
                    _ => Action::None,
                },
                frame_type::DISC => {
                    let ua = RfcommFrame::ua(1 - dlc_c_r, dlci).to_bytes();
                    Action::ClosedByPeerDisc(ua)
                }
                frame_type::UIH => Action::Uih,
                _ => Action::None,
            }
        };

        match action {
            Action::None => (Vec::new(), Vec::new()),
            Action::Opened(frames) => {
                self.state = MultiplexerState::Connected;
                (frames, vec![MultiplexerEvent::DlcOpened(dlci)])
            }
            Action::ClosedAfterOurDisc => {
                self.dlcs.remove(&dlci);
                (Vec::new(), vec![MultiplexerEvent::DlcClosed(dlci)])
            }
            Action::ClosedByPeerDisc(ua) => {
                self.dlcs.remove(&dlci);
                (vec![ua], vec![MultiplexerEvent::DlcClosed(dlci)])
            }
            Action::Uih => self.on_dlc_uih(dlci, frame),
        }
    }

    /// Handles a frame naming a DLCI we have no [`Dlc`] for.
    ///
    /// PN is *optional* before SABM (RFCOMM 1.1 5.5.3, TS 07.10 5.4.6.3.1),
    /// so a conforming peer may open a DLC with a bare SABM and the defaults.
    /// This used to drop such a frame silently, which left that peer waiting
    /// out its own T1 (60 s in Zephyr) with no idea why. We open the DLC with
    /// defaults when the channel is being listened on — matching Zephyr's
    /// `rfcomm_handle_sabm`, which is what real peers are tested against —
    /// and answer DM otherwise so the peer fails immediately.
    fn on_frame_for_unknown_dlci(
        &mut self,
        frame: &RfcommFrame,
    ) -> (Vec<Vec<u8>>, Vec<MultiplexerEvent>) {
        let dlci = frame.dlci;
        match frame.frame_type {
            frame_type::SABM => {
                let Some(&(rx_max_frame_size, rx_initial_credits)) =
                    self.listen_configs.get(&(dlci >> 1))
                else {
                    return (vec![RfcommFrame::dm(1, dlci).to_bytes()], Vec::new());
                };
                // No PN means nothing was negotiated: N1 falls back to its
                // default and credit-based flow control is off for the
                // session (Zephyr's `rfcomm_dlc_connected` settles an
                // still-`Unknown` session to not-supported the same way).
                if self.cfc == CreditFlowControl::Unknown {
                    self.cfc = CreditFlowControl::NotSupported;
                }
                let cfc = self.cfc == CreditFlowControl::Supported;
                let mut dlc = Dlc::new(
                    dlci,
                    self.role_c_r(),
                    DEFAULT_FRAME_SIZE_WITHOUT_PN,
                    0,
                    rx_max_frame_size,
                    if cfc { rx_initial_credits } else { 0 },
                    self.peer_mtu,
                    cfc,
                );
                dlc.state = DlcState::Connected;
                let dlc_c_r = dlc.c_r;
                self.dlcs.insert(dlci, dlc);
                self.state = MultiplexerState::Connected;
                let ua = RfcommFrame::ua(1 - dlc_c_r, dlci).to_bytes();
                (
                    vec![ua, msc_command(dlc_c_r, dlci)],
                    vec![MultiplexerEvent::DlcOpened(dlci)],
                )
            }
            // TS 07.10 5.4.6.3.1: DM is the answer to a DISC on a DLC that is
            // not open, so the peer stops retransmitting.
            frame_type::DISC => (vec![RfcommFrame::dm(1, dlci).to_bytes()], Vec::new()),
            _ => (Vec::new(), Vec::new()),
        }
    }

    fn on_dlc_uih(
        &mut self,
        dlci: u8,
        frame: RfcommFrame,
    ) -> (Vec<Vec<u8>>, Vec<MultiplexerEvent>) {
        let Some(dlc) = self.dlcs.get_mut(&dlci) else {
            return (Vec::new(), Vec::new());
        };
        let mut data = frame.information.as_slice();
        // The credit octet exists only when CFC is in use (RFCOMM 1.1 5.5.2),
        // so P/F is only a credit indicator then; reading it otherwise would
        // eat the first byte of application data. Zephyr strips it whenever
        // P/F is set, which is more lenient towards a peer that sets P/F it
        // did not negotiate; keying on the negotiated state instead keeps rx
        // symmetric with what `process_tx` will send.
        if dlc.cfc && frame.p_f == 1 {
            let received_credits = data.first().copied().unwrap_or(0);
            dlc.tx_credits = dlc.tx_credits.saturating_add(received_credits);
            data = &data[data.len().min(1)..];
        }

        let mut events = Vec::new();
        if !data.is_empty() {
            events.push(MultiplexerEvent::DataReceived(dlci, data.to_vec()));
            dlc.rx_credits = dlc.rx_credits.saturating_sub(1);
        }

        let out_frames = dlc.process_tx().into_iter().map(|f| f.to_bytes()).collect();
        (out_frames, events)
    }
}

/// The MSC command every side sends once its DLC opens (TS 07.10 5.4.6.3.7):
/// V.24 signals asserted, carried on the control channel.
fn msc_command(dlc_c_r: u8, dlci: u8) -> Vec<u8> {
    let mcc = make_mcc(
        mcc_type::MSC,
        true,
        &MccMsc::default_response(dlci).to_bytes(),
    );
    RfcommFrame::uih(dlc_c_r, 0, mcc, 0).to_bytes()
}

fn unknown_dlci(dlci: u8) -> SimbleError {
    SimbleError::DeviceError(format!("RFCOMM: unknown DLCI {dlci:#04x}"))
}

// ---------------------------------------------------------------------------
// Client / Server
// ---------------------------------------------------------------------------

/// Client role: thin helper tying [`Multiplexer`] to a
/// [`ClassicChannelManager`]-obtained L2CAP channel.
#[derive(Debug, Default)]
pub struct RfcommClient;

impl RfcommClient {
    /// Allocates the RFCOMM L2CAP channel (client role) and returns its
    /// local CID plus the `Connection Request` to send. Drive the L2CAP
    /// handshake to completion via `manager` as usual, then build
    /// `Multiplexer::new(Role::Initiator, channel.peer_mtu)` and call
    /// [`Multiplexer::start`].
    pub fn connect(
        manager: &mut ClassicChannelManager,
        mtu: u16,
    ) -> Result<(u16, ConnectionRequestHeader), SimbleError> {
        manager.connect(&ClassicChannelSpec::with_mtu(RFCOMM_PSM, mtu))
    }
}

/// Server role: registers the RFCOMM PSM and tracks which server channel
/// numbers (1-30) are being listened on.
#[derive(Debug, Default)]
pub struct RfcommServer {
    listen_configs: HashMap<u8, (u16, u8)>,
}

impl RfcommServer {
    /// Creates a server with no listening channels.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers the RFCOMM PSM with the L2CAP channel manager.
    pub fn register(&self, manager: &mut ClassicChannelManager) -> Result<(), SimbleError> {
        manager.register_server(RFCOMM_PSM)
    }

    /// Reserves `channel` (a server channel number, 1-30) to accept incoming
    /// DLC opens once an L2CAP connection and its `Multiplexer` exist.
    pub fn listen(&mut self, channel: u8, max_frame_size: u16, initial_credits: u8) -> bool {
        reserve_listen_channel(
            &mut self.listen_configs,
            channel,
            max_frame_size,
            initial_credits,
        )
    }

    /// Builds a responder-role `Multiplexer` for a freshly opened L2CAP
    /// channel, pre-populated with this server's `listen`-registered channels.
    pub fn new_multiplexer(&self, channel: &ClassicChannel) -> Multiplexer {
        let mut multiplexer = Multiplexer::new(Role::Responder, channel.peer_mtu);
        for (&ch, &(max_frame_size, initial_credits)) in &self.listen_configs {
            multiplexer.listen(ch, max_frame_size, initial_credits);
        }
        multiplexer
    }
}

// ---------------------------------------------------------------------------
// SDP integration
// ---------------------------------------------------------------------------

/// Builds SDP service record attributes advertising an RFCOMM service on
/// `channel`, optionally tagged with a service class `uuid`.
pub fn make_service_sdp_records(
    service_record_handle: u32,
    channel: u8,
    uuid: Option<SdpUuid>,
) -> Vec<ServiceAttribute> {
    let mut records = vec![
        ServiceAttribute::new(
            attribute_id::SERVICE_RECORD_HANDLE,
            DataElement::unsigned_integer_32(service_record_handle),
        ),
        ServiceAttribute::new(
            attribute_id::BROWSE_GROUP_LIST,
            DataElement::sequence(vec![DataElement::uuid(SdpUuid::SDP_PUBLIC_BROWSE_ROOT)]),
        ),
        ServiceAttribute::new(
            attribute_id::PROTOCOL_DESCRIPTOR_LIST,
            DataElement::sequence(vec![
                DataElement::sequence(vec![DataElement::uuid(SdpUuid::BT_L2CAP_PROTOCOL_ID)]),
                DataElement::sequence(vec![
                    DataElement::uuid(SdpUuid::BT_RFCOMM_PROTOCOL_ID),
                    DataElement::unsigned_integer_8(channel),
                ]),
            ]),
        ),
    ];
    if let Some(uuid) = uuid {
        records.push(ServiceAttribute::new(
            attribute_id::SERVICE_CLASS_ID_LIST,
            DataElement::sequence(vec![DataElement::uuid(uuid)]),
        ));
    }
    records
}

/// Searches for all RFCOMM services via `sdp_client`/`transport`, returning
/// a map from server channel number to that service's class UUIDs.
pub fn find_rfcomm_channels(
    sdp_client: &mut SdpClient,
    mut transport: impl FnMut(&[u8]) -> Vec<u8>,
) -> Result<HashMap<u8, Vec<SdpUuid>>, SimbleError> {
    let results = sdp_client.search_attributes(
        &[SdpUuid::BT_RFCOMM_PROTOCOL_ID],
        &[
            AttributeIdSpec::Single(attribute_id::PROTOCOL_DESCRIPTOR_LIST),
            AttributeIdSpec::Single(attribute_id::SERVICE_CLASS_ID_LIST),
        ],
        &mut transport,
    )?;

    let mut channels = HashMap::new();
    for attrs in results {
        let mut service_classes = Vec::new();
        let mut channel = None;
        for attr in &attrs {
            match attr.id {
                attribute_id::PROTOCOL_DESCRIPTOR_LIST => {
                    // [[L2CAP_PROTOCOL], [RFCOMM_PROTOCOL, RFCOMM_CHANNEL]]
                    if let Some(rfcomm_proto) = attr
                        .value
                        .as_sequence()
                        .and_then(|list| list.get(1))
                        .and_then(DataElement::as_sequence)
                        && let Some((value, _)) = rfcomm_proto
                            .get(1)
                            .and_then(DataElement::as_unsigned_integer)
                    {
                        channel = Some(value as u8);
                    }
                }
                attribute_id::SERVICE_CLASS_ID_LIST => {
                    if let Some(list) = attr.value.as_sequence() {
                        service_classes = list.iter().filter_map(DataElement::as_uuid).collect();
                    }
                }
                _ => {}
            }
        }
        if let Some(channel) = channel
            && !service_classes.is_empty()
        {
            channels.insert(channel, service_classes);
        }
    }
    Ok(channels)
}

/// Searches for the server channel number of the RFCOMM service advertising
/// `uuid`, if any.
pub fn find_rfcomm_channel_with_uuid(
    sdp_client: &mut SdpClient,
    uuid: SdpUuid,
    transport: impl FnMut(&[u8]) -> Vec<u8>,
) -> Result<Option<u8>, SimbleError> {
    let channels = find_rfcomm_channels(sdp_client, transport)?;
    Ok(channels
        .into_iter()
        .find(|(_, uuids)| uuids.contains(&uuid))
        .map(|(channel, _)| channel))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fcs_known_vector() {
        // ETSI TS 07.10 known-good vector: address=0x03, control=0x3f,
        // length=0x01 (1-byte, EA set, value 0), fcs=0x1c.
        assert_eq!(compute_fcs(&[0x03, 0x3f, 0x01]), 0x1c);
    }

    #[test]
    fn test_frame_round_trip_known_vector() {
        let data = [0x03, 0x3f, 0x01, 0x1c];
        let frame = RfcommFrame::parse(&data).expect("valid frame");
        assert_eq!(frame.frame_type, frame_type::SABM);
        assert_eq!(frame.c_r, 1);
        assert_eq!(frame.dlci, 0);
        assert_eq!(frame.p_f, 1);
        assert!(frame.information.is_empty());
        assert_eq!(frame.to_bytes(), data);
    }

    #[test]
    fn test_frame_round_trip_with_payload() {
        let frame = RfcommFrame::uih(1, 5, b"hello".to_vec(), 0);
        let bytes = frame.to_bytes();
        let parsed = RfcommFrame::parse(&bytes).expect("valid frame");
        assert_eq!(parsed, frame);
    }

    #[test]
    fn test_frame_corrupt_fcs_rejected() {
        let frame = RfcommFrame::uih(1, 5, b"hello".to_vec(), 0);
        let mut bytes = frame.to_bytes();
        *bytes.last_mut().unwrap() ^= 0xFF;
        assert!(RfcommFrame::parse(&bytes).is_none());
    }

    #[test]
    fn test_frame_truncated_rejected() {
        assert!(RfcommFrame::parse(&[0x03]).is_none());
        assert!(RfcommFrame::parse(&[]).is_none());
    }

    #[test]
    fn test_mcc_pn_round_trip() {
        let pn = MccPn {
            dlci: 4,
            cl: 0xF0,
            priority: 7,
            ack_timer: 0,
            max_frame_size: 1000,
            max_retransmissions: 0,
            initial_credits: 7,
        };
        let mcc = make_mcc(mcc_type::PN, true, &pn.to_bytes());
        let (parsed_type, c_r, value) = parse_mcc(&mcc).expect("valid mcc");
        assert_eq!(parsed_type, mcc_type::PN);
        assert!(c_r);
        assert_eq!(MccPn::parse(value), Some(pn));
    }

    /// Drives both sides of a `Multiplexer` pair to a fully open DLC by
    /// alternately feeding each side's output into the other.
    fn drive_open_dlc(initiator: &mut Multiplexer, responder: &mut Multiplexer, channel: u8) -> u8 {
        let sabm = initiator.start().unwrap();
        let mut to_responder = vec![sabm];
        let mut to_initiator: Vec<Vec<u8>> = Vec::new();
        let mut opened_dlci = None;
        let mut requested_open = false;

        for _ in 0..10 {
            let mut next_to_initiator = Vec::new();
            for frame in to_responder.drain(..) {
                let (out, events) = responder.receive(&frame).unwrap();
                next_to_initiator.extend(out);
                for event in events {
                    if let MultiplexerEvent::DlcOpened(dlci) = event {
                        opened_dlci = Some(dlci);
                    }
                }
            }
            let mut next_to_responder = Vec::new();
            for frame in to_initiator.drain(..) {
                let (out, events) = initiator.receive(&frame).unwrap();
                next_to_responder.extend(out);
                for event in events {
                    if let MultiplexerEvent::DlcOpened(dlci) = event {
                        opened_dlci = Some(dlci);
                    }
                }
            }
            to_initiator = next_to_initiator;
            to_responder = next_to_responder;

            if !requested_open && initiator.is_connected() && responder.is_connected() {
                requested_open = true;
                to_responder.push(
                    initiator
                        .open_dlc(channel, DEFAULT_MAX_FRAME_SIZE, DEFAULT_INITIAL_CREDITS)
                        .unwrap(),
                );
            }

            if opened_dlci.is_some() && to_initiator.is_empty() && to_responder.is_empty() {
                break;
            }
        }
        opened_dlci.expect("DLC did not open")
    }

    #[test]
    fn test_multiplexer_startup_handshake() {
        let mut initiator = Multiplexer::new(Role::Initiator, DEFAULT_L2CAP_MTU);
        let mut responder = Multiplexer::new(Role::Responder, DEFAULT_L2CAP_MTU);

        let sabm = initiator.start().unwrap();
        assert_eq!(initiator.state, MultiplexerState::Connecting);

        let (to_initiator, events) = responder.receive(&sabm).unwrap();
        assert_eq!(responder.state, MultiplexerState::Connected);
        assert_eq!(events, vec![MultiplexerEvent::Started]);

        let (out, events) = initiator.receive(&to_initiator[0]).unwrap();
        assert!(out.is_empty());
        assert_eq!(events, vec![MultiplexerEvent::Started]);
        assert_eq!(initiator.state, MultiplexerState::Connected);
    }

    #[test]
    fn test_open_dlc_and_send_receive_with_credits() {
        let mut initiator = Multiplexer::new(Role::Initiator, DEFAULT_L2CAP_MTU);
        let mut responder = Multiplexer::new(Role::Responder, DEFAULT_L2CAP_MTU);
        responder.listen(1, DEFAULT_MAX_FRAME_SIZE, DEFAULT_INITIAL_CREDITS);

        let dlci = drive_open_dlc(&mut initiator, &mut responder, 1);
        assert!(initiator.dlcs.get(&dlci).unwrap().is_open());
        assert!(responder.dlcs.get(&dlci).unwrap().is_open());
        assert_eq!(
            initiator.dlcs.get(&dlci).unwrap().tx_credits,
            DEFAULT_INITIAL_CREDITS
        );

        // Initiator -> responder data.
        let frames = initiator.write(dlci, b"The quick brown fox").unwrap();
        assert!(initiator.dlcs.get(&dlci).unwrap().tx_credits < DEFAULT_INITIAL_CREDITS);
        let mut received = Vec::new();
        for frame in frames {
            let (_, events) = responder.receive(&frame).unwrap();
            for event in events {
                if let MultiplexerEvent::DataReceived(got_dlci, data) = event {
                    assert_eq!(got_dlci, dlci);
                    received.extend(data);
                }
            }
        }
        assert_eq!(received, b"The quick brown fox");

        // Responder -> initiator data.
        let frames = responder.write(dlci, b"Lorem ipsum").unwrap();
        let mut received = Vec::new();
        for frame in frames {
            let (_, events) = initiator.receive(&frame).unwrap();
            for event in events {
                if let MultiplexerEvent::DataReceived(got_dlci, data) = event {
                    assert_eq!(got_dlci, dlci);
                    received.extend(data);
                }
            }
        }
        assert_eq!(received, b"Lorem ipsum");
    }

    #[test]
    fn test_credit_exhaustion_blocks_send_until_refilled() {
        let mut initiator = Multiplexer::new(Role::Initiator, DEFAULT_L2CAP_MTU);
        let mut responder = Multiplexer::new(Role::Responder, DEFAULT_L2CAP_MTU);
        responder.listen(1, DEFAULT_MAX_FRAME_SIZE, 1);

        let dlci = drive_open_dlc(&mut initiator, &mut responder, 1);
        // Responder granted the initiator only 1 initial tx credit.
        assert_eq!(initiator.dlcs.get(&dlci).unwrap().tx_credits, 1);

        let frames = initiator.write(dlci, b"first").unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(initiator.dlcs.get(&dlci).unwrap().tx_credits, 0);

        // A second write has no credit left to send immediately.
        let frames = initiator.write(dlci, b"second").unwrap();
        assert!(frames.is_empty());

        // Peer grants 3 fresh credits via a credit-only UIH frame (P/F=1,
        // no data beyond the leading credit octet); the buffered "second"
        // write can now drain, spending one of them.
        let credit_grant = RfcommFrame::uih(0, dlci, vec![3], 1).to_bytes();
        let (out, events) = initiator.receive(&credit_grant).unwrap();
        assert!(events.is_empty());
        assert_eq!(out.len(), 1);
        assert_eq!(initiator.dlcs.get(&dlci).unwrap().tx_credits, 2);
    }

    #[test]
    fn test_close_dlc_disc_ua() {
        let mut initiator = Multiplexer::new(Role::Initiator, DEFAULT_L2CAP_MTU);
        let mut responder = Multiplexer::new(Role::Responder, DEFAULT_L2CAP_MTU);
        responder.listen(1, DEFAULT_MAX_FRAME_SIZE, DEFAULT_INITIAL_CREDITS);
        let dlci = drive_open_dlc(&mut initiator, &mut responder, 1);

        let disc = initiator.close_dlc(dlci).unwrap();
        assert_eq!(
            initiator.dlcs.get(&dlci).unwrap().state,
            DlcState::Disconnecting
        );

        let (to_initiator, events) = responder.receive(&disc).unwrap();
        assert_eq!(events, vec![MultiplexerEvent::DlcClosed(dlci)]);
        assert!(!responder.dlcs.contains_key(&dlci));

        let (_, events) = initiator.receive(&to_initiator[0]).unwrap();
        assert_eq!(events, vec![MultiplexerEvent::DlcClosed(dlci)]);
        assert!(!initiator.dlcs.contains_key(&dlci));
    }

    #[test]
    fn test_disconnect_multiplexer() {
        let mut initiator = Multiplexer::new(Role::Initiator, DEFAULT_L2CAP_MTU);
        let mut responder = Multiplexer::new(Role::Responder, DEFAULT_L2CAP_MTU);
        let sabm = initiator.start().unwrap();
        let (to_initiator, _) = responder.receive(&sabm).unwrap();
        initiator.receive(&to_initiator[0]).unwrap();

        let disc = initiator.disconnect().unwrap();
        let (to_initiator, events) = responder.receive(&disc).unwrap();
        assert_eq!(events, vec![MultiplexerEvent::Disconnected]);
        assert_eq!(responder.state, MultiplexerState::Disconnected);

        let (_, events) = initiator.receive(&to_initiator[0]).unwrap();
        assert_eq!(events, vec![MultiplexerEvent::Disconnected]);
        assert_eq!(initiator.state, MultiplexerState::Disconnected);
    }

    #[test]
    fn test_open_dlc_rejected_unlisted_channel() {
        let mut initiator = Multiplexer::new(Role::Initiator, DEFAULT_L2CAP_MTU);
        let mut responder = Multiplexer::new(Role::Responder, DEFAULT_L2CAP_MTU);
        // No listen() call: channel 1 is not being served.
        let sabm = initiator.start().unwrap();
        let (to_initiator, _) = responder.receive(&sabm).unwrap();
        initiator.receive(&to_initiator[0]).unwrap();

        let pn = initiator
            .open_dlc(1, DEFAULT_MAX_FRAME_SIZE, DEFAULT_INITIAL_CREDITS)
            .unwrap();
        let (to_initiator, _) = responder.receive(&pn).unwrap();
        let (_, events) = initiator.receive(&to_initiator[0]).unwrap();
        assert_eq!(events, vec![MultiplexerEvent::DlcOpenRejected(1)]);
        assert_eq!(initiator.state, MultiplexerState::Connected);
    }

    #[test]
    fn test_service_record_sdp_round_trip() {
        use crate::classic::sdp::SdpServer;

        let handle = 2u32;
        let channel = 1u8;
        let uuid = SdpUuid::Uuid128([
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x01,
        ]);

        let mut sdp_server = SdpServer::new();
        sdp_server.service_records.insert(
            handle,
            make_service_sdp_records(handle, channel, Some(uuid)),
        );

        let mut sdp_client = SdpClient::new();
        let channels =
            find_rfcomm_channels(&mut sdp_client, |req| sdp_server.handle_request(req, 1024))
                .unwrap();
        assert_eq!(channels.get(&channel), Some(&vec![uuid]));

        let found = find_rfcomm_channel_with_uuid(&mut sdp_client, uuid, |req| {
            sdp_server.handle_request(req, 1024)
        })
        .unwrap();
        assert_eq!(found, Some(channel));
    }

    #[test]
    fn test_server_psm_registration_lifecycle() {
        let mut manager = ClassicChannelManager::new();
        let server = RfcommServer::new();
        server.register(&mut manager).unwrap();
        assert!(manager.is_server_registered(RFCOMM_PSM));
        manager.unregister_server(RFCOMM_PSM);
        assert!(!manager.is_server_registered(RFCOMM_PSM));
    }

    /// Brings a responder multiplexer's session up by feeding it SABM(0).
    fn connected_responder() -> Multiplexer {
        let mut responder = Multiplexer::new(Role::Responder, DEFAULT_L2CAP_MTU);
        responder
            .receive(&RfcommFrame::sabm(1, 0).to_bytes())
            .expect("SABM(0) accepted");
        responder
    }

    fn pn_command_frame(dlci: u8, cl: u8, initial_credits: u8) -> Vec<u8> {
        let pn = MccPn {
            dlci,
            cl,
            priority: 7,
            ack_timer: 0,
            max_frame_size: DEFAULT_MAX_FRAME_SIZE,
            max_retransmissions: 0,
            initial_credits,
        };
        RfcommFrame::uih(1, 0, make_mcc(mcc_type::PN, true, &pn.to_bytes()), 0).to_bytes()
    }

    /// Extracts the single PN record from a multiplexer's reply frames.
    fn sole_pn_response(frames: &[Vec<u8>]) -> MccPn {
        let frame = RfcommFrame::parse(&frames[0]).expect("valid frame");
        let (mcc, c_r, value) = parse_mcc(&frame.information).expect("valid mcc");
        assert_eq!(mcc, mcc_type::PN);
        assert!(!c_r, "a PN response must have C/R clear");
        MccPn::parse(value).expect("valid PN")
    }

    #[test]
    fn test_a_bare_sabm_on_a_listened_channel_opens_the_dlc_with_defaults() {
        // PN is optional before SABM (RFCOMM 1.1 5.5.3): a peer may open a
        // DLC with a bare SABM. Dropping it silently left that peer waiting
        // out its own T1.
        let mut responder = connected_responder();
        responder.listen(1, DEFAULT_MAX_FRAME_SIZE, DEFAULT_INITIAL_CREDITS);

        let (out, events) = responder
            .receive(&RfcommFrame::sabm(1, 2).to_bytes())
            .unwrap();

        assert_eq!(events, vec![MultiplexerEvent::DlcOpened(2)]);
        let ua = RfcommFrame::parse(&out[0]).expect("valid frame");
        assert_eq!(ua.frame_type, frame_type::UA);
        assert_eq!(ua.dlci, 2);

        let dlc = responder.dlcs.get(&2).expect("DLC created");
        assert!(dlc.is_open());
        // Nothing was negotiated, so N1 is its default and CFC is off.
        assert_eq!(dlc.tx_max_frame_size, DEFAULT_FRAME_SIZE_WITHOUT_PN);
        assert!(!dlc.cfc);
        assert_eq!(responder.cfc, CreditFlowControl::NotSupported);
    }

    #[test]
    fn test_a_bare_sabm_on_an_unlistened_channel_is_answered_with_dm() {
        let mut responder = connected_responder();

        let (out, events) = responder
            .receive(&RfcommFrame::sabm(1, 8).to_bytes())
            .unwrap();

        assert!(events.is_empty());
        let dm = RfcommFrame::parse(&out[0]).expect("valid frame");
        assert_eq!(dm.frame_type, frame_type::DM);
        assert_eq!(dm.dlci, 8);
        assert!(responder.dlcs.is_empty());
    }

    #[test]
    fn test_a_disc_on_an_unknown_dlci_is_answered_with_dm() {
        let mut responder = connected_responder();

        let (out, _) = responder
            .receive(&RfcommFrame::disc(1, 6).to_bytes())
            .unwrap();

        let dm = RfcommFrame::parse(&out[0]).expect("valid frame");
        assert_eq!(dm.frame_type, frame_type::DM);
        assert_eq!(dm.dlci, 6);
    }

    #[test]
    fn test_a_pn_asking_for_no_cfc_is_not_answered_with_cfc() {
        // RFCOMM 1.1 5.5.3: 0xE0 may only answer 0xF0. Answering it to a
        // 0x00 request makes our credit octets look like application data.
        let mut responder = connected_responder();
        responder.listen(1, DEFAULT_MAX_FRAME_SIZE, DEFAULT_INITIAL_CREDITS);

        let (out, _) = responder
            .receive(&pn_command_frame(2, pn_cl::NO_CFC, 0))
            .unwrap();

        let pn = sole_pn_response(&out);
        assert_eq!(pn.cl, pn_cl::NO_CFC);
        assert_eq!(pn.initial_credits, 0);
        assert_eq!(responder.cfc, CreditFlowControl::NotSupported);
    }

    #[test]
    fn test_a_pn_asking_for_cfc_is_answered_with_cfc() {
        let mut responder = connected_responder();
        responder.listen(1, DEFAULT_MAX_FRAME_SIZE, DEFAULT_INITIAL_CREDITS);

        let (out, _) = responder
            .receive(&pn_command_frame(2, pn_cl::CFC_COMMAND, 7))
            .unwrap();

        let pn = sole_pn_response(&out);
        assert_eq!(pn.cl, pn_cl::CFC_RESPONSE);
        assert_eq!(pn.initial_credits, DEFAULT_INITIAL_CREDITS);
        assert_eq!(responder.cfc, CreditFlowControl::Supported);
    }

    #[test]
    fn test_a_dlc_without_cfc_sends_no_credit_octet() {
        // The whole point of honouring cl=0x00: the peer's byte stream must
        // contain exactly what was written, with no credit octet spliced in.
        let mut responder = connected_responder();
        responder.listen(1, DEFAULT_MAX_FRAME_SIZE, DEFAULT_INITIAL_CREDITS);
        responder
            .receive(&pn_command_frame(2, pn_cl::NO_CFC, 0))
            .unwrap();
        responder
            .receive(&RfcommFrame::sabm(1, 2).to_bytes())
            .unwrap();

        let frames = responder.write(2, b"hello").unwrap();

        assert_eq!(frames.len(), 1, "no credit-only frame should be emitted");
        let frame = RfcommFrame::parse(&frames[0]).expect("valid frame");
        assert_eq!(frame.p_f, 0, "P/F marks a credit octet, which is absent");
        assert_eq!(frame.information, b"hello");
    }

    #[test]
    fn test_a_dlc_without_cfc_is_not_gated_on_credits() {
        // Without CFC the peer grants no credits at all, so gating on them
        // would mean never sending anything.
        let mut responder = connected_responder();
        responder.listen(1, DEFAULT_MAX_FRAME_SIZE, DEFAULT_INITIAL_CREDITS);
        responder
            .receive(&pn_command_frame(2, pn_cl::NO_CFC, 0))
            .unwrap();
        responder
            .receive(&RfcommFrame::sabm(1, 2).to_bytes())
            .unwrap();
        assert_eq!(responder.dlcs.get(&2).unwrap().tx_credits, 0);

        assert!(!responder.write(2, b"first").unwrap().is_empty());
        assert!(!responder.write(2, b"second").unwrap().is_empty());
    }

    #[test]
    fn test_a_dlc_with_cfc_still_sends_the_credit_octet() {
        let mut responder = connected_responder();
        responder.listen(1, DEFAULT_MAX_FRAME_SIZE, DEFAULT_INITIAL_CREDITS);
        responder
            .receive(&pn_command_frame(2, pn_cl::CFC_COMMAND, 7))
            .unwrap();
        responder
            .receive(&RfcommFrame::sabm(1, 2).to_bytes())
            .unwrap();

        let frames = responder.write(2, b"hello").unwrap();

        let frame = RfcommFrame::parse(&frames[0]).expect("valid frame");
        assert_eq!(frame.p_f, 1);
        assert_eq!(&frame.information[1..], b"hello");
    }

    #[test]
    fn test_data_delivered_immediately_after_dlc_opens() {
        // `receive` is synchronous and returns events directly, so a UIH
        // data frame delivered right after the UA that completes a DLC open
        // must still be reported correctly, with no window where it could
        // be dropped.
        let mut initiator = Multiplexer::new(Role::Initiator, DEFAULT_L2CAP_MTU);
        let mut responder = Multiplexer::new(Role::Responder, DEFAULT_L2CAP_MTU);
        responder.listen(1, DEFAULT_MAX_FRAME_SIZE, DEFAULT_INITIAL_CREDITS);
        let dlci = drive_open_dlc(&mut initiator, &mut responder, 1);

        let data_frame = RfcommFrame::uih(0, dlci, b"123".to_vec(), 0).to_bytes();
        let (_, events) = initiator.receive(&data_frame).unwrap();
        assert_eq!(
            events,
            vec![MultiplexerEvent::DataReceived(dlci, b"123".to_vec())]
        );
    }
}
