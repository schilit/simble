// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! AVCTP (Audio/Video Control Transport Protocol), the L2CAP-borne message
//! transport AVRCP (`crate::classic::avrcp`) rides on.
//!
//! Wire format (AVCTP spec 1.4, section 6): every packet starts with a
//! 1-byte header packing the transaction label, packet type
//! (single/start/continue/end), C/R bit (command or response), and IPID bit
//! (set in a response to flag an unsupported profile). Single packets carry
//! the 16-bit big-endian PID (the profile's service class UUID value, e.g.
//! [`crate::classic::avrcp::AVRCP_PID`]) right after the header; when a
//! message exceeds the L2CAP MTU it is fragmented into a start packet
//! (header, packet count, PID) followed by continue/end packets that carry
//! only the header, and reassembled by [`MessageAssembler`].
//!
//! [`Protocol`] is the per-channel state machine in the house synchronous
//! shape (`receive(pdu) -> (outgoing PDUs, events)`, as in
//! `avdtp::Protocol`): it reassembles messages, routes commands by PID, and
//! answers commands for unregistered PIDs with an IPID response
//! (AVCTP spec 1.4, section 5.2).

use crate::l2cap::classic::{ClassicChannelManager, ClassicChannelSpec};
use crate::packets::l2cap_signaling::ConnectionRequestHeader;
use crate::types::SimbleError;

/// Well-known PSM for the AVCTP control channel (Bluetooth Assigned Numbers).
pub const AVCTP_PSM: u16 = 0x0017;
/// Well-known PSM for the AVCTP browsing channel (Bluetooth Assigned Numbers).
pub const AVCTP_BROWSING_PSM: u16 = 0x001B;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PacketType {
    Single,
    Start,
    Continue,
    End,
}

impl PacketType {
    fn from_bits(bits: u8) -> Self {
        match bits & 3 {
            0 => PacketType::Single,
            1 => PacketType::Start,
            2 => PacketType::Continue,
            _ => PacketType::End,
        }
    }

    fn as_bits(self) -> u8 {
        match self {
            PacketType::Single => 0,
            PacketType::Start => 1,
            PacketType::Continue => 2,
            PacketType::End => 3,
        }
    }
}

/// A fully reassembled AVCTP message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub transaction_label: u8,
    pub is_command: bool,
    /// Set in a response to report that the command's PID is not supported
    /// (AVCTP spec 1.4, section 6.1.2).
    pub ipid: bool,
    pub pid: u16,
    pub payload: Vec<u8>,
}

fn header_byte(transaction_label: u8, packet_type: PacketType, is_command: bool, ipid: bool) -> u8 {
    (transaction_label << 4)
        | (packet_type.as_bits() << 2)
        | (u8::from(!is_command) << 1)
        | u8::from(ipid)
}

/// Serializes one message into AVCTP packets, fragmenting into
/// start/continue/end packets when it exceeds `peer_mtu` (AVCTP spec 1.4,
/// section 6.2).
pub fn write_message(
    transaction_label: u8,
    is_command: bool,
    ipid: bool,
    pid: u16,
    payload: &[u8],
    peer_mtu: u16,
) -> Vec<Vec<u8>> {
    let mtu = usize::from(peer_mtu.max(5));
    if 3 + payload.len() <= mtu {
        let mut pdu = vec![header_byte(
            transaction_label,
            PacketType::Single,
            is_command,
            ipid,
        )];
        pdu.extend_from_slice(&pid.to_be_bytes());
        pdu.extend_from_slice(payload);
        return vec![pdu];
    }
    // Start packets carry a 4-byte header, continuation packets 1 byte.
    let first_capacity = mtu - 4;
    let rest_capacity = mtu - 1;
    // The packet-count field is one byte; payloads anywhere near 255
    // fragments would exceed any realistic AV/C message, so saturate.
    let number_of_packets =
        (1 + (payload.len() - first_capacity).div_ceil(rest_capacity)).min(255) as u8;

    let mut pdus = Vec::new();
    let mut pdu = vec![
        header_byte(transaction_label, PacketType::Start, is_command, ipid),
        number_of_packets,
    ];
    pdu.extend_from_slice(&pid.to_be_bytes());
    pdu.extend_from_slice(&payload[..first_capacity]);
    pdus.push(pdu);

    let mut remaining = &payload[first_capacity..];
    while !remaining.is_empty() {
        let take = rest_capacity.min(remaining.len());
        let packet_type = if take == remaining.len() {
            PacketType::End
        } else {
            PacketType::Continue
        };
        let mut pdu = vec![header_byte(
            transaction_label,
            packet_type,
            is_command,
            ipid,
        )];
        pdu.extend_from_slice(&remaining[..take]);
        pdus.push(pdu);
        remaining = &remaining[take..];
    }
    pdus
}

// ---------------------------------------------------------------------------
// Message assembler
// ---------------------------------------------------------------------------

/// Reassembles fragmented AVCTP messages. Malformed or out-of-sequence
/// packets are dropped without panicking so a hostile peer cannot take the
/// control channel down.
#[derive(Debug, Default)]
pub struct MessageAssembler {
    in_progress: bool,
    transaction_label: u8,
    is_command: bool,
    ipid: bool,
    pid: u16,
    payload: Vec<u8>,
    number_of_packets: u8,
    packets_received: u8,
}

impl MessageAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    fn reset(&mut self) {
        *self = Self::default();
    }

    /// Feeds one AVCTP packet; returns the completed message once the last
    /// fragment arrives, or `None` while incomplete (or on a dropped packet).
    pub fn on_pdu(&mut self, pdu: &[u8]) -> Option<Message> {
        self.packets_received = self.packets_received.saturating_add(1);

        let first = *pdu.first()?;
        let transaction_label = first >> 4;
        let packet_type = PacketType::from_bits(first >> 2);
        let is_command = (first >> 1) & 1 == 0;
        let ipid = first & 1 != 0;

        // IPID is only meaningful in responses (AVCTP spec 1.4, 6.1.2).
        if is_command && ipid {
            self.reset();
            return None;
        }

        match packet_type {
            PacketType::Single | PacketType::Start => {
                // A SINGLE or START discards any unterminated predecessor
                // and starts fresh.
                self.reset();
                if packet_type == PacketType::Single {
                    let pid_bytes = pdu.get(1..3)?;
                    return Some(Message {
                        transaction_label,
                        is_command,
                        ipid,
                        pid: u16::from_be_bytes([pid_bytes[0], pid_bytes[1]]),
                        payload: pdu[3..].to_vec(),
                    });
                }
                self.packets_received = 1;
                self.number_of_packets = *pdu.get(1)?;
                let pid_bytes = pdu.get(2..4)?;
                self.in_progress = true;
                self.transaction_label = transaction_label;
                self.is_command = is_command;
                self.ipid = ipid;
                self.pid = u16::from_be_bytes([pid_bytes[0], pid_bytes[1]]);
                self.payload = pdu[4..].to_vec();
                None
            }
            PacketType::Continue | PacketType::End => {
                if !self.in_progress
                    || transaction_label != self.transaction_label
                    || is_command != self.is_command
                    || self.packets_received > self.number_of_packets
                {
                    self.reset();
                    return None;
                }
                self.payload.extend_from_slice(&pdu[1..]);
                if packet_type == PacketType::Continue {
                    return None;
                }
                if self.packets_received != self.number_of_packets {
                    // Premature END.
                    self.reset();
                    return None;
                }
                let message = Message {
                    transaction_label: self.transaction_label,
                    is_command: self.is_command,
                    ipid: self.ipid,
                    pid: self.pid,
                    payload: std::mem::take(&mut self.payload),
                };
                self.reset();
                Some(message)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Protocol
// ---------------------------------------------------------------------------

/// Result of feeding one incoming packet into [`Protocol::receive`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AvctpEvent {
    /// A command arrived for a registered PID.
    Command {
        transaction_label: u8,
        pid: u16,
        payload: Vec<u8>,
    },
    /// A response arrived for one of our commands.
    Response {
        transaction_label: u8,
        pid: u16,
        payload: Vec<u8>,
    },
    /// The peer answered our command with IPID: it does not support `pid`.
    InvalidPid { transaction_label: u8, pid: u16 },
}

/// The AVCTP state machine for one L2CAP channel: reassembles incoming
/// messages and routes them by PID. Commands for PIDs no profile has
/// registered draw an automatic IPID response (AVCTP spec 1.4, section 5.2).
#[derive(Debug)]
pub struct Protocol {
    peer_mtu: u16,
    assembler: MessageAssembler,
    registered_pids: Vec<u16>,
}

impl Protocol {
    pub fn new(peer_mtu: u16) -> Self {
        Self {
            peer_mtu,
            assembler: MessageAssembler::new(),
            registered_pids: Vec::new(),
        }
    }

    /// Declares `pid` as served locally, so incoming commands for it are
    /// delivered instead of drawing an IPID response.
    pub fn register_pid(&mut self, pid: u16) {
        if !self.registered_pids.contains(&pid) {
            self.registered_pids.push(pid);
        }
    }

    /// Builds the packets for one command message.
    pub fn send_command(&self, transaction_label: u8, pid: u16, payload: &[u8]) -> Vec<Vec<u8>> {
        write_message(transaction_label, true, false, pid, payload, self.peer_mtu)
    }

    /// Builds the packets for one response message.
    pub fn send_response(&self, transaction_label: u8, pid: u16, payload: &[u8]) -> Vec<Vec<u8>> {
        write_message(transaction_label, false, false, pid, payload, self.peer_mtu)
    }

    /// Builds the IPID (invalid PID) response for a command we cannot serve.
    pub fn send_ipid_response(&self, transaction_label: u8, pid: u16) -> Vec<Vec<u8>> {
        write_message(transaction_label, false, true, pid, &[], self.peer_mtu)
    }

    /// Processes one incoming packet, returning packets to send back plus
    /// any events.
    pub fn receive(&mut self, pdu: &[u8]) -> (Vec<Vec<u8>>, Vec<AvctpEvent>) {
        let Some(message) = self.assembler.on_pdu(pdu) else {
            return (Vec::new(), Vec::new());
        };
        if message.is_command {
            if !self.registered_pids.contains(&message.pid) {
                return (
                    self.send_ipid_response(message.transaction_label, message.pid),
                    Vec::new(),
                );
            }
            return (
                Vec::new(),
                vec![AvctpEvent::Command {
                    transaction_label: message.transaction_label,
                    pid: message.pid,
                    payload: message.payload,
                }],
            );
        }
        let event = if message.ipid {
            AvctpEvent::InvalidPid {
                transaction_label: message.transaction_label,
                pid: message.pid,
            }
        } else {
            AvctpEvent::Response {
                transaction_label: message.transaction_label,
                pid: message.pid,
                payload: message.payload,
            }
        };
        (Vec::new(), vec![event])
    }
}

// ---------------------------------------------------------------------------
// L2CAP integration
// ---------------------------------------------------------------------------

/// Registers the AVCTP control PSM as accepting incoming connections.
pub fn register_control_server(manager: &mut ClassicChannelManager) -> Result<(), SimbleError> {
    manager.register_server(AVCTP_PSM)
}

/// Registers the AVCTP browsing PSM as accepting incoming connections.
pub fn register_browsing_server(manager: &mut ClassicChannelManager) -> Result<(), SimbleError> {
    manager.register_server(AVCTP_BROWSING_PSM)
}

/// Allocates the AVCTP control L2CAP channel (initiator role), returning
/// its local CID plus the `Connection Request` to send.
pub fn connect_control_channel(
    manager: &mut ClassicChannelManager,
    mtu: u16,
) -> Result<(u16, ConnectionRequestHeader), SimbleError> {
    manager.connect(&ClassicChannelSpec::with_mtu(AVCTP_PSM, mtu))
}

/// Allocates the AVCTP browsing L2CAP channel (initiator role); see
/// [`connect_control_channel`].
pub fn connect_browsing_channel(
    manager: &mut ClassicChannelManager,
    mtu: u16,
) -> Result<(u16, ConnectionRequestHeader), SimbleError> {
    manager.connect(&ClassicChannelSpec::with_mtu(AVCTP_BROWSING_PSM, mtu))
}
