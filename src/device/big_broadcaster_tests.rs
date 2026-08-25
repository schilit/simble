use super::*;

/// Type adapter: this module holds opcodes as `OpCode`, the shared builder
/// takes the `u16` the packet carries.
fn command_complete(opcode: OpCode, params: &[u8]) -> Vec<u8> {
    crate::test_support::command_complete(opcode.get(), params)
}

fn create_big_complete(status: u8, handles: &[u16]) -> Vec<u8> {
    let mut params = vec![big_subevent_code::LE_CREATE_BIG_COMPLETE];
    params.extend_from_slice(&LeCreateBigCompleteEventHeader::serialize(
        status, 0, 0x0186A0, 0x0124F8, 0x02, 3, 1, 0, 2, 100, 8, handles,
    ));
    let mut packet = vec![0x04, 0x3E, params.len() as u8];
    packet.extend_from_slice(&params);
    packet
}

/// Walks the whole sequence, checking the opcode of each command as it
/// comes out — the order matters: a controller refuses periodic
/// advertising data before its parameters, and LE Create BIG before the
/// periodic train is enabled.
fn run_to_streaming(handles: &[u16]) -> BigBroadcaster {
    let mut b = BigBroadcaster::new(BroadcastConfig::default());
    let mut next = b.start();
    for expected in [
        ext_adv_opcode::LE_SET_EXTENDED_ADVERTISING_PARAMETERS,
        ext_adv_opcode::LE_SET_EXTENDED_ADVERTISING_DATA,
        ext_adv_opcode::LE_SET_PERIODIC_ADVERTISING_PARAMETERS,
        ext_adv_opcode::LE_SET_PERIODIC_ADVERTISING_DATA,
        ext_adv_opcode::LE_SET_EXTENDED_ADVERTISING_ENABLE,
        ext_adv_opcode::LE_SET_PERIODIC_ADVERTISING_ENABLE,
    ] {
        assert_eq!(next.len(), 1, "one command at a time");
        assert_eq!(
            u16::from_le_bytes([next[0][1], next[0][2]]),
            expected.get(),
            "unexpected command in the setup sequence"
        );
        // Extended Advertising Parameters returns status + TX power.
        next = b.on_packet(&crate::test_support::command_complete(
            expected.get(),
            &[0x00, 0x00],
        ));
    }
    assert_eq!(
        u16::from_le_bytes([next[0][1], next[0][2]]),
        big_opcode::LE_CREATE_BIG.get()
    );
    assert_eq!(b.state(), BroadcastState::CreatingBig);

    let mut next = b.on_packet(&create_big_complete(0x00, handles));
    for _ in 0..handles.len() {
        assert_eq!(
            &next[0][1..3],
            &super::super::host::opcode::LE_SETUP_ISO_DATA_PATH
        );
        assert_eq!(next[0][6], iso_data_path::INPUT, "a source opens Input");
        next = b.on_packet(&command_complete(
            OpCode::from_bytes(super::super::host::opcode::LE_SETUP_ISO_DATA_PATH),
            &[0x00],
        ));
    }
    assert!(next.is_empty());
    b
}

#[test]
fn test_the_full_setup_sequence() {
    let b = run_to_streaming(&[0x0E00, 0x0E01]);
    assert_eq!(b.state(), BroadcastState::Streaming);
    assert_eq!(b.bis_handles(), &[0x0E00, 0x0E01]);
}

#[test]
fn test_the_create_big_parameter_block_is_31_octets() {
    let mut b = BigBroadcaster::new(BroadcastConfig::default());
    b.state = BroadcastState::CreatingBig;
    let packet = b.create_big();
    // H4 type, opcode, length, then parameters.
    assert_eq!(packet[3] as usize, packet.len() - 4);
    assert_eq!(packet[3], 31, "rootcanal dies on a wrong-length block");
}

#[test]
fn test_audio_is_refused_until_the_data_paths_are_open() {
    let mut b = BigBroadcaster::new(BroadcastConfig::default());
    assert!(b.send_sdu(1, &[0xAA; 100]).is_none(), "idle");
    let mut b = run_to_streaming(&[0x0E00, 0x0E01]);
    assert!(b.send_sdu(1, &[0xAA; 100]).is_some());
}

#[test]
fn test_each_bis_has_its_own_sequence_numbers() {
    let mut b = run_to_streaming(&[0x0E00, 0x0E01]);
    // The sequence number lives in the ISO data load header, bytes 5-6,
    // and the handle in bytes 1-2.
    let left = b.send_sdu(1, &[0x01; 8]).unwrap();
    let right = b.send_sdu(2, &[0x02; 8]).unwrap();
    let left2 = b.send_sdu(1, &[0x03; 8]).unwrap();
    assert_eq!(u16::from_le_bytes([left[1], left[2]]) & 0x0FFF, 0x0E00);
    assert_eq!(u16::from_le_bytes([right[1], right[2]]) & 0x0FFF, 0x0E01);
    assert_eq!(u16::from_le_bytes([left[5], left[6]]), 0);
    assert_eq!(u16::from_le_bytes([right[5], right[6]]), 0);
    assert_eq!(u16::from_le_bytes([left2[5], left2[6]]), 1);
    assert!(b.send_sdu(3, &[0x04; 8]).is_none(), "no third BIS");
}

/// Metadata is the only part of a running broadcast that may change, and
/// it changes by rewriting the periodic train — not by touching the BIG.
#[test]
fn test_metadata_is_republished_without_disturbing_the_big() {
    let mut b = BigBroadcaster::new(BroadcastConfig::default());
    assert!(
        b.update_metadata(vec![0x04, 0x04, b'e', b'n', b'g'])
            .is_none(),
        "nothing to rewrite before the train is up"
    );

    let mut b = run_to_streaming(&[0x0E00, 0x0E01]);
    let handles = b.bis_handles().to_vec();
    let command = b
        .update_metadata(vec![0x04, 0x04, b'e', b'n', b'g'])
        .expect("streaming, so the train exists");
    assert_eq!(
        u16::from_le_bytes([command[1], command[2]]),
        ext_adv_opcode::LE_SET_PERIODIC_ADVERTISING_DATA.get()
    );
    // The new BASE is what a receiver will read on the next train.
    assert!(
        b.config()
            .periodic_advertising_data()
            .windows(3)
            .any(|w| w == b"eng"),
        "the language metadata reached the BASE"
    );
    assert!(b.take_update_status().is_none(), "not answered yet");

    let replies = b.on_packet(&command_complete(
        ext_adv_opcode::LE_SET_PERIODIC_ADVERTISING_DATA,
        &[0x00],
    ));
    assert!(replies.is_empty(), "an update restarts nothing");
    assert_eq!(b.take_update_status(), Some(0x00));
    assert_eq!(b.take_update_status(), None, "taken once");
    assert_eq!(b.state(), BroadcastState::Streaming, "the BIG is untouched");
    assert_eq!(b.bis_handles(), handles.as_slice());
    assert!(b.send_sdu(1, &[0xAA; 100]).is_some(), "audio keeps flowing");
}

#[test]
fn test_a_refused_command_stops_the_sequence() {
    let mut b = BigBroadcaster::new(BroadcastConfig::default());
    b.start();
    // 0x12 = Invalid HCI Command Parameters.
    let next = b.on_packet(&command_complete(
        ext_adv_opcode::LE_SET_EXTENDED_ADVERTISING_PARAMETERS,
        &[0x12, 0x00],
    ));
    assert!(next.is_empty());
    assert_eq!(b.state(), BroadcastState::Failed(0x12));
}

#[test]
fn test_a_refused_big_is_recorded_rather_than_retried() {
    let mut b = run_to_create_big();
    // 0x0C = Command Disallowed, what a controller answers if the
    // periodic train is not running.
    let mut status = vec![0x04, 0x0F, 0x04, 0x0C, 0x01];
    status.extend_from_slice(&big_opcode::LE_CREATE_BIG.get().to_le_bytes());
    assert!(b.on_packet(&status).is_empty());
    assert_eq!(b.state(), BroadcastState::Failed(0x0C));
}

#[test]
fn test_a_failed_create_big_complete_is_recorded() {
    let mut b = run_to_create_big();
    assert!(b.on_packet(&create_big_complete(0x42, &[])).is_empty());
    assert_eq!(b.state(), BroadcastState::Failed(0x42));
}

fn run_to_create_big() -> BigBroadcaster {
    let mut b = BigBroadcaster::new(BroadcastConfig::default());
    let mut next = b.start();
    for expected in [
        ext_adv_opcode::LE_SET_EXTENDED_ADVERTISING_PARAMETERS,
        ext_adv_opcode::LE_SET_EXTENDED_ADVERTISING_DATA,
        ext_adv_opcode::LE_SET_PERIODIC_ADVERTISING_PARAMETERS,
        ext_adv_opcode::LE_SET_PERIODIC_ADVERTISING_DATA,
        ext_adv_opcode::LE_SET_EXTENDED_ADVERTISING_ENABLE,
        ext_adv_opcode::LE_SET_PERIODIC_ADVERTISING_ENABLE,
    ] {
        next = b.on_packet(&crate::test_support::command_complete(
            expected.get(),
            &[0x00, 0x00],
        ));
    }
    let _ = next;
    b
}

/// The advertising payload a scanner sees, byte for byte against what
/// Bumble's `bap.BroadcastAudioAnnouncement(0xABCDEF).get_advertising_data()`
/// produces: `06 16 52 18 ef cd ab`.
#[test]
fn test_advertising_data_matches_bumble() {
    let config = BroadcastConfig {
        broadcast_id: 0x00AB_CDEF,
        broadcast_name: "Simble".to_string(),
        ..Default::default()
    };
    let data = config.advertising_data();
    assert_eq!(
        &data[..7],
        &[0x06, 0x16, 0x52, 0x18, 0xEF, 0xCD, 0xAB],
        "Broadcast Audio Announcement"
    );
    // Bumble: bytes(AdvertisingData([BroadcastName('Simble')])) = 073053696d626c65
    assert_eq!(&data[7..], b"\x07\x30Simble", "Broadcast Name");
}

/// The BASE a receiver reads, byte for byte against Bumble's own
/// `BasicAudioAnnouncement(...).get_advertising_data()` for the same
/// configuration — this is the structure that decides whether a foreign
/// receiver can decode anything at all.
#[test]
fn test_base_matches_bumble() {
    let config = BroadcastConfig {
        metadata: vec![0x04, 0x04, b'e', b'n', b'g'],
        ..Default::default()
    };
    let expected = [
        0x2e, 0x16, 0x51, 0x18, 0x40, 0x9c, 0x00, 0x01, 0x02, 0x06, 0x00, 0x00, 0x00, 0x00, 0x0a,
        0x02, 0x01, 0x08, 0x02, 0x02, 0x01, 0x03, 0x04, 0x64, 0x00, 0x05, 0x04, 0x04, 0x65, 0x6e,
        0x67, 0x01, 0x06, 0x05, 0x03, 0x01, 0x00, 0x00, 0x00, 0x02, 0x06, 0x05, 0x03, 0x02, 0x00,
        0x00, 0x00,
    ];
    assert_eq!(config.periodic_advertising_data(), expected);
}
