use super::*;
use crate::device::big_broadcaster::BroadcastConfig;
use crate::packets::big::LeBigInfoAdvertisingReportEvent as BigInfo;
use crate::packets::build_iso_packet;
use crate::packets::ext_adv::ExtendedAdvertisingReportHeader;

fn le_meta(subevent: u8, params: &[u8]) -> Vec<u8> {
    let mut packet = vec![0x04, 0x3E, (1 + params.len()) as u8, subevent];
    packet.extend_from_slice(params);
    packet
}

/// An extended advertising report carrying `data`, from a source with SID
/// 3 at a fixed address.
fn advertising_report(data: &[u8]) -> Vec<u8> {
    let header = ExtendedAdvertisingReportHeader {
        event_type: U16::new(0),
        address_type: 0x00,
        address: [0x11, 0x22, 0x33, 0x44, 0x55, 0x66],
        primary_phy: adv_phy::LE_1M,
        secondary_phy: adv_phy::LE_2M,
        advertising_sid: 3,
        tx_power: 0x7F,
        rssi: -40,
        periodic_advertising_interval: U16::new(80),
        direct_address_type: 0x00,
        direct_address: [0; 6],
        data_length: data.len() as u8,
    };
    le_meta(
        ext_adv_subevent_code::LE_EXTENDED_ADVERTISING_REPORT,
        &LeExtendedAdvertisingReportEvent::serialize(&[(header, data)]),
    )
}

fn sync_established(sync_handle: u16) -> Vec<u8> {
    let event = LePeriodicAdvertisingSyncEstablishedEvent {
        status: 0x00,
        sync_handle: U16::new(sync_handle),
        advertising_sid: 3,
        advertiser_address_type: 0x00,
        advertiser_address: [0x11, 0x22, 0x33, 0x44, 0x55, 0x66],
        advertiser_phy: adv_phy::LE_2M,
        periodic_advertising_interval: U16::new(80),
        advertiser_clock_accuracy: 0x00,
    };
    le_meta(
        ext_adv_subevent_code::LE_PERIODIC_ADVERTISING_SYNC_ESTABLISHED,
        event.as_bytes(),
    )
}

fn periodic_report(sync_handle: u16, data_status: u8, data: &[u8]) -> Vec<u8> {
    le_meta(
        ext_adv_subevent_code::LE_PERIODIC_ADVERTISING_REPORT,
        &LePeriodicAdvertisingReportEventHeader::serialize(
            sync_handle,
            0x7F,
            -40,
            0xFF,
            data_status,
            data,
        ),
    )
}

fn big_info(sync_handle: u16, num_bis: u8, encryption: u8) -> Vec<u8> {
    let report = BigInfo {
        sync_handle: U16::new(sync_handle),
        num_bis,
        nse: 3,
        iso_interval: U16::new(8),
        bn: 1,
        pto: 0,
        irc: 2,
        max_pdu: U16::new(100),
        sdu_interval: crate::packets::ext_adv::U24::new(10_000),
        max_sdu: U16::new(100),
        phy: adv_phy::LE_2M,
        framing: 0,
        encryption,
    };
    le_meta(
        big_subevent_code::LE_BIGINFO_ADVERTISING_REPORT,
        report.as_bytes(),
    )
}

fn big_sync_established(status: u8, handles: &[u16]) -> Vec<u8> {
    le_meta(
        big_subevent_code::LE_BIG_SYNC_ESTABLISHED,
        &LeBigSyncEstablishedEventHeader::serialize(
            status, 0, 0x0124F8, 3, 1, 0, 2, 100, 8, handles,
        ),
    )
}

/// Drives a receiver from `start()` all the way to Receiving, against the
/// advertising and periodic payloads the *broadcaster* in this crate
/// produces. This is a self-test — see `tests/interop/` for the checks
/// that involve a foreign stack.
fn run_to_receiving(config: ReceiverConfig) -> BigReceiver {
    let source = BroadcastConfig {
        broadcast_id: 0x00AB_CDEF,
        ..Default::default()
    };
    let mut r = BigReceiver::new(config);
    let next = r.start();
    assert_eq!(
        u16::from_le_bytes([next[0][1], next[0][2]]),
        ext_adv_opcode::LE_SET_EXTENDED_SCAN_PARAMETERS.get()
    );
    let next = r.on_packet(&crate::test_support::command_complete(
        ext_adv_opcode::LE_SET_EXTENDED_SCAN_PARAMETERS.get(),
        &[0x00],
    ));
    assert_eq!(
        u16::from_le_bytes([next[0][1], next[0][2]]),
        ext_adv_opcode::LE_SET_EXTENDED_SCAN_ENABLE.get()
    );
    assert!(
        r.on_packet(&crate::test_support::command_complete(
            ext_adv_opcode::LE_SET_EXTENDED_SCAN_ENABLE.get(),
            &[0x00]
        ))
        .is_empty()
    );
    assert_eq!(r.state(), ReceiverState::Scanning);

    let next = r.on_packet(&advertising_report(&source.advertising_data()));
    assert_eq!(
        u16::from_le_bytes([next[0][1], next[0][2]]),
        ext_adv_opcode::LE_PERIODIC_ADVERTISING_CREATE_SYNC.get()
    );
    assert_eq!(r.found().unwrap().broadcast_id, 0x00AB_CDEF);
    assert_eq!(r.found().unwrap().advertising_sid, 3);

    assert!(r.on_packet(&sync_established(0x0009)).is_empty());
    assert_eq!(r.state(), ReceiverState::WaitingForAnnouncement);

    // The BASE alone is not enough: without BIGInfo the receiver does not
    // know whether the streams are encrypted.
    assert!(
        r.on_packet(&periodic_report(
            0x0009,
            0x00,
            &source.periodic_advertising_data()
        ))
        .is_empty()
    );
    assert_eq!(r.state(), ReceiverState::WaitingForAnnouncement);
    assert_eq!(r.base().unwrap().subgroups[0].bis.len(), 2);

    let next = r.on_packet(&big_info(0x0009, 2, big_encryption::UNENCRYPTED));
    assert_eq!(
        u16::from_le_bytes([next[0][1], next[0][2]]),
        big_opcode::LE_BIG_CREATE_SYNC.get()
    );
    // Header plus one octet per BIS index, taken from the BASE.
    assert_eq!(next[0][3], 26, "rootcanal dies on a wrong-length block");
    assert_eq!(&next[0][next[0].len() - 2..], &[1, 2], "BIS indices");

    let mut next = r.on_packet(&big_sync_established(0x00, &[0x0E10, 0x0E11]));
    for _ in 0..2 {
        assert_eq!(
            &next[0][1..3],
            &crate::device::host::opcode::LE_SETUP_ISO_DATA_PATH
        );
        assert_eq!(next[0][6], iso_data_path::OUTPUT, "a sink opens Output");
        next = r.on_packet(&crate::test_support::command_complete(
            u16::from_le_bytes(crate::device::host::opcode::LE_SETUP_ISO_DATA_PATH),
            &[0x00],
        ));
    }
    assert!(next.is_empty());
    assert_eq!(r.state(), ReceiverState::Receiving);
    r
}

#[test]
fn test_the_full_synchronization_sequence() {
    let r = run_to_receiving(ReceiverConfig::default());
    assert_eq!(r.bis_handles(), &[0x0E10, 0x0E11]);
    assert_eq!(r.sync_handle(), Some(0x0009));
}

#[test]
fn test_sdus_on_the_synced_handles_are_collected() {
    let mut r = run_to_receiving(ReceiverConfig::default());
    r.on_packet(&build_iso_packet(0x0E10, 0, &[0xAA; 100]));
    r.on_packet(&build_iso_packet(0x0E11, 0, &[0xBB; 100]));
    // A handle that is not one of ours belongs to some other stream.
    r.on_packet(&build_iso_packet(0x0E99, 0, &[0xCC; 100]));
    assert_eq!(r.sdu_count(), 2);
    assert_eq!(r.poll_sdu().unwrap().payload, vec![0xAA; 100]);
    assert_eq!(r.poll_sdu().unwrap().handle, 0x0E11);
    assert!(r.poll_sdu().is_none());
}

/// The octets the BASE arrived as, kept beside the parsed form: a consumer
/// showing a receiver's view of its source compares those bytes with what
/// the broadcaster published, which re-serializing the parse cannot do.
#[test]
fn test_the_base_octets_are_kept_as_they_arrived() {
    let r = run_to_receiving(ReceiverConfig::default());
    let source = BroadcastConfig {
        broadcast_id: 0x00AB_CDEF,
        ..Default::default()
    };
    // The Service Data payload, i.e. the periodic advertising data with its
    // AD length, AD type and UUID stripped.
    assert_eq!(
        r.base_bytes(),
        Some(&source.periodic_advertising_data()[4..])
    );
}

/// Leaving a BIG is answered by Command Complete and nothing else — no
/// BIG Sync Lost follows a local termination, so if this were not handled
/// the receiver would report `Receiving` forever after it had stopped.
#[test]
fn test_leaving_the_big_is_reflected_in_the_state() {
    let mut r = run_to_receiving(ReceiverConfig::default());
    let terminate = r.terminate();
    assert_eq!(
        u16::from_le_bytes([terminate[1], terminate[2]]),
        big_opcode::LE_BIG_TERMINATE_SYNC.get()
    );
    assert!(
        r.on_packet(&crate::test_support::command_complete(
            big_opcode::LE_BIG_TERMINATE_SYNC.get(),
            &[0x00, 0x00]
        ))
        .is_empty()
    );
    assert_eq!(r.state(), ReceiverState::Terminated);
    assert!(!r.is_receiving());
    assert!(r.bis_handles().is_empty());
    // And nothing that arrives afterwards is counted as audio.
    r.on_packet(&build_iso_packet(0x0E10, 1, &[0xAA; 100]));
    assert_eq!(r.sdu_count(), 0);
}

#[test]
fn test_a_broadcast_id_filter_skips_other_sources() {
    let source = BroadcastConfig {
        broadcast_id: 0x00AB_CDEF,
        ..Default::default()
    };
    let mut r = BigReceiver::new(ReceiverConfig {
        broadcast_id: Some(0x00_1234),
        ..Default::default()
    });
    r.start();
    r.on_packet(&crate::test_support::command_complete(
        ext_adv_opcode::LE_SET_EXTENDED_SCAN_PARAMETERS.get(),
        &[0x00],
    ));
    r.on_packet(&crate::test_support::command_complete(
        ext_adv_opcode::LE_SET_EXTENDED_SCAN_ENABLE.get(),
        &[0x00],
    ));
    assert!(
        r.on_packet(&advertising_report(&source.advertising_data()))
            .is_empty(),
        "a different Broadcast_ID must not be synced to"
    );
    assert_eq!(r.state(), ReceiverState::Scanning);
    assert!(r.found().is_none());
}

#[test]
fn test_fragmented_periodic_reports_are_reassembled() {
    let source = BroadcastConfig::default();
    let data = source.periodic_advertising_data();
    let (first, second) = data.split_at(20);
    let mut r = BigReceiver::new(ReceiverConfig::default());
    r.start();
    r.on_packet(&crate::test_support::command_complete(
        ext_adv_opcode::LE_SET_EXTENDED_SCAN_PARAMETERS.get(),
        &[0x00],
    ));
    r.on_packet(&crate::test_support::command_complete(
        ext_adv_opcode::LE_SET_EXTENDED_SCAN_ENABLE.get(),
        &[0x00],
    ));
    r.on_packet(&advertising_report(&source.advertising_data()));
    r.on_packet(&sync_established(0x0009));

    // Half a BASE must not parse into a half-truth.
    assert!(
        r.on_packet(&periodic_report(0x0009, 0x01, first))
            .is_empty()
    );
    assert!(r.base().is_none());
    r.on_packet(&periodic_report(0x0009, 0x00, second));
    assert_eq!(r.base().unwrap().presentation_delay, 40_000);
}

#[test]
fn test_a_truncated_periodic_report_is_discarded() {
    let source = BroadcastConfig::default();
    let mut r = BigReceiver::new(ReceiverConfig::default());
    r.start();
    r.on_packet(&crate::test_support::command_complete(
        ext_adv_opcode::LE_SET_EXTENDED_SCAN_PARAMETERS.get(),
        &[0x00],
    ));
    r.on_packet(&crate::test_support::command_complete(
        ext_adv_opcode::LE_SET_EXTENDED_SCAN_ENABLE.get(),
        &[0x00],
    ));
    r.on_packet(&advertising_report(&source.advertising_data()));
    r.on_packet(&sync_established(0x0009));
    r.on_packet(&periodic_report(
        0x0009,
        0x02,
        &source.periodic_advertising_data()[..10],
    ));
    assert!(r.base().is_none());
    assert_eq!(r.state(), ReceiverState::WaitingForAnnouncement);
}

#[test]
fn test_an_encrypted_source_without_a_code_is_refused() {
    let source = BroadcastConfig::default();
    let mut r = BigReceiver::new(ReceiverConfig::default());
    r.start();
    r.on_packet(&crate::test_support::command_complete(
        ext_adv_opcode::LE_SET_EXTENDED_SCAN_PARAMETERS.get(),
        &[0x00],
    ));
    r.on_packet(&crate::test_support::command_complete(
        ext_adv_opcode::LE_SET_EXTENDED_SCAN_ENABLE.get(),
        &[0x00],
    ));
    r.on_packet(&advertising_report(&source.advertising_data()));
    r.on_packet(&sync_established(0x0009));
    r.on_packet(&periodic_report(
        0x0009,
        0x00,
        &source.periodic_advertising_data(),
    ));
    assert!(
        r.on_packet(&big_info(0x0009, 2, big_encryption::ENCRYPTED))
            .is_empty()
    );
    assert_eq!(r.state(), ReceiverState::Failed(0x1D));
}

#[test]
fn test_a_failed_big_sync_is_recorded() {
    let mut r = BigReceiver::new(ReceiverConfig::default());
    r.state = ReceiverState::SyncingToBig;
    // 0x3E = Connection Failed to be Established.
    assert!(r.on_packet(&big_sync_established(0x3E, &[])).is_empty());
    assert_eq!(r.state(), ReceiverState::Failed(0x3E));
}

#[test]
fn test_losing_the_big_stops_the_stream() {
    let mut r = run_to_receiving(ReceiverConfig::default());
    let lost = le_meta(
        big_subevent_code::LE_BIG_SYNC_LOST,
        LeBigSyncLostEvent {
            big_handle: 0,
            reason: 0x3E,
        }
        .as_bytes(),
    );
    r.on_packet(&lost);
    assert_eq!(r.state(), ReceiverState::Lost(0x3E));
    r.on_packet(&build_iso_packet(0x0E10, 1, &[0xAA; 100]));
    assert_eq!(r.sdu_count(), 0, "a lost BIG carries nothing");
}
