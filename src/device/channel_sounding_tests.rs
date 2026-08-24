/// The host's `[u8; 2]` opcodes and the simulated controller's `u16` ones
/// are now derived from `packets::hci::cs_opcode`, so this asserts the
/// derivation rather than a hand-copied value: if the canonical constant
/// moves, all three move together, and if someone re-introduces a literal
/// on one side this fails.
#[test]
fn cs_opcodes_agree_across_host_controller_and_packets() {
    use crate::packets::hci::cs_opcode;
    for (host, canonical) in [
        (
            opcode::LE_CS_SECURITY_ENABLE,
            cs_opcode::LE_CS_SECURITY_ENABLE,
        ),
        (opcode::LE_CS_CREATE_CONFIG, cs_opcode::LE_CS_CREATE_CONFIG),
        (
            opcode::LE_CS_SET_PROCEDURE_PARAMETERS,
            cs_opcode::LE_CS_SET_PROCEDURE_PARAMETERS,
        ),
        (
            opcode::LE_CS_PROCEDURE_ENABLE,
            cs_opcode::LE_CS_PROCEDURE_ENABLE,
        ),
    ] {
        assert_eq!(host, canonical.to_bytes());
        assert_eq!(u16::from_le_bytes(host), canonical.as_u16());
    }
}
use super::*;

/// An LE Meta event packet carrying `parameters` (subevent code first).
fn le_meta(parameters: &[u8]) -> Vec<u8> {
    let mut packet = vec![0x04, 0x3E, parameters.len() as u8];
    packet.extend_from_slice(parameters);
    packet
}

/// LE CS Security Enable Complete.
fn security_complete(status: u8, handle: u16) -> Vec<u8> {
    let mut params = vec![subevent::SECURITY_ENABLE_COMPLETE, status];
    params.extend_from_slice(&handle.to_le_bytes());
    le_meta(&params)
}

/// LE CS Config Complete for `role`.
fn config_complete(status: u8, handle: u16, config_id: u8, role: u8) -> Vec<u8> {
    let mut params = vec![subevent::CONFIG_COMPLETE, status];
    params.extend_from_slice(&handle.to_le_bytes());
    params.push(config_id);
    params.extend_from_slice(&[0x01, 0x02, 0xFF, 0x02, 0x14, 0x00, 0x03]);
    params.push(role);
    params.extend_from_slice(&[0x00, 0x01]);
    params.extend_from_slice(&[0xFF; 10]);
    params.extend_from_slice(&[0x01, 0x00, 0x00, 0x00, 0x00]);
    le_meta(&params)
}

/// LE CS Procedure Enable Complete.
fn enable_complete(status: u8, handle: u16, config_id: u8) -> Vec<u8> {
    let mut params = vec![subevent::PROCEDURE_ENABLE_COMPLETE, status];
    params.extend_from_slice(&handle.to_le_bytes());
    params.push(config_id);
    params.extend_from_slice(&[0x01, 0x00, 0x00]);
    params.extend_from_slice(&[0x40, 0x0D, 0x00, 0x01]);
    params.extend_from_slice(&[0u8; 10]);
    le_meta(&params)
}

/// Command Complete for LE CS Set Procedure Parameters.
fn parameters_complete(status: u8, handle: u16) -> Vec<u8> {
    let mut packet = vec![0x04, 0x0E, 0x06, 0x01];
    packet.extend_from_slice(&opcode::LE_CS_SET_PROCEDURE_PARAMETERS);
    packet.push(status);
    packet.extend_from_slice(&handle.to_le_bytes());
    packet
}

/// An LE CS Subevent Result carrying `tones`.
fn subevent_result(handle: u16, config_id: u8, counter: u16, tones: &[Tone]) -> Vec<u8> {
    let mut params = vec![subevent::SUBEVENT_RESULT];
    params.extend_from_slice(&handle.to_le_bytes());
    params.push(config_id);
    params.extend_from_slice(&counter.to_le_bytes());
    params.extend_from_slice(&counter.to_le_bytes());
    params.extend_from_slice(&[0xFF, 0xFF]);
    params.push(0xC4); // reference power level
    params.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
    params.push(0x01); // antenna paths
    params.push(tones.len() as u8);
    for tone in tones {
        params.extend_from_slice(&[2, tone.channel, 5, 0x00]);
        let packed =
            (u32::from(tone.i as u16) & 0x0FFF) | ((u32::from(tone.q as u16) & 0x0FFF) << 12);
        params.extend_from_slice(&[packed as u8, (packed >> 8) as u8, (packed >> 16) as u8]);
        params.push(tone.quality);
    }
    le_meta(&params)
}

/// Tones one end would measure at `distance_m`, with `offsets` applied.
fn tones_at(distance_m: f64, offsets: &[f64], sign: f64) -> Vec<Tone> {
    use crate::controller::propagation::{channel_frequency_hz, propagation_phase_rad};
    offsets
        .iter()
        .enumerate()
        .map(|(step, offset)| {
            let channel = step as u8 * 2;
            let phase =
                propagation_phase_rad(distance_m, channel_frequency_hz(channel)) + sign * offset;
            Tone {
                channel,
                i: (phase.cos() * 2047.0).round() as i16,
                q: (phase.sin() * 2047.0).round() as i16,
                quality: 0,
            }
        })
        .collect()
}

#[test]
fn test_the_initiator_walks_the_whole_setup_sequence() {
    let mut initiator = CsInitiator::new(1);
    assert_eq!(initiator.state(), CsState::Idle);

    let started = initiator.start(0x0040);
    assert_eq!(&started[0][1..3], &opcode::LE_CS_SECURITY_ENABLE);
    assert_eq!(initiator.state(), CsState::Securing);

    let next = initiator.on_packet(&security_complete(0x00, 0x0040));
    assert_eq!(&next[0][1..3], &opcode::LE_CS_CREATE_CONFIG);
    assert_eq!(next[0][3], 28, "LE CS Create Config is 28 parameter bytes");
    assert_eq!(
        next[0][7], 0x01,
        "create context must configure the remote too, or the reflector never knows"
    );
    assert_eq!(next[0][14], cs_role::INITIATOR);
    assert_eq!(initiator.state(), CsState::Configuring);

    let next = initiator.on_packet(&config_complete(0x00, 0x0040, 1, cs_role::INITIATOR));
    assert_eq!(&next[0][1..3], &opcode::LE_CS_SET_PROCEDURE_PARAMETERS);
    assert_eq!(initiator.state(), CsState::SettingParameters);

    let next = initiator.on_packet(&parameters_complete(0x00, 0x0040));
    assert_eq!(&next[0][1..3], &opcode::LE_CS_PROCEDURE_ENABLE);
    assert_eq!(next[0][7], 0x01, "enable = 1");
    assert_eq!(initiator.state(), CsState::Enabling);

    initiator.on_packet(&enable_complete(0x00, 0x0040, 1));
    assert_eq!(initiator.state(), CsState::Measuring);
    assert!(initiator.is_measuring());
}

#[test]
fn test_a_refusal_at_any_step_is_recorded_and_the_sequence_stops() {
    // 0x0C = Command Disallowed, what a controller answers when the peer
    // does not support Channel Sounding at all.
    for (stage, packet) in [
        ("security", security_complete(0x0C, 0x0040)),
        (
            "config",
            config_complete(0x0C, 0x0040, 1, cs_role::INITIATOR),
        ),
    ] {
        let mut initiator = CsInitiator::new(1);
        initiator.start(0x0040);
        if stage == "config" {
            initiator.on_packet(&security_complete(0x00, 0x0040));
        }
        let next = initiator.on_packet(&packet);
        assert!(next.is_empty(), "{stage}: no command after a refusal");
        assert_eq!(initiator.state(), CsState::Failed(0x0C), "{stage}");
    }
}

/// An initiator that has reached `Measuring` on handle 0x0040.
fn measuring_initiator() -> CsInitiator {
    let mut initiator = CsInitiator::new(1);
    initiator.start(0x0040);
    initiator.on_packet(&security_complete(0x00, 0x0040));
    initiator.on_packet(&config_complete(0x00, 0x0040, 1, cs_role::INITIATOR));
    initiator.on_packet(&parameters_complete(0x00, 0x0040));
    initiator.on_packet(&enable_complete(0x00, 0x0040, 1));
    initiator
}

#[test]
fn test_no_estimate_exists_until_the_peers_data_arrives() {
    let mut initiator = measuring_initiator();
    let mut rng = crate::controller::propagation::Rng::new(1);
    let offsets: Vec<f64> = (0..19).map(|_| rng.uniform_phase()).collect();
    let local = tones_at(6.0, &offsets, 1.0);

    initiator.on_packet(&subevent_result(0x0040, 1, 5, &local));
    assert_eq!(initiator.pending_local_tones().len(), 19);
    assert!(
        initiator.estimate().is_none(),
        "half a measurement is not a measurement"
    );
    assert!(
        initiator.local_tones().is_empty(),
        "and half a measurement must not be reported as tones either"
    );

    let remote = RangingData {
        ranging_counter: 5,
        config_id: 1,
        selected_tx_power: 0,
        antenna_paths_mask: 0x01,
        reference_power_level: -60,
        tones: tones_at(6.0, &offsets, -1.0),
    };
    assert!(initiator.on_ranging_data(&remote.to_bytes()));
    let estimate = initiator.estimate().expect("both halves are in");
    assert!(
        (estimate.distance_m - 6.0).abs() < 0.1,
        "estimated {}",
        estimate.distance_m
    );
    assert_eq!(initiator.procedure_counts(), (1, 0));
}

#[test]
fn test_data_from_a_different_procedure_is_counted_not_combined() {
    // Tones from two procedures were measured with different oscillator
    // phases. Summing them cancels nothing, and would produce a confident
    // number unrelated to distance — so the counter has to gate it.
    let mut initiator = measuring_initiator();
    let mut rng = crate::controller::propagation::Rng::new(2);
    let offsets: Vec<f64> = (0..19).map(|_| rng.uniform_phase()).collect();
    initiator.on_packet(&subevent_result(
        0x0040,
        1,
        9,
        &tones_at(6.0, &offsets, 1.0),
    ));

    let stale = RangingData {
        ranging_counter: 8, // the previous procedure
        config_id: 1,
        selected_tx_power: 0,
        antenna_paths_mask: 0x01,
        reference_power_level: -60,
        tones: tones_at(6.0, &offsets, -1.0),
    };
    assert!(initiator.on_ranging_data(&stale.to_bytes()), "it parsed");
    assert!(initiator.estimate().is_none(), "but it was not combined");
    assert_eq!(initiator.procedure_counts(), (0, 1));
    assert!(
        initiator.combined_tones().is_empty(),
        "and nothing may be shown as a sum of two procedures' tones"
    );
}

#[test]
fn test_a_local_half_waiting_for_its_peer_is_not_a_mismatch() {
    // The local half always arrives first; the peer's has to cross a GATT
    // link. Counting that as a fault would report one on every single
    // measurement, which is how the demo page's counter first read
    // "70 procedures combined / 70 mismatched".
    let mut initiator = measuring_initiator();
    let mut rng = crate::controller::propagation::Rng::new(4);
    let offsets: Vec<f64> = (0..19).map(|_| rng.uniform_phase()).collect();

    for counter in 1..=3u16 {
        initiator.on_packet(&subevent_result(
            0x0040,
            1,
            counter,
            &tones_at(5.0, &offsets, 1.0),
        ));
        let peer = RangingData {
            ranging_counter: counter,
            config_id: 1,
            selected_tx_power: 0,
            antenna_paths_mask: 0x01,
            reference_power_level: -60,
            tones: tones_at(5.0, &offsets, -1.0),
        };
        initiator.on_ranging_data(&peer.to_bytes());
    }
    assert_eq!(
        initiator.procedure_counts(),
        (3, 0),
        "three measurements, no faults"
    );
    assert_eq!(initiator.measured_counter(), Some(3));
}

#[test]
fn test_the_tones_reported_all_come_from_one_procedure() {
    // What a caller shows must be internally consistent: the local tones,
    // the peer's, and their sums have to be from the same measurement, or
    // the sums shown are not the sums the distance was fitted to.
    let mut initiator = measuring_initiator();
    let mut rng = crate::controller::propagation::Rng::new(5);
    let offsets: Vec<f64> = (0..19).map(|_| rng.uniform_phase()).collect();
    initiator.on_packet(&subevent_result(
        0x0040,
        1,
        1,
        &tones_at(5.0, &offsets, 1.0),
    ));
    let peer = RangingData {
        ranging_counter: 1,
        config_id: 1,
        selected_tx_power: 0,
        antenna_paths_mask: 0x01,
        reference_power_level: -60,
        tones: tones_at(5.0, &offsets, -1.0),
    };
    initiator.on_ranging_data(&peer.to_bytes());
    let first_local = initiator.local_tones().to_vec();

    // The next procedure's local half arrives; its peer's has not.
    let later: Vec<f64> = (0..19).map(|_| rng.uniform_phase()).collect();
    initiator.on_packet(&subevent_result(0x0040, 1, 2, &tones_at(9.0, &later, 1.0)));
    assert_eq!(
        initiator.local_tones(),
        first_local.as_slice(),
        "the reported half must not race ahead of the peer's"
    );
    assert_eq!(initiator.measured_counter(), Some(1));
    assert_eq!(initiator.combined_tones().len(), 19);
    assert_eq!(initiator.procedure_counts(), (1, 0));
}

#[test]
fn test_results_for_another_connection_are_ignored() {
    let mut initiator = measuring_initiator();
    let tones = tones_at(3.0, &[0.0; 19], 1.0);
    initiator.on_packet(&subevent_result(0x0041, 1, 1, &tones));
    assert!(initiator.pending_local_tones().is_empty());
    initiator.on_packet(&subevent_result(0x0040, 1, 1, &tones));
    assert_eq!(initiator.pending_local_tones().len(), 19);
}

#[test]
fn test_malformed_ranging_data_is_rejected_rather_than_half_read() {
    let mut initiator = measuring_initiator();
    assert!(!initiator.on_ranging_data(&[]));
    assert!(!initiator.on_ranging_data(&[0x01, 0x02, 0x03]));
    assert!(initiator.estimate().is_none());
}

#[test]
fn test_the_reflector_publishes_only_what_it_was_configured_for() {
    let mut reflector = CsReflector::new();
    let tones = tones_at(4.0, &[0.0; 19], 1.0);

    // Before any configuration, results are not this device's business.
    reflector.on_packet(&subevent_result(0x0040, 1, 1, &tones));
    assert!(reflector.take_ranging_data().is_none());
    assert!(!reflector.is_configured());

    // A config naming this device the *initiator* must not make it a
    // reflector: it would then publish data nobody asked for.
    reflector.on_packet(&config_complete(0x00, 0x0040, 1, cs_role::INITIATOR));
    assert!(!reflector.is_configured());

    reflector.on_packet(&config_complete(0x00, 0x0040, 1, cs_role::REFLECTOR));
    assert!(reflector.is_configured());
    assert_eq!(reflector.connection_handle(), 0x0040);

    reflector.on_packet(&subevent_result(0x0040, 1, 7, &tones));
    let body = reflector.take_ranging_data().expect("data to publish");
    let parsed = RangingData::parse(&body).expect("well formed");
    assert_eq!(parsed.ranging_counter, 7);
    assert_eq!(parsed.tones.len(), 19);
    assert_eq!(reflector.subevent_count(), 1);
    assert!(reflector.take_ranging_data().is_none(), "drained");
}

#[test]
fn test_the_reflector_keeps_only_the_newest_procedure() {
    // An initiator drops data whose counter does not match its current
    // subevent, so publishing a backlog wastes the link and delivers
    // nothing.
    let mut reflector = CsReflector::new();
    reflector.on_packet(&config_complete(0x00, 0x0040, 1, cs_role::REFLECTOR));
    let tones = tones_at(4.0, &[0.0; 19], 1.0);
    for counter in 1..=4 {
        reflector.on_packet(&subevent_result(0x0040, 1, counter, &tones));
    }
    let body = reflector.take_ranging_data().unwrap();
    assert_eq!(RangingData::parse(&body).unwrap().ranging_counter, 4);
    assert!(reflector.take_ranging_data().is_none());
    assert_eq!(reflector.subevent_count(), 4, "all four were measured");
}

#[test]
fn test_stopping_disables_the_procedure_once() {
    let mut initiator = measuring_initiator();
    let stop = initiator.stop();
    assert_eq!(&stop[0][1..3], &opcode::LE_CS_PROCEDURE_ENABLE);
    assert_eq!(stop[0][7], 0x00, "enable = 0");
    assert_eq!(initiator.state(), CsState::Idle);
    assert!(initiator.stop().is_empty(), "already stopped");
}
