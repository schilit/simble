// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for LE Audio PACS (Published Audio Capabilities) and VCP (Volume Control),
//! plus end-to-end ASCS Control Point dispatch through a real device's ATT write path.

use simble::VirtualDevice;
use simble::att::opcode as att_opcode;
use simble::gatt::GattDatabase;
use simble::l2cap::{L2capHeader, cid};
use simble::profiles::ascs::{
    AseState, AudioStreamControlService, opcode as ascs_opcode, reason_code, response_code,
};
use simble::profiles::bap::LC3_CODEC_ID;
use simble::profiles::pacs::audio_location;
use simble::profiles::{PublishedAudioCapabilitiesService, VolumeControlService};
use simble::types::{Address, AddressType};

#[test]
fn test_pacs_service_audio_capabilities_registration() {
    let mut db = GattDatabase::new();
    let pacs = PublishedAudioCapabilitiesService::register(
        &mut db,
        audio_location::STEREO,
        audio_location::FRONT_CENTER,
    );

    // 1. Read Sink PAC record
    let sink_pac = db
        .read(pacs.sink_pac_value_handle, 0)
        .expect("read sink pac");
    assert_eq!(sink_pac[0], 0x01); // 1 PAC record
    assert_eq!(sink_pac[1], 0x06); // LC3 Codec

    // 2. Read Sink Audio Locations (STEREO = 0x03)
    let sink_loc = db.read(pacs.sink_locations_value_handle, 0).unwrap();
    let loc_val = u32::from_le_bytes(sink_loc.try_into().unwrap());
    assert_eq!(loc_val, audio_location::STEREO);

    // 3. Read Supported Audio Contexts
    let contexts = db.read(pacs.supported_contexts_value_handle, 0).unwrap();
    assert_eq!(contexts, &[0x07, 0x00, 0x07, 0x00]);
}

#[test]
fn test_vcp_volume_control_service() {
    let mut db = GattDatabase::new();
    let vcp = VolumeControlService::register(&mut db, 120, 0); // Volume 120, Unmuted

    // 1. Read Volume State
    let state = db
        .read(vcp.volume_state_value_handle, 0)
        .expect("read state");
    assert_eq!(state[0], 120); // Volume
    assert_eq!(state[1], 0); // Unmuted

    // 2. Update Volume State
    db.set_value(vcp.volume_state_value_handle, &[150, 1, 0x01])
        .unwrap(); // Muted, Vol 150
    let updated = db.read(vcp.volume_state_value_handle, 0).unwrap();
    assert_eq!(updated[0], 150);
    assert_eq!(updated[1], 1);
}

// A raw ATT Write Request to the ASE Control Point must reach the ASE state machine via
// the AttributeHandler ASCS registers - before that handler existed, a write arriving
// through `VirtualDevice::process_l2cap_packet` silently overwrote the control point's
// stored bytes and no ASE ever left Idle.
#[test]
fn test_ascs_control_point_dispatch_through_att_write() {
    let addr = Address::from_be_bytes([0xC0, 0xFF, 0xEE, 0x01, 0x02, 0x03]);
    let mut dev = VirtualDevice::new("LeAudioSink", addr, AddressType::Random);
    let ascs = AudioStreamControlService::register(&mut dev.gatt_db, &[1], &[]);
    assert_eq!(ascs.ase(1).unwrap().state, AseState::Idle);

    let conn_h = 0x0040;
    dev.on_connected(conn_h, Address::ANY);

    let mut write_req = vec![att_opcode::WRITE_REQ];
    write_req.extend_from_slice(&ascs.control_point_value_handle.to_le_bytes());
    write_req.push(ascs_opcode::CONFIG_CODEC);
    write_req.push(1); // one ASE operation
    write_req.push(1); // ase_id
    write_req.push(3); // target_latency
    write_req.push(1); // target_phy
    write_req.extend_from_slice(&LC3_CODEC_ID);
    write_req.push(0); // codec_specific_configuration length

    let l2cap = L2capHeader::serialize(cid::ATT, &write_req);
    let resp = dev.process_l2cap_packet(conn_h, &l2cap).unwrap().unwrap();
    let (_, payload) = L2capHeader::parse(&resp).unwrap();
    assert_eq!(payload[0], att_opcode::WRITE_RSP);

    let ase = ascs.ase(1).unwrap();
    assert_eq!(ase.state, AseState::CodecConfigured);
    assert_eq!(ase.codec_id, LC3_CODEC_ID);
    assert_eq!(
        dev.gatt_db.read(ase.value_handle, 0).unwrap()[..2],
        [1, AseState::CodecConfigured as u8]
    );
    assert_eq!(
        ascs.control_point_notification(),
        vec![
            ascs_opcode::CONFIG_CODEC,
            1,
            1,
            response_code::SUCCESS,
            reason_code::NONE
        ]
    );
}

/// Every characteristic that declares Notify or Indicate must be followed by
/// a Client Characteristic Configuration descriptor, or a client has nowhere
/// to write its subscription and can never receive a value (Core Spec Vol 3,
/// Part G, Section 3.3.3.3).
///
/// This is a class-level guard rather than a per-profile assertion: the
/// defect appeared independently in PACS, ASCS, RAS and five example scripts,
/// each time as "one more characteristic someone forgot".
#[test]
fn test_every_notifying_characteristic_has_a_cccd() {
    use simble::gatt::{GattDatabase, desc_uuid};
    use simble::profiles::{
        AudioStreamControlService, PublishedAudioCapabilitiesService, RangingService,
    };
    use simble::types::Uuid;

    let mut db = GattDatabase::new();
    PublishedAudioCapabilitiesService::register(&mut db, 0x03, 0x00);
    AudioStreamControlService::register(&mut db, &[1], &[2]);
    RangingService::register(&mut db);

    const NOTIFY_OR_INDICATE: u8 = 0x10 | 0x20;
    let cccd = Uuid::Uuid16(desc_uuid::CLIENT_CHARACTERISTIC_CONFIGURATION);
    // A characteristic declaration's value is [properties, handle_lo,
    // handle_hi, uuid...]; the value attribute follows it, and the CCCD (if
    // any) follows that.
    let declaration = Uuid::Uuid16(0x2803);
    let handles: Vec<u16> = db.attributes.keys().copied().collect();

    let mut checked = 0;
    for (i, handle) in handles.iter().enumerate() {
        let attr = &db.attributes[handle];
        if attr.uuid != declaration || attr.value.is_empty() {
            continue;
        }
        if attr.value[0] & NOTIFY_OR_INDICATE == 0 {
            continue;
        }
        let characteristic_uuid = &attr.value[3..];
        let follows_cccd = handles
            .get(i + 2)
            .map(|h| db.attributes[h].uuid == cccd)
            .unwrap_or(false);
        assert!(
            follows_cccd,
            "characteristic {characteristic_uuid:02X?} declares notify/indicate \
             but has no CCCD after its value attribute"
        );
        checked += 1;
    }
    assert!(
        checked >= 9,
        "expected to check every notifying characteristic in PACS + ASCS + RAS, saw {checked}"
    );
}
