// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for LE Audio PACS (Published Audio Capabilities) and VCP (Volume Control).

use simble::gatt::GattDatabase;
use simble::profiles::pacs::audio_location;
use simble::profiles::{PublishedAudioCapabilitiesService, VolumeControlService};

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
