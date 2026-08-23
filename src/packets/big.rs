// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Broadcast Isochronous Group (BIG) HCI packets — the connectionless half of
//! LE Audio's media plane, and the transport underneath Auracast (Core Spec
//! 5.2, Vol 4, Part E, Sections 7.8.103-7.8.107 commands / 7.7.65.27-7.7.65.30
//! and 7.7.65.34 LE Meta Event subevents).
//!
//! A BIG is the broadcast counterpart of a CIG: one advertising set's periodic
//! advertising train carries the BIGInfo that lets any number of receivers
//! synchronize, and each Broadcast Isochronous Stream (BIS) inside the group
//! gets its own connection handle to write SDUs on. There is no ACL link and
//! no acknowledgement anywhere in the picture, which is why the *broadcaster*
//! side needs periodic advertising running first (`LE Create BIG` takes an
//! advertising handle, not a connection handle) and the *receiver* side needs
//! a periodic advertising sync handle before it can ask to join.
//!
//! Layout style follows [`crate::packets::ext_adv`], the module these sit next
//! to: zero-copy `#[repr(C)]` `Unaligned` structs, `HciCommand::OP_CODE` for
//! the command opcode, and — for the two variable-length structures, `LE BIG
//! Create Sync`'s trailing BIS index array and the two Complete events'
//! trailing connection-handle arrays — a fixed header plus a `parse` that
//! hands back the trailer as a borrowed slice.
//!
//! Field layouts were cross-checked against Bumble's `hci.py`
//! (`HCI_LE_Create_BIG_Command` and friends), whose encoder produces
//! byte-identical parameter blocks; see the tests at the bottom, which assert
//! against captures taken from it.

use crate::packets::ext_adv::U24;
use crate::packets::hci::{HciCommand, OpCode};
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, Ref, Unaligned,
    byteorder::{LittleEndian, U16},
};

/// BIG OpCodes (OGF 0x08, LE Controller Commands).
///
/// OCFs per Bluetooth Core Spec Vol 4, Part E, Sections 7.8.103-7.8.107.
/// `LE Create BIG Test` (OCF 0x0069) is deliberately absent: it exists only to
/// drive a controller's ISO test mode, and netsim's rootcanal does not
/// advertise support for it in Read Local Supported Commands.
pub mod big_opcode {
    use super::OpCode;

    /// 7.8.103 LE Create BIG.
    pub const LE_CREATE_BIG: OpCode = OpCode::from_bytes([0x68, 0x20]);
    /// 7.8.105 LE Terminate BIG.
    pub const LE_TERMINATE_BIG: OpCode = OpCode::from_bytes([0x6A, 0x20]);
    /// 7.8.106 LE BIG Create Sync.
    pub const LE_BIG_CREATE_SYNC: OpCode = OpCode::from_bytes([0x6B, 0x20]);
    /// 7.8.107 LE BIG Terminate Sync.
    pub const LE_BIG_TERMINATE_SYNC: OpCode = OpCode::from_bytes([0x6C, 0x20]);
}

/// BIG LE Meta Subevent Codes (Event Code 0x3E).
///
/// Per Bluetooth Core Spec Vol 4, Part E, Sections 7.7.65.27-7.7.65.30 and
/// 7.7.65.34.
pub mod big_subevent_code {
    /// 7.7.65.27 LE Create BIG Complete.
    pub const LE_CREATE_BIG_COMPLETE: u8 = 0x1B;
    /// 7.7.65.28 LE Terminate BIG Complete.
    pub const LE_TERMINATE_BIG_COMPLETE: u8 = 0x1C;
    /// 7.7.65.29 LE BIG Sync Established.
    pub const LE_BIG_SYNC_ESTABLISHED: u8 = 0x1D;
    /// 7.7.65.30 LE BIG Sync Lost.
    pub const LE_BIG_SYNC_LOST: u8 = 0x1E;
    /// 7.7.65.34 LE BIGInfo Advertising Report.
    pub const LE_BIGINFO_ADVERTISING_REPORT: u8 = 0x22;
}

/// Packing of BISes within an isochronous event (7.8.103: Packing).
pub mod big_packing {
    /// All subevents of one BIS before the next BIS's.
    pub const SEQUENTIAL: u8 = 0x00;
    /// One subevent of each BIS before the next subevent.
    pub const INTERLEAVED: u8 = 0x01;
}

/// Framing of the isochronous data (7.8.103: Framing).
pub mod big_framing {
    /// Unframed — the SDU interval is a whole multiple of the ISO interval.
    pub const UNFRAMED: u8 = 0x00;
    /// Framed — SDUs carry their own segmentation headers.
    pub const FRAMED: u8 = 0x01;
}

/// Encryption of the BIS payloads (7.8.103 / 7.8.106: Encryption).
pub mod big_encryption {
    /// The BISes are not encrypted; the broadcast code is ignored.
    pub const UNENCRYPTED: u8 = 0x00;
    /// The BISes are encrypted with the accompanying broadcast code.
    pub const ENCRYPTED: u8 = 0x01;
}

/// LE Create BIG Command (7.8.103).
///
/// Issued by a broadcaster once its advertising set has periodic advertising
/// enabled. The controller answers Command Status, then reports the BIS
/// connection handles in [`LeCreateBigCompleteEventHeader`] — which is the
/// only way for the host to learn them.
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeCreateBig {
    /// Identifier of the BIG being created.
    pub big_handle: u8,
    /// Advertising set whose periodic advertising train carries the BIGInfo.
    pub advertising_handle: u8,
    /// Number of BISes in the group (1..=0x1F).
    pub num_bis: u8,
    /// Time between SDUs on each BIS, in microseconds.
    pub sdu_interval: U24, // Microseconds, 0x0000FF-0x0FFFFF.
    /// Largest SDU the host will write to a BIS, in octets.
    pub max_sdu: U16<LittleEndian>,
    /// Transport latency budget, in milliseconds.
    pub max_transport_latency: U16<LittleEndian>, // Milliseconds.
    /// Retransmission number (0x00-0x1E).
    pub rtn: u8,
    /// PHY bitfield: 0x01 = 1M, 0x02 = 2M, 0x04 = Coded.
    pub phy: u8,
    /// Sequential or interleaved packing (see [`big_packing`]).
    pub packing: u8,
    /// Framed or unframed (see [`big_framing`]).
    pub framing: u8,
    /// Whether the BISes are encrypted (see [`big_encryption`]).
    pub encryption: u8,
    /// The 16-octet broadcast code, meaningful only when `encryption` is set.
    pub broadcast_code: [u8; 16],
}

impl HciCommand for LeCreateBig {
    const OP_CODE: OpCode = big_opcode::LE_CREATE_BIG;
}

/// LE Terminate BIG Command (7.8.105).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeTerminateBig {
    /// Identifier of the BIG to tear down.
    pub big_handle: u8,
    /// Reason code reported to the host in LE Terminate BIG Complete.
    pub reason: u8,
}

impl HciCommand for LeTerminateBig {
    const OP_CODE: OpCode = big_opcode::LE_TERMINATE_BIG;
}

/// LE BIG Create Sync Command Header (7.8.106).
///
/// Followed by `num_bis` one-octet BIS indices (1-based, as they appear in the
/// BASE), which is why this is a header rather than a whole command struct —
/// the same shape [`crate::packets::ext_adv::LeSetPeriodicAdvertisingDataHeader`]
/// uses for its trailing data.
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeBigCreateSyncHeader {
    /// Identifier the host assigns to the BIG it is joining.
    pub big_handle: u8,
    /// Periodic advertising sync handle the BIGInfo arrived on.
    pub sync_handle: U16<LittleEndian>,
    /// Whether the BISes are encrypted (see [`big_encryption`]).
    pub encryption: u8,
    /// The 16-octet broadcast code, meaningful only when `encryption` is set.
    pub broadcast_code: [u8; 16],
    /// Maximum number of subevents the receiver may use (0 = no preference).
    pub mse: u8,
    /// Synchronization timeout, in units of 10 ms.
    pub big_sync_timeout: U16<LittleEndian>, // Units of 10 ms.
    /// Number of BIS indices that follow.
    pub num_bis: u8,
}

impl HciCommand for LeBigCreateSyncHeader {
    const OP_CODE: OpCode = big_opcode::LE_BIG_CREATE_SYNC;
}

impl LeBigCreateSyncHeader {
    /// Parses the fixed header and the trailing BIS index array.
    pub fn parse(bytes: &[u8]) -> Option<(Ref<&[u8], Self>, &[u8])> {
        let (header, rest) = Ref::<&[u8], Self>::from_prefix(bytes).ok()?;
        let expected = header.num_bis as usize;
        if rest.len() < expected {
            return None;
        }
        Some((header, &rest[..expected]))
    }

    /// Serializes the command: fixed header followed by the BIS indices.
    ///
    /// `bis` must be non-empty and hold 1-based BIS indices taken from the
    /// BASE; a controller answers Invalid HCI Command Parameters otherwise.
    pub fn serialize(
        big_handle: u8,
        sync_handle: u16,
        encryption: u8,
        broadcast_code: [u8; 16],
        mse: u8,
        big_sync_timeout: u16,
        bis: &[u8],
    ) -> Vec<u8> {
        let header = Self {
            big_handle,
            sync_handle: U16::new(sync_handle),
            encryption,
            broadcast_code,
            mse,
            big_sync_timeout: U16::new(big_sync_timeout),
            num_bis: bis.len() as u8,
        };
        let mut buf = Vec::with_capacity(size_of::<Self>() + bis.len());
        buf.extend_from_slice(header.as_bytes());
        buf.extend_from_slice(bis);
        buf
    }
}

/// LE BIG Terminate Sync Command (7.8.107).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeBigTerminateSync {
    /// Identifier of the synchronized BIG to leave.
    pub big_handle: u8,
}

impl HciCommand for LeBigTerminateSync {
    const OP_CODE: OpCode = big_opcode::LE_BIG_TERMINATE_SYNC;
}

/// Return parameters of LE BIG Terminate Sync (7.8.107). Unlike the other
/// three, this command completes with Command Complete rather than Command
/// Status.
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeBigTerminateSyncResponse {
    /// HCI status code.
    pub status: u8,
    /// Identifier of the BIG whose sync was terminated.
    pub big_handle: u8,
}

/// LE Create BIG Complete Event Header (Subevent 0x1B, 7.7.65.27).
///
/// Followed by `num_bis` two-octet BIS connection handles. As in
/// [`crate::packets::ext_adv`], `parse` takes the subevent parameters *after*
/// the subevent code octet.
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeCreateBigCompleteEventHeader {
    /// HCI status code.
    pub status: u8,
    /// Identifier of the BIG that was created.
    pub big_handle: u8,
    /// Maximum delay from an SDU's reference point to its transmission, in µs.
    pub big_sync_delay: U24, // Microseconds.
    /// Maximum transport latency actually achieved, in µs.
    pub transport_latency_big: U24, // Microseconds.
    /// PHY the BIG runs on: 0x01 = 1M, 0x02 = 2M, 0x03 = Coded.
    pub phy: u8,
    /// Number of subevents in each isochronous interval.
    pub nse: u8,
    /// Burst number — payloads per BIS per interval.
    pub bn: u8,
    /// Pre-transmission offset.
    pub pto: u8,
    /// Immediate repetition count.
    pub irc: u8,
    /// Maximum PDU size on a BIS, in octets.
    pub max_pdu: U16<LittleEndian>,
    /// Isochronous interval, in units of 1.25 ms.
    pub iso_interval: U16<LittleEndian>, // Units of 1.25 ms.
    /// Number of connection handles that follow.
    pub num_bis: u8,
}

impl LeCreateBigCompleteEventHeader {
    /// Parses the fixed header and the trailing BIS connection handles.
    pub fn parse(bytes: &[u8]) -> Option<(Ref<&[u8], Self>, Vec<u16>)> {
        let (header, rest) = Ref::<&[u8], Self>::from_prefix(bytes).ok()?;
        let handles = read_handles(rest, header.num_bis)?;
        Some((header, handles))
    }

    /// Serializes the subevent parameters (no subevent code octet): fixed
    /// header followed by the BIS connection handles.
    #[allow(clippy::too_many_arguments)]
    pub fn serialize(
        status: u8,
        big_handle: u8,
        big_sync_delay: u32,
        transport_latency_big: u32,
        phy: u8,
        nse: u8,
        bn: u8,
        pto: u8,
        irc: u8,
        max_pdu: u16,
        iso_interval: u16,
        handles: &[u16],
    ) -> Vec<u8> {
        let header = Self {
            status,
            big_handle,
            big_sync_delay: U24::new(big_sync_delay),
            transport_latency_big: U24::new(transport_latency_big),
            phy,
            nse,
            bn,
            pto,
            irc,
            max_pdu: U16::new(max_pdu),
            iso_interval: U16::new(iso_interval),
            num_bis: handles.len() as u8,
        };
        let mut buf = Vec::with_capacity(size_of::<Self>() + 2 * handles.len());
        buf.extend_from_slice(header.as_bytes());
        write_handles(&mut buf, handles);
        buf
    }
}

/// LE Terminate BIG Complete Event (Subevent 0x1C, 7.7.65.28).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeTerminateBigCompleteEvent {
    /// Identifier of the BIG that was torn down.
    pub big_handle: u8,
    /// Why it was torn down (the host's own reason, echoed back).
    pub reason: u8,
}

/// LE BIG Sync Established Event Header (Subevent 0x1D, 7.7.65.29).
///
/// Followed by `num_bis` two-octet BIS connection handles. Note the missing
/// `big_sync_delay`: a receiver is told the transport latency but not the
/// broadcaster's transmission delay, so this is *not* the same layout as
/// [`LeCreateBigCompleteEventHeader`] minus a field.
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeBigSyncEstablishedEventHeader {
    /// HCI status code.
    pub status: u8,
    /// Identifier of the BIG that was joined.
    pub big_handle: u8,
    /// Maximum transport latency, in µs.
    pub transport_latency_big: U24, // Microseconds.
    /// Number of subevents in each isochronous interval.
    pub nse: u8,
    /// Burst number — payloads per BIS per interval.
    pub bn: u8,
    /// Pre-transmission offset.
    pub pto: u8,
    /// Immediate repetition count.
    pub irc: u8,
    /// Maximum PDU size on a BIS, in octets.
    pub max_pdu: U16<LittleEndian>,
    /// Isochronous interval, in units of 1.25 ms.
    pub iso_interval: U16<LittleEndian>, // Units of 1.25 ms.
    /// Number of connection handles that follow.
    pub num_bis: u8,
}

impl LeBigSyncEstablishedEventHeader {
    /// Parses the fixed header and the trailing BIS connection handles.
    pub fn parse(bytes: &[u8]) -> Option<(Ref<&[u8], Self>, Vec<u16>)> {
        let (header, rest) = Ref::<&[u8], Self>::from_prefix(bytes).ok()?;
        let handles = read_handles(rest, header.num_bis)?;
        Some((header, handles))
    }

    /// Serializes the subevent parameters (no subevent code octet): fixed
    /// header followed by the BIS connection handles.
    #[allow(clippy::too_many_arguments)]
    pub fn serialize(
        status: u8,
        big_handle: u8,
        transport_latency_big: u32,
        nse: u8,
        bn: u8,
        pto: u8,
        irc: u8,
        max_pdu: u16,
        iso_interval: u16,
        handles: &[u16],
    ) -> Vec<u8> {
        let header = Self {
            status,
            big_handle,
            transport_latency_big: U24::new(transport_latency_big),
            nse,
            bn,
            pto,
            irc,
            max_pdu: U16::new(max_pdu),
            iso_interval: U16::new(iso_interval),
            num_bis: handles.len() as u8,
        };
        let mut buf = Vec::with_capacity(size_of::<Self>() + 2 * handles.len());
        buf.extend_from_slice(header.as_bytes());
        write_handles(&mut buf, handles);
        buf
    }
}

/// LE BIG Sync Lost Event (Subevent 0x1E, 7.7.65.30).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeBigSyncLostEvent {
    /// Identifier of the BIG whose sync was lost.
    pub big_handle: u8,
    /// Reason the sync ended.
    pub reason: u8,
}

/// LE BIGInfo Advertising Report Event (Subevent 0x22, 7.7.65.34).
///
/// A receiver that is synchronized to a periodic advertising train gets one of
/// these each time the train's ACAD carries BIGInfo. It is the only place the
/// BIS count, the SDU interval and the encryption flag are announced *before*
/// syncing, so a receiver waits for it before issuing LE BIG Create Sync.
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeBigInfoAdvertisingReportEvent {
    /// Periodic advertising sync handle the BIGInfo arrived on.
    pub sync_handle: U16<LittleEndian>,
    /// Number of BISes in the advertised group.
    pub num_bis: u8,
    /// Number of subevents in each isochronous interval.
    pub nse: u8,
    /// Isochronous interval, in units of 1.25 ms.
    pub iso_interval: U16<LittleEndian>, // Units of 1.25 ms.
    /// Burst number.
    pub bn: u8,
    /// Pre-transmission offset.
    pub pto: u8,
    /// Immediate repetition count.
    pub irc: u8,
    /// Maximum PDU size on a BIS, in octets.
    pub max_pdu: U16<LittleEndian>,
    /// Time between SDUs, in microseconds.
    pub sdu_interval: U24, // Microseconds.
    /// Maximum SDU size, in octets.
    pub max_sdu: U16<LittleEndian>,
    /// PHY: 0x01 = 1M, 0x02 = 2M, 0x03 = Coded.
    pub phy: u8,
    /// Framed or unframed (see [`big_framing`]).
    pub framing: u8,
    /// Whether the BISes are encrypted (see [`big_encryption`]).
    pub encryption: u8,
}

/// Reads `count` little-endian connection handles from the front of `bytes`.
fn read_handles(bytes: &[u8], count: u8) -> Option<Vec<u16>> {
    let expected = 2 * count as usize;
    let raw = bytes.get(..expected)?;
    Some(
        raw.as_chunks::<2>()
            .0
            .iter()
            .map(|&c| u16::from_le_bytes(c))
            .collect(),
    )
}

/// Appends connection handles in little-endian order.
fn write_handles(buf: &mut Vec<u8>, handles: &[u16]) {
    for handle in handles {
        buf.extend_from_slice(&handle.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_opcodes() {
        assert_eq!(LeCreateBig::OP_CODE.get(), 0x2068);
        assert_eq!(LeTerminateBig::OP_CODE.get(), 0x206A);
        assert_eq!(LeBigCreateSyncHeader::OP_CODE.get(), 0x206B);
        assert_eq!(LeBigTerminateSync::OP_CODE.get(), 0x206C);
    }

    /// A wrong-length parameter block is not a status code from netsim's
    /// rootcanal — it kills the controller process outright — so the sizes
    /// are pinned here rather than discovered at runtime.
    #[test]
    fn test_fixed_parameter_lengths() {
        assert_eq!(size_of::<LeCreateBig>(), 31);
        assert_eq!(size_of::<LeTerminateBig>(), 2);
        assert_eq!(size_of::<LeBigCreateSyncHeader>(), 24);
        assert_eq!(size_of::<LeBigTerminateSync>(), 1);
        assert_eq!(size_of::<LeCreateBigCompleteEventHeader>(), 18);
        assert_eq!(size_of::<LeBigSyncEstablishedEventHeader>(), 14);
        assert_eq!(size_of::<LeBigInfoAdvertisingReportEvent>(), 19);
    }

    /// Byte-for-byte against Bumble's own encoder, which is the second opinion
    /// on every field's order and width:
    ///
    /// ```text
    /// HCI_LE_Create_BIG_Command(big_handle=0, advertising_handle=0, num_bis=1,
    ///     sdu_interval=10000, max_sdu=40, max_transport_latency=20, rtn=2,
    ///     phy=2, packing=0, framing=0, encryption=0, broadcast_code=bytes(16))
    /// -> 0168201f 00 00 01 102700 2800 1400 02 02 00 00 00 00*16
    /// ```
    #[test]
    fn test_create_big_matches_bumble() {
        let command = LeCreateBig {
            big_handle: 0,
            advertising_handle: 0,
            num_bis: 1,
            sdu_interval: U24::new(10_000),
            max_sdu: U16::new(40),
            max_transport_latency: U16::new(20),
            rtn: 2,
            phy: 2,
            packing: big_packing::SEQUENTIAL,
            framing: big_framing::UNFRAMED,
            encryption: big_encryption::UNENCRYPTED,
            broadcast_code: [0; 16],
        };
        let expected = hex(&format!(
            "{}{}",
            "000001102700280014000202000000",
            "00".repeat(16)
        ));
        assert_eq!(expected.len(), 31);
        assert_eq!(command.as_bytes(), &expected[..]);
    }

    /// ```text
    /// HCI_LE_BIG_Create_Sync_Command(big_handle=0, sync_handle=1, encryption=0,
    ///     broadcast_code=bytes(16), mse=0, big_sync_timeout=200, bis=[1, 2])
    /// -> 016b201a 00 0100 00 00*16 00 c800 02 01 02
    /// ```
    #[test]
    fn test_big_create_sync_matches_bumble() {
        let bytes = LeBigCreateSyncHeader::serialize(0, 0x0001, 0, [0; 16], 0, 200, &[1, 2]);
        let expected = hex(&format!(
            "{}{}{}",
            "00010000",
            "00".repeat(16),
            "00c800020102"
        ));
        assert_eq!(bytes, expected);
        assert_eq!(bytes.len(), 26, "the length rootcanal expects");
    }

    #[test]
    fn test_big_create_sync_round_trips() {
        let bytes =
            LeBigCreateSyncHeader::serialize(3, 0x0007, 1, [0xAB; 16], 2, 0x4000, &[1, 2, 3]);
        let (header, bis) = LeBigCreateSyncHeader::parse(&bytes).expect("round trips");
        assert_eq!(header.big_handle, 3);
        assert_eq!(header.sync_handle.get(), 7);
        assert_eq!(header.encryption, 1);
        assert_eq!(header.broadcast_code, [0xAB; 16]);
        assert_eq!(header.mse, 2);
        assert_eq!(header.big_sync_timeout.get(), 0x4000);
        assert_eq!(bis, &[1, 2, 3]);
    }

    #[test]
    fn test_a_truncated_bis_array_is_rejected() {
        let mut bytes = LeBigCreateSyncHeader::serialize(0, 1, 0, [0; 16], 0, 200, &[1, 2]);
        bytes.pop();
        assert!(LeBigCreateSyncHeader::parse(&bytes).is_none());
    }

    #[test]
    fn test_create_big_complete_round_trips() {
        let bytes = LeCreateBigCompleteEventHeader::serialize(
            0x00,
            0,
            0x0186A0,
            0x0124F8,
            0x02,
            3,
            1,
            0,
            2,
            40,
            8,
            &[0x0E00, 0x0E01],
        );
        assert_eq!(bytes.len(), 18 + 4);
        let (header, handles) = LeCreateBigCompleteEventHeader::parse(&bytes).expect("round trips");
        assert_eq!(header.status, 0);
        assert_eq!(header.big_sync_delay.get(), 0x0186A0);
        assert_eq!(header.transport_latency_big.get(), 0x0124F8);
        assert_eq!(header.max_pdu.get(), 40);
        assert_eq!(header.iso_interval.get(), 8);
        assert_eq!(handles, vec![0x0E00, 0x0E01]);
    }

    #[test]
    fn test_big_sync_established_round_trips() {
        let bytes = LeBigSyncEstablishedEventHeader::serialize(
            0x00,
            1,
            0x0124F8,
            3,
            1,
            0,
            2,
            100,
            8,
            &[0x0E02],
        );
        assert_eq!(bytes.len(), 14 + 2);
        let (header, handles) =
            LeBigSyncEstablishedEventHeader::parse(&bytes).expect("round trips");
        assert_eq!(header.big_handle, 1);
        assert_eq!(header.transport_latency_big.get(), 0x0124F8);
        assert_eq!(header.num_bis, 1);
        assert_eq!(handles, vec![0x0E02]);
    }

    /// The two Complete events differ by more than one field, and reading one
    /// with the other's layout silently yields plausible-looking garbage.
    #[test]
    fn test_the_two_complete_events_have_different_layouts() {
        assert_ne!(
            size_of::<LeCreateBigCompleteEventHeader>(),
            size_of::<LeBigSyncEstablishedEventHeader>(),
            "LE BIG Sync Established has no BIG_Sync_Delay and no PHY"
        );
    }

    #[test]
    fn test_a_truncated_handle_array_is_rejected() {
        let mut bytes =
            LeCreateBigCompleteEventHeader::serialize(0, 0, 0, 0, 2, 1, 1, 0, 1, 40, 8, &[1, 2]);
        bytes.pop();
        assert!(LeCreateBigCompleteEventHeader::parse(&bytes).is_none());
    }

    #[test]
    fn test_biginfo_report_parses() {
        // sync_handle=1, num_bis=2, nse=3, iso_interval=8, bn=1, pto=0, irc=2,
        // max_pdu=100, sdu_interval=10000, max_sdu=100, phy=2, framing=0, enc=0
        let bytes = hex("01000203080001000264001027006400020000");
        let report = LeBigInfoAdvertisingReportEvent::ref_from_bytes(&bytes).expect("parses");
        assert_eq!(report.sync_handle.get(), 1);
        assert_eq!(report.num_bis, 2);
        assert_eq!(report.nse, 3);
        assert_eq!(report.iso_interval.get(), 8);
        assert_eq!(report.max_pdu.get(), 100);
        assert_eq!(report.sdu_interval.get(), 10_000);
        assert_eq!(report.max_sdu.get(), 100);
        assert_eq!(report.phy, 2);
        assert_eq!(report.encryption, big_encryption::UNENCRYPTED);
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }
}
