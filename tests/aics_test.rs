// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Audio Input Control Service (AICS) tests: Audio Input State get/set round-trips, mute
//! transitions and their Mute-Disabled restriction, Gain Mode transitions and the
//! Manual-Only/Automatic-Only restriction, stale Change_Counter rejection, and the writable
//! Audio Input Description characteristic - driven entirely through the public GATT
//! database API.

use simble::gatt::GattDatabase;
use simble::profiles::aics::{
    AudioInputControlService, AudioInputStatus, AudioInputType, GainMode, GainSettingsProperties,
    Mute, error_code, opcode,
};

fn register(db: &mut GattDatabase) -> AudioInputControlService {
    AudioInputControlService::register(
        db,
        GainSettingsProperties::default(),
        AudioInputType::Microphone,
        AudioInputStatus::Active,
        "Bluetooth",
    )
}

#[test]
fn test_init_service_state() {
    let mut db = GattDatabase::new();
    let aics = register(&mut db);

    assert_eq!(
        db.read(aics.audio_input_state_value_handle, 0).unwrap(),
        &[0, Mute::NotMuted as u8, GainMode::Manual as u8, 0]
    );
    assert_eq!(
        db.read(aics.gain_settings_properties_value_handle, 0)
            .unwrap(),
        &[1, 0, 255]
    );
    assert_eq!(
        db.read(aics.audio_input_status_value_handle, 0).unwrap(),
        &[AudioInputStatus::Active as u8]
    );
    assert_eq!(
        db.read(aics.audio_input_type_value_handle, 0).unwrap(),
        &[AudioInputType::Microphone as u8]
    );
}

#[test]
fn test_wrong_opcode_is_rejected() {
    let mut db = GattDatabase::new();
    let mut aics = register(&mut db);
    assert_eq!(
        aics.write_control_point(&mut db, &[0xFF]),
        Err(error_code::OPCODE_NOT_SUPPORTED)
    );
}

#[test]
fn test_set_gain_setting_when_manual_updates_state() {
    let mut db = GattDatabase::new();
    let mut aics = register(&mut db);

    aics.write_control_point(&mut db, &[opcode::SET_GAIN_SETTING, 0, 120])
        .unwrap();

    assert_eq!(aics.audio_input_state.gain_setting, 120);
    // Set Gain Setting never advances Change_Counter - only Mute/Gain Mode do.
    assert_eq!(aics.audio_input_state.change_counter, 0);
    assert_eq!(
        db.read(aics.audio_input_state_value_handle, 0).unwrap()[0],
        120
    );
}

#[test]
fn test_set_gain_setting_when_manual_only_updates_state() {
    let mut db = GattDatabase::new();
    let mut aics = register(&mut db);
    aics.audio_input_state.gain_mode = GainMode::ManualOnly;

    aics.write_control_point(&mut db, &[opcode::SET_GAIN_SETTING, 0, 120])
        .unwrap();
    assert_eq!(aics.audio_input_state.gain_setting, 120);
}

#[test]
fn test_set_gain_setting_when_automatic_is_ignored() {
    let mut db = GattDatabase::new();
    let mut aics = register(&mut db);
    aics.audio_input_state.gain_mode = GainMode::Automatic;

    assert_eq!(
        aics.write_control_point(&mut db, &[opcode::SET_GAIN_SETTING, 0, 120]),
        Ok(())
    );
    assert_eq!(aics.audio_input_state.gain_setting, 0);
}

#[test]
fn test_set_gain_setting_when_automatic_only_is_ignored() {
    let mut db = GattDatabase::new();
    let mut aics = register(&mut db);
    aics.audio_input_state.gain_mode = GainMode::AutomaticOnly;

    aics.write_control_point(&mut db, &[opcode::SET_GAIN_SETTING, 0, 120])
        .unwrap();
    assert_eq!(aics.audio_input_state.gain_setting, 0);
}

#[test]
fn test_unmute_when_muted() {
    let mut db = GattDatabase::new();
    let mut aics = register(&mut db);
    aics.audio_input_state.mute = Mute::Muted;

    aics.write_control_point(&mut db, &[opcode::UNMUTE, 0])
        .unwrap();
    assert_eq!(aics.audio_input_state.mute, Mute::NotMuted);
    assert_eq!(aics.audio_input_state.change_counter, 1);
}

#[test]
fn test_unmute_when_mute_disabled_is_rejected() {
    let mut db = GattDatabase::new();
    let mut aics = register(&mut db);
    aics.audio_input_state.mute = Mute::Disabled;

    assert_eq!(
        aics.write_control_point(&mut db, &[opcode::UNMUTE, 0]),
        Err(error_code::MUTE_DISABLED)
    );
    assert_eq!(aics.audio_input_state.mute, Mute::Disabled);
    assert_eq!(aics.audio_input_state.change_counter, 0);
}

#[test]
fn test_mute_when_not_muted() {
    let mut db = GattDatabase::new();
    let mut aics = register(&mut db);

    aics.write_control_point(&mut db, &[opcode::MUTE, 0])
        .unwrap();
    assert_eq!(aics.audio_input_state.mute, Mute::Muted);
    assert_eq!(aics.audio_input_state.change_counter, 1);
}

#[test]
fn test_mute_when_mute_disabled_is_rejected() {
    let mut db = GattDatabase::new();
    let mut aics = register(&mut db);
    aics.audio_input_state.mute = Mute::Disabled;
    aics.audio_input_state.change_counter = 0;

    assert_eq!(
        aics.write_control_point(&mut db, &[opcode::MUTE, 0]),
        Err(error_code::MUTE_DISABLED)
    );
    assert_eq!(aics.audio_input_state.mute, Mute::Disabled);
    assert_eq!(aics.audio_input_state.change_counter, 0);
}

#[test]
fn test_stale_change_counter_rejects_mute() {
    let mut db = GattDatabase::new();
    let mut aics = register(&mut db);

    assert_eq!(
        aics.write_control_point(&mut db, &[opcode::MUTE, 1]),
        Err(error_code::INVALID_CHANGE_COUNTER)
    );
    assert_eq!(aics.audio_input_state.mute, Mute::NotMuted);
}

#[test]
fn test_set_manual_gain_mode_when_automatic() {
    let mut db = GattDatabase::new();
    let mut aics = register(&mut db);
    aics.audio_input_state.gain_mode = GainMode::Automatic;

    aics.write_control_point(&mut db, &[opcode::SET_MANUAL_GAIN_MODE, 0])
        .unwrap();
    assert_eq!(aics.audio_input_state.gain_mode, GainMode::Manual);
    assert_eq!(aics.audio_input_state.change_counter, 1);
}

#[test]
fn test_set_manual_gain_mode_when_already_manual_is_noop() {
    let mut db = GattDatabase::new();
    let mut aics = register(&mut db);

    aics.write_control_point(&mut db, &[opcode::SET_MANUAL_GAIN_MODE, 0])
        .unwrap();
    assert_eq!(aics.audio_input_state.gain_mode, GainMode::Manual);
    assert_eq!(aics.audio_input_state.change_counter, 0);
}

#[test]
fn test_set_manual_gain_mode_when_manual_only_is_rejected() {
    let mut db = GattDatabase::new();
    let mut aics = register(&mut db);
    aics.audio_input_state.gain_mode = GainMode::ManualOnly;

    assert_eq!(
        aics.write_control_point(&mut db, &[opcode::SET_MANUAL_GAIN_MODE, 0]),
        Err(error_code::GAIN_MODE_CHANGE_NOT_ALLOWED)
    );
    assert_eq!(aics.audio_input_state.gain_mode, GainMode::ManualOnly);
}

#[test]
fn test_set_manual_gain_mode_when_automatic_only_is_rejected() {
    let mut db = GattDatabase::new();
    let mut aics = register(&mut db);
    aics.audio_input_state.gain_mode = GainMode::AutomaticOnly;

    assert_eq!(
        aics.write_control_point(&mut db, &[opcode::SET_MANUAL_GAIN_MODE, 0]),
        Err(error_code::GAIN_MODE_CHANGE_NOT_ALLOWED)
    );
}

#[test]
fn test_set_automatic_gain_mode_when_manual() {
    let mut db = GattDatabase::new();
    let mut aics = register(&mut db);

    aics.write_control_point(&mut db, &[opcode::SET_AUTOMATIC_GAIN_MODE, 0])
        .unwrap();
    assert_eq!(aics.audio_input_state.gain_mode, GainMode::Automatic);
    assert_eq!(aics.audio_input_state.change_counter, 1);
}

#[test]
fn test_set_automatic_gain_mode_when_already_automatic_is_noop() {
    let mut db = GattDatabase::new();
    let mut aics = register(&mut db);
    aics.audio_input_state.gain_mode = GainMode::Automatic;

    aics.write_control_point(&mut db, &[opcode::SET_AUTOMATIC_GAIN_MODE, 0])
        .unwrap();
    assert_eq!(aics.audio_input_state.gain_mode, GainMode::Automatic);
    assert_eq!(aics.audio_input_state.change_counter, 0);
}

#[test]
fn test_set_automatic_gain_mode_when_manual_only_is_rejected() {
    let mut db = GattDatabase::new();
    let mut aics = register(&mut db);
    aics.audio_input_state.gain_mode = GainMode::ManualOnly;

    assert_eq!(
        aics.write_control_point(&mut db, &[opcode::SET_AUTOMATIC_GAIN_MODE, 0]),
        Err(error_code::GAIN_MODE_CHANGE_NOT_ALLOWED)
    );
}

#[test]
fn test_set_automatic_gain_mode_when_automatic_only_is_rejected() {
    let mut db = GattDatabase::new();
    let mut aics = register(&mut db);
    aics.audio_input_state.gain_mode = GainMode::AutomaticOnly;

    assert_eq!(
        aics.write_control_point(&mut db, &[opcode::SET_AUTOMATIC_GAIN_MODE, 0]),
        Err(error_code::GAIN_MODE_CHANGE_NOT_ALLOWED)
    );
}

#[test]
fn test_audio_input_description_initial_value_and_write() {
    let mut db = GattDatabase::new();
    let aics = register(&mut db);

    assert_eq!(
        db.read(aics.audio_input_description_value_handle, 0)
            .unwrap(),
        b"Bluetooth"
    );

    db.write(aics.audio_input_description_value_handle, b"Line Input")
        .unwrap();
    assert_eq!(
        db.read(aics.audio_input_description_value_handle, 0)
            .unwrap(),
        b"Line Input"
    );
}
