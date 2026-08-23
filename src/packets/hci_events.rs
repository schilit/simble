// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Zero-copy views over the HCI events a host consumes (Core Spec Vol 4,
//! Part E, Section 7.7), plus the parser that turns an H4 event packet into
//! a typed [`HciEvent`].
//!
//! These exist because index-based parsing (`packet[8]`) silently drops or
//! misreads fields: a dropped Peer_Address_Type broke SMP pairing against
//! real stacks, since the type is mixed into the pairing crypto. A typed
//! layout makes that class of bug unrepresentable.

use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned, byteorder::little_endian::U16,
};

/// HCI event codes (Vol 4, Part E, Section 7.7).
pub mod event_code {
    /// Connection Complete (BR/EDR).
    pub const CONNECTION_COMPLETE: u8 = 0x03;
    /// Connection Request (BR/EDR) — a peer is paging us.
    pub const CONNECTION_REQUEST: u8 = 0x04;
    /// Disconnection Complete.
    pub const DISCONNECTION_COMPLETE: u8 = 0x05;
    /// Encryption Change.
    pub const ENCRYPTION_CHANGE: u8 = 0x08;
    /// Command Complete.
    pub const COMMAND_COMPLETE: u8 = 0x0E;
    /// Command Status — a command the controller accepted but has not yet
    /// finished (LE Create BIG and LE BIG Create Sync both answer this way).
    pub const COMMAND_STATUS: u8 = 0x0F;
    /// Synchronous Connection Complete (BR/EDR) — a SCO/eSCO link is up, or
    /// failed. This is the only event that hands a host the *audio* handle;
    /// a host that watches only Connection Complete never learns it.
    pub const SYNCHRONOUS_CONNECTION_COMPLETE: u8 = 0x2C;
    /// LE Meta (the subevent code selects the LE event).
    pub const LE_META: u8 = 0x3E;
}

/// LE Meta subevent codes (Vol 4, Part E, Section 7.7.65).
pub mod le_subevent {
    /// LE Connection Complete.
    pub const CONNECTION_COMPLETE: u8 = 0x01;
    /// LE Advertising Report.
    pub const ADVERTISING_REPORT: u8 = 0x02;
    /// LE Long Term Key Request.
    pub const LONG_TERM_KEY_REQUEST: u8 = 0x05;
    /// LE Enhanced Connection Complete (adds the resolved RPAs).
    pub const ENHANCED_CONNECTION_COMPLETE: u8 = 0x0A;
    /// LE CIS Established — the isochronous stream is up.
    pub const CIS_ESTABLISHED: u8 = 0x19;
    /// LE CIS Request — a central is asking to open a CIS to us.
    pub const CIS_REQUEST: u8 = 0x1A;
}

/// The 2-byte HCI event header that follows the H4 packet-type byte.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct HciEventHeader {
    /// Event code (see [`event_code`]).
    pub code: u8,
    /// Length of the event parameters that follow.
    pub parameter_total_length: u8,
}

/// The parameters shared by LE Connection Complete and its Enhanced variant,
/// up to and including the peer address. The Enhanced event inserts the
/// local/peer resolvable addresses after this prefix, so only these leading
/// fields are laid out identically — and they are all the host needs.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct LeConnectionCompletePrefix {
    /// Subevent code (0x01 or 0x0A).
    pub subevent_code: u8,
    /// 0x00 on success.
    pub status: u8,
    /// Connection handle (12 significant bits).
    pub connection_handle: U16,
    /// Local role: 0x00 central, 0x01 peripheral.
    pub role: u8,
    /// 0x00 public, 0x01 random; the Enhanced event adds 0x02/0x03 for a
    /// resolved identity address (public/random respectively).
    pub peer_address_type: u8,
    /// Peer address, little-endian on the wire.
    pub peer_address: [u8; 6],
}

/// LE Long Term Key Request parameters (Vol 4, Part E, Section 7.7.65.5):
/// the controller asking the host for the key that starts encryption.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct LeLongTermKeyRequest {
    /// Subevent code (0x05).
    pub subevent_code: u8,
    /// Connection handle the key is wanted for.
    pub connection_handle: U16,
    /// Random number from the peer's encryption start.
    pub random_number: [u8; 8],
    /// Encrypted diversifier from the peer's encryption start.
    pub encrypted_diversifier: U16,
}

/// Encryption Change parameters (Vol 4, Part E, Section 7.7.8).
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct EncryptionChange {
    /// 0x00 on success.
    pub status: u8,
    /// The connection whose encryption state changed.
    pub connection_handle: U16,
    /// 0x00 off, non-zero on (E0/AES-CCM).
    pub encryption_enabled: u8,
}

/// Disconnection Complete parameters (Vol 4, Part E, Section 7.7.5).
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct DisconnectionComplete {
    /// 0x00 on success.
    pub status: u8,
    /// The connection that ended.
    pub connection_handle: U16,
    /// Disconnect reason code.
    pub reason: u8,
}

/// Connection Request parameters (Vol 4, Part E, Section 7.7.4): a BR/EDR
/// peer is paging this device. The host answers with Accept (or Reject)
/// Connection Request — a peripheral that never answers is never connectable.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct ConnectionRequest {
    /// The paging device's address, little-endian on the wire.
    pub bd_addr: [u8; 6],
    /// The peer's Class of Device (3 octets).
    pub class_of_device: [u8; 3],
    /// 0x01 ACL, 0x00/0x02 SCO/eSCO.
    pub link_type: u8,
}

/// Connection Complete parameters (Vol 4, Part E, Section 7.7.3) — the BR/EDR
/// counterpart of LE Connection Complete.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct ConnectionComplete {
    /// 0x00 on success.
    pub status: u8,
    /// The new connection handle.
    pub connection_handle: U16,
    /// The peer's address, little-endian on the wire.
    pub bd_addr: [u8; 6],
    /// 0x01 ACL, 0x00 SCO.
    pub link_type: u8,
    /// Whether link-level encryption is on.
    pub encryption_enabled: u8,
}

/// Synchronous Connection Complete parameters (Vol 4, Part E, Section
/// 7.7.35): the SCO/eSCO link a Setup or Accept Synchronous Connection
/// asked for is up, or failed with `status`.
///
/// The handle here is the **synchronous** link's own, not the ACL handle it
/// was set up over. HCI SCO data packets are addressed to this one, and a
/// host that reuses the ACL handle for audio sends it into a channel that
/// drops it without complaint.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct SynchronousConnectionComplete {
    /// 0x00 on success; otherwise why the link was not made.
    pub status: u8,
    /// The synchronous link's connection handle.
    pub connection_handle: U16,
    /// The peer's address, little-endian on the wire.
    pub bd_addr: [u8; 6],
    /// 0x00 SCO, 0x02 eSCO.
    pub link_type: u8,
    /// Transmission interval, in baseband slots.
    pub transmission_interval: u8,
    /// Retransmission window, in baseband slots.
    pub retransmission_window: u8,
    /// Receive packet length, in octets.
    pub rx_packet_length: U16,
    /// Transmit packet length, in octets.
    pub tx_packet_length: U16,
    /// Air mode: 0x00 μ-law, 0x01 A-law, 0x02 CVSD, 0x03 transparent. These
    /// are *not* the Voice Setting's air coding format numbers.
    pub air_mode: u8,
}

/// LE CIS Request parameters (Vol 4, Part E, Section 7.7.65.26): a central
/// wants to open an isochronous stream on an existing ACL connection. The
/// peripheral answers with LE Accept CIS Request (or rejects).
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct LeCisRequest {
    /// Subevent code (0x1A).
    pub subevent_code: u8,
    /// The ACL connection the stream belongs to.
    pub acl_connection_handle: U16,
    /// The handle the CIS will use — ISO data is addressed to this, not to
    /// the ACL handle.
    pub cis_connection_handle: U16,
    /// Which CIG the stream belongs to.
    pub cig_id: u8,
    /// Which CIS within the group.
    pub cis_id: u8,
}

/// LE CIS Established parameters (Vol 4, Part E, Section 7.7.65.25). Only
/// the leading fields matter to a host setting up its data path; the timing
/// parameters that follow are the controller's business.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct LeCisEstablishedPrefix {
    /// Subevent code (0x19).
    pub subevent_code: u8,
    /// 0x00 on success.
    pub status: u8,
    /// The established stream's handle — ISO SDUs travel on this.
    pub cis_connection_handle: U16,
}

/// The fixed part of a Command Complete event (Vol 4, Part E, Section
/// 7.7.14). The return parameters that follow are command-specific — LE Set
/// CIG Parameters answers with the CIS handles a central then creates
/// streams on, which is the only way to learn them.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct CommandCompletePrefix {
    /// How many further command packets the controller can accept.
    pub num_hci_command_packets: u8,
    /// Which command completed.
    pub command_opcode: U16,
}

/// The fixed part of one LE Advertising Report (Vol 4, Part E, Section
/// 7.7.65.2); the AD data and a trailing RSSI byte follow it.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct LeAdvertisingReportHeader {
    /// Advertising event type: 0x00 ADV_IND, 0x01 ADV_DIRECT_IND,
    /// 0x02 ADV_SCAN_IND, 0x03 ADV_NONCONN_IND, 0x04 SCAN_RSP.
    pub event_type: u8,
    /// 0x00 public, 0x01 random.
    pub address_type: u8,
    /// Advertiser address, little-endian on the wire.
    pub address: [u8; 6],
    /// Length of the AD data that follows.
    pub data_length: u8,
}

impl LeAdvertisingReportHeader {
    /// ADV_IND and ADV_DIRECT_IND are the connectable event types.
    pub fn is_connectable(&self) -> bool {
        self.event_type <= 0x01
    }

    /// Whether this report is a scan response rather than an advertisement.
    pub fn is_scan_response(&self) -> bool {
        self.event_type == 0x04
    }
}

/// One advertising report: its typed header, the AD data, and the RSSI.
#[derive(Debug, PartialEq, Eq)]
pub struct AdvertisingReport<'a> {
    /// Event type, address, and data length.
    pub header: &'a LeAdvertisingReportHeader,
    /// The advertising (or scan response) data.
    pub data: &'a [u8],
    /// Received signal strength, in dBm.
    pub rssi: i8,
}

/// Walks the reports packed into an LE Advertising Report event's
/// parameters (subevent code, report count, then that many reports).
/// Stops at the first truncated report rather than misreading past it.
pub fn advertising_reports(parameters: &[u8]) -> Vec<AdvertisingReport<'_>> {
    let mut reports = Vec::new();
    let Some((&count, mut rest)) = parameters.get(1..).and_then(<[u8]>::split_first) else {
        return reports;
    };
    for _ in 0..count {
        let Ok((header, after)) = LeAdvertisingReportHeader::ref_from_prefix(rest) else {
            break;
        };
        let data_len = usize::from(header.data_length);
        // The report needs its declared data plus one RSSI byte.
        if after.len() < data_len + 1 {
            break;
        }
        reports.push(AdvertisingReport {
            header,
            data: &after[..data_len],
            rssi: after[data_len] as i8,
        });
        rest = &after[data_len + 1..];
    }
    reports
}

/// One parsed HCI event: a typed view borrowed from the packet buffer.
#[derive(Debug, PartialEq, Eq)]
pub enum HciEvent<'a> {
    /// LE Connection Complete or its Enhanced variant.
    LeConnectionComplete(&'a LeConnectionCompletePrefix),
    /// LE Long Term Key Request.
    LeLongTermKeyRequest(&'a LeLongTermKeyRequest),
    /// Encryption Change.
    EncryptionChange(&'a EncryptionChange),
    /// Disconnection Complete.
    DisconnectionComplete(&'a DisconnectionComplete),
    /// LE CIS Request — a central wants to open an isochronous stream.
    LeCisRequest(&'a LeCisRequest),
    /// LE CIS Established — the stream is up (or failed, see `status`).
    LeCisEstablished(&'a LeCisEstablishedPrefix),
    /// Connection Request (BR/EDR).
    ConnectionRequest(&'a ConnectionRequest),
    /// Connection Complete (BR/EDR).
    ConnectionComplete(&'a ConnectionComplete),
    /// Synchronous Connection Complete (BR/EDR) — the call-audio link.
    SynchronousConnectionComplete(&'a SynchronousConnectionComplete),
    /// Command Complete, split into its fixed header and the completed
    /// command's own return parameters.
    CommandComplete {
        /// Which command completed, and the controller's command credit.
        header: &'a CommandCompletePrefix,
        /// Return parameters, whose layout depends on the opcode.
        return_parameters: &'a [u8],
    },
    /// Any other event, with its raw parameters (LE Meta events keep their
    /// subevent code as the first parameter byte).
    Other {
        /// The event code.
        code: u8,
        /// The event's parameters, as received.
        parameters: &'a [u8],
    },
}

impl<'a> HciEvent<'a> {
    /// Parses an HCI event's parameters (no H4 type byte, no event header).
    /// Returns `None` only if a recognized event is too short to be valid.
    fn from_parameters(code: u8, parameters: &'a [u8]) -> Option<Self> {
        let event = match code {
            event_code::LE_META => match parameters.first() {
                Some(&le_subevent::CONNECTION_COMPLETE)
                | Some(&le_subevent::ENHANCED_CONNECTION_COMPLETE) => Self::LeConnectionComplete(
                    LeConnectionCompletePrefix::ref_from_prefix(parameters)
                        .ok()?
                        .0,
                ),
                Some(&le_subevent::LONG_TERM_KEY_REQUEST) => Self::LeLongTermKeyRequest(
                    LeLongTermKeyRequest::ref_from_prefix(parameters).ok()?.0,
                ),
                Some(&le_subevent::CIS_REQUEST) => {
                    Self::LeCisRequest(LeCisRequest::ref_from_prefix(parameters).ok()?.0)
                }
                Some(&le_subevent::CIS_ESTABLISHED) => Self::LeCisEstablished(
                    LeCisEstablishedPrefix::ref_from_prefix(parameters).ok()?.0,
                ),
                _ => Self::Other { code, parameters },
            },
            event_code::COMMAND_COMPLETE => {
                let (header, return_parameters) =
                    CommandCompletePrefix::ref_from_prefix(parameters).ok()?;
                Self::CommandComplete {
                    header,
                    return_parameters,
                }
            }
            event_code::ENCRYPTION_CHANGE => {
                Self::EncryptionChange(EncryptionChange::ref_from_prefix(parameters).ok()?.0)
            }
            event_code::DISCONNECTION_COMPLETE => Self::DisconnectionComplete(
                DisconnectionComplete::ref_from_prefix(parameters).ok()?.0,
            ),
            event_code::CONNECTION_REQUEST => {
                Self::ConnectionRequest(ConnectionRequest::ref_from_prefix(parameters).ok()?.0)
            }
            event_code::CONNECTION_COMPLETE => {
                Self::ConnectionComplete(ConnectionComplete::ref_from_prefix(parameters).ok()?.0)
            }
            event_code::SYNCHRONOUS_CONNECTION_COMPLETE => Self::SynchronousConnectionComplete(
                SynchronousConnectionComplete::ref_from_prefix(parameters)
                    .ok()?
                    .0,
            ),
            _ => Self::Other { code, parameters },
        };
        Some(event)
    }

    /// Parses a complete H4 event packet (leading `0x04` type byte, event
    /// header, then parameters). `None` if it isn't a well-formed event.
    pub fn parse_h4(packet: &'a [u8]) -> Option<Self> {
        let (&h4_type, rest) = packet.split_first()?;
        if h4_type != crate::transport::h4_type::HCI_EVENT {
            return None;
        }
        let (header, parameters) = HciEventHeader::ref_from_prefix(rest).ok()?;
        // Trust the declared length when it fits, so trailing bytes (padding
        // from a fixed-size buffer) never leak into a parsed view.
        let len = usize::from(header.parameter_total_length).min(parameters.len());
        Self::from_parameters(header.code, &parameters[..len])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_le_connection_complete_keeps_address_and_type() {
        // The bug this type exists to prevent: a dropped Peer_Address_Type.
        let mut packet = vec![0x04, 0x3E, 0x13, 0x01, 0x00, 0x40, 0x00, 0x01, 0x01];
        packet.extend_from_slice(&[0xB9, 0x62, 0xF7, 0xD6, 0x79, 0x7C]); // peer, LE order
        packet.extend_from_slice(&[0x18, 0x00, 0x00, 0x00, 0x48, 0x00, 0x00]);
        let Some(HciEvent::LeConnectionComplete(event)) = HciEvent::parse_h4(&packet) else {
            panic!("should parse as a connection complete");
        };
        assert_eq!(event.status, 0x00);
        assert_eq!(event.connection_handle.get(), 0x0040);
        assert_eq!(event.peer_address_type, 0x01, "random address type");
        assert_eq!(event.peer_address, [0xB9, 0x62, 0xF7, 0xD6, 0x79, 0x7C]);
    }

    #[test]
    fn test_parse_ltk_request_and_encryption_change() {
        let mut ltk = vec![0x04, 0x3E, 0x0D, 0x05, 0x40, 0x00];
        ltk.extend_from_slice(&[0xAA; 8]); // random number
        ltk.extend_from_slice(&[0x34, 0x12]); // ediv
        let Some(HciEvent::LeLongTermKeyRequest(event)) = HciEvent::parse_h4(&ltk) else {
            panic!("should parse as an LTK request");
        };
        assert_eq!(event.connection_handle.get(), 0x0040);
        assert_eq!(event.encrypted_diversifier.get(), 0x1234);

        let enc = [0x04, 0x08, 0x04, 0x00, 0x40, 0x00, 0x01];
        let Some(HciEvent::EncryptionChange(event)) = HciEvent::parse_h4(&enc) else {
            panic!("should parse as an encryption change");
        };
        assert_eq!(event.status, 0);
        assert_eq!(event.connection_handle.get(), 0x0040);
        assert_eq!(event.encryption_enabled, 1);
    }

    #[test]
    fn test_advertising_reports_walk_packed_reports() {
        // Two reports in one event: 3 bytes of AD data each.
        let mut params = vec![le_subevent::ADVERTISING_REPORT, 0x02];
        for (addr_byte, rssi) in [(0xAA, 0xC3u8), (0xBB, 0xB0u8)] {
            params.extend_from_slice(&[0x00, 0x01]); // ADV_IND, random
            params.extend_from_slice(&[addr_byte; 6]);
            params.push(0x03);
            params.extend_from_slice(&[0x02, 0x01, 0x06]); // flags AD
            params.push(rssi);
        }
        let reports = advertising_reports(&params);
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].header.address, [0xAA; 6]);
        assert!(reports[0].header.is_connectable());
        assert_eq!(reports[0].data, &[0x02, 0x01, 0x06]);
        assert_eq!(reports[0].rssi, -61);
        assert_eq!(reports[1].header.address, [0xBB; 6]);
        assert_eq!(reports[1].rssi, -80);

        // A truncated second report is dropped, not misread.
        let truncated = &params[..params.len() - 4];
        assert_eq!(advertising_reports(truncated).len(), 1);
    }

    #[test]
    fn test_short_and_foreign_packets_are_rejected() {
        // Truncated connection complete: no silent misread.
        assert!(HciEvent::parse_h4(&[0x04, 0x3E, 0x04, 0x01, 0x00, 0x40]).is_none());
        // ACL data, not an event.
        assert!(HciEvent::parse_h4(&[0x02, 0x40, 0x00]).is_none());
        // Unknown event code still parses, as Other.
        assert!(matches!(
            HciEvent::parse_h4(&[0x04, 0x7F, 0x01, 0x01]),
            Some(HciEvent::Other { code: 0x7F, .. })
        ));
        // A Command Complete too short to hold its own header is rejected
        // rather than read as an opcode that isn't there.
        assert!(HciEvent::parse_h4(&[0x04, 0x0E, 0x01, 0x01]).is_none());
    }

    #[test]
    fn test_command_complete_splits_header_from_return_parameters() {
        // LE Set CIG Parameters completing: one command credit, opcode
        // 0x2062, then status / CIG_ID / CIS_Count / one CIS handle.
        let packet = [
            0x04, 0x0E, 0x08, 0x01, 0x62, 0x20, 0x00, 0x01, 0x01, 0x00, 0x0E,
        ];
        let Some(HciEvent::CommandComplete {
            header,
            return_parameters,
        }) = HciEvent::parse_h4(&packet)
        else {
            panic!("expected a Command Complete");
        };
        assert_eq!(header.command_opcode.get(), 0x2062);
        assert_eq!(header.num_hci_command_packets, 1);
        assert_eq!(return_parameters, &[0x00, 0x01, 0x01, 0x00, 0x0E]);
    }
}
