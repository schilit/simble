use super::*;

/// Runs ticks until `ready`, or panics. Ticks advance 100 ms each, the
/// same cadence the page uses.
fn drive(kit: &mut CarKit, mut until: impl FnMut(&CarKit) -> bool) -> Vec<CarKitEvent> {
    let mut events = Vec::new();
    for step in 0..400 {
        events.extend(kit.tick(step * 100));
        if until(kit) {
            return events;
        }
    }
    panic!(
        "car kit stalled at {:?} / {:?}: {:?}",
        kit.phase(),
        kit.call_phase(),
        kit.error()
    );
}

fn connected() -> CarKit {
    let mut kit = CarKit::new();
    kit.start();
    drive(&mut kit, |k| k.phase() == LinkPhase::Ready);
    kit
}

fn said(kit: &CarKit, text: &str) -> bool {
    kit.transcript().any(|line| line.text == text)
}

/// The link the AT bytes ride is a real BR/EDR one, and this is where
/// that stops being a claim: the head unit was told an *address*, and
/// everything it knows beyond that — the phone's Class of Device, its
/// name, its RFCOMM channel — it learned over the air.
#[test]
fn test_the_head_unit_finds_the_phone_by_inquiry_before_it_can_call_it() {
    let kit = connected();
    let head_unit = kit.scene.classic_device(kit.head_unit).expect("head unit");

    let found = head_unit
        .discovered()
        .iter()
        .find(|d| d.address == PHONE_ADDRESS)
        .expect("the inquiry found the phone");
    assert_eq!(
        found.class_of_device, PHONE_CLASS_OF_DEVICE,
        "the Class of Device is the phone's, read off an Inquiry Result"
    );
    assert_eq!(
        found.name.as_deref(),
        Some("Simble Phone"),
        "and its name, which only a Remote Name Request can supply — an \
             inquiry result carries none"
    );
}

/// Both ends agree there is an ACL connection, which is the difference
/// between a link and a drawing of one.
#[test]
fn test_the_at_bytes_ride_an_acl_connection_both_ends_can_see() {
    let kit = connected();
    let (car_handle, car_peer) = kit
        .scene
        .classic_device(kit.head_unit)
        .and_then(|d| d.host().connection())
        .expect("the head unit has an ACL connection");
    let (phone_handle, phone_peer) = kit
        .scene
        .classic_device(kit.phone)
        .and_then(|d| d.host().connection())
        .expect("the phone has one too");
    assert_eq!(car_peer, PHONE_ADDRESS);
    assert_eq!(phone_peer, HEAD_UNIT_ADDRESS);
    // Handles are allocated per controller, so they need not match — but
    // neither may be the "no connection" sentinel.
    assert_ne!(car_handle, 0);
    assert_ne!(phone_handle, 0);
}

/// The phases are walked in the order the sequence actually happens in,
/// and none is skipped. A link that reached `Ready` without ever being
/// in `Paging` would be a worse bug than one that never got there.
#[test]
fn test_the_link_walks_the_bredr_sequence_in_order() {
    let mut kit = CarKit::new();
    kit.start();
    let events = drive(&mut kit, |k| k.phase() == LinkPhase::Ready);
    let phases: Vec<LinkPhase> = events
        .into_iter()
        .filter_map(|e| match e {
            CarKitEvent::LinkPhase(p) => Some(p),
            _ => None,
        })
        .collect();
    let expected = [
        LinkPhase::Inquiring,
        LinkPhase::Paging,
        LinkPhase::Discovering,
        LinkPhase::OpeningDlc,
        LinkPhase::EstablishingSlc,
        LinkPhase::ConfiguringHeadUnit,
        LinkPhase::Ready,
    ];
    let mut cursor = 0;
    for phase in expected {
        let found = phases[cursor..]
            .iter()
            .position(|p| *p == phase)
            .unwrap_or_else(|| panic!("{phase:?} missing after {cursor}: {phases:?}"));
        cursor += found + 1;
    }
}

#[test]
fn test_the_head_unit_discovers_the_channel_rather_than_assuming_it() {
    let kit = connected();
    let detail = kit
        .steps()
        .into_iter()
        .find(|s| s.id == "sdp")
        .expect("sdp step")
        .detail;
    assert!(
        detail.contains(&format!("server channel {AG_RFCOMM_CHANNEL}")),
        "the SDP answer should name the channel: {detail}"
    );
    // The profile version came out of the same record. A
    // BluetoothProfileDescriptorList encodes it as `major << 8 | minor`
    // in decimal, so reading the minor as a nibble reports 1.9 as "v1.0"
    // — a wrong number that still looks like a version, which is the
    // only reason this is asserted rather than eyeballed.
    assert!(
        detail.contains("HFP v1.9"),
        "the record advertises HFP 1.9: {detail}"
    );
    assert!(
        detail.contains("bytes out"),
        "and the search was a real transaction with a size: {detail}"
    );
}

#[test]
fn test_the_service_level_connection_runs_in_the_order_the_profile_specifies() {
    let kit = connected();
    let commands: Vec<&str> = kit
        .transcript()
        .filter(|line| line.from_hf)
        .map(|line| line.text.as_str())
        .collect();
    let expected = [
        "AT+BRSF=",
        "AT+BAC=",
        "AT+CIND=?",
        "AT+CIND?",
        "AT+CMER=",
        "AT+CHLD=?",
        "AT+BIND=",
        "AT+BIND=?",
        "AT+BIND?",
    ];
    let mut cursor = 0;
    for prefix in expected {
        let found = commands[cursor..]
            .iter()
            .position(|c| c.starts_with(prefix))
            .unwrap_or_else(|| panic!("{prefix} missing after {cursor}: {commands:?}"));
        cursor += found + 1;
    }
}

#[test]
fn test_an_incoming_call_rings_the_head_unit_with_the_caller_id() {
    let mut kit = connected();
    assert!(kit.incoming_call("+15551234"));
    drive(&mut kit, |k| said(k, "RING"));

    assert!(said(&kit, "RING"));
    assert!(
        said(&kit, "+CLIP: \"+15551234\",129"),
        "the +CLIP line should carry the number: {:?}",
        kit.transcript().map(|l| &l.text).collect::<Vec<_>>()
    );
    // callsetup is the third indicator, so +CIEV names index 3.
    assert!(said(&kit, "+CIEV: 3,1"));
}

#[test]
fn test_answering_sends_ata_and_flips_the_call_indicator() {
    let mut kit = connected();
    kit.incoming_call("+15551234");
    drive(&mut kit, |k| said(k, "RING"));

    assert!(kit.answer());
    drive(&mut kit, |k| k.call_phase() == CallPhase::Active);

    assert!(said(&kit, "ATA"));
    // call is the second indicator; it goes up before callsetup goes down.
    let seq_call = kit
        .transcript()
        .find(|l| l.text == "+CIEV: 2,1")
        .expect("call = 1")
        .seq;
    let seq_setup = kit
        .transcript()
        .find(|l| l.text == "+CIEV: 3,0" && l.seq > seq_call)
        .expect("callsetup = 0 after call = 1")
        .seq;
    assert!(seq_setup > seq_call);
}

#[test]
fn test_the_head_unit_hangs_up_with_chup() {
    let mut kit = connected();
    kit.incoming_call("+15551234");
    drive(&mut kit, |k| said(k, "RING"));
    kit.answer();
    drive(&mut kit, |k| k.call_phase() == CallPhase::Active);

    assert!(kit.hang_up());
    drive(&mut kit, |k| k.call_phase() == CallPhase::Idle);
    assert!(said(&kit, "AT+CHUP"));
    assert!(said(&kit, "+CIEV: 2,0"));
}

#[test]
fn test_the_phone_can_end_the_call_without_the_head_unit_asking() {
    let mut kit = connected();
    kit.incoming_call("+15551234");
    drive(&mut kit, |k| said(k, "RING"));
    kit.answer();
    drive(&mut kit, |k| k.call_phase() == CallPhase::Active);

    let before = kit.transcript().filter(|l| l.text == "AT+CHUP").count();
    assert!(kit.phone_end_call());
    drive(&mut kit, |k| said(k, "+CIEV: 2,0"));
    assert_eq!(
        kit.transcript().filter(|l| l.text == "AT+CHUP").count(),
        before,
        "an AG-side hangup puts no command on the wire, only an indicator"
    );
}

#[test]
fn test_dialing_from_the_dashboard_uses_the_voice_call_form_of_atd() {
    let mut kit = connected();
    assert!(kit.car_dial("5550142"));
    drive(&mut kit, |k| k.call_phase() == CallPhase::Dialing);
    assert!(
        said(&kit, "ATD5550142;"),
        "HFP 4.19.1 requires the trailing semicolon"
    );
    drive(&mut kit, |k| k.call_phase() == CallPhase::Alerting);
    assert!(said(&kit, "+CIEV: 3,2"));
    assert!(said(&kit, "+CIEV: 3,3"));
}

#[test]
fn test_a_call_the_phone_placed_reaches_the_dashboard_with_no_command_at_all() {
    let mut kit = connected();
    assert!(kit.phone_dial("+15559876"));
    drive(&mut kit, |k| said(k, "+CIEV: 3,2"));
    assert!(!said(&kit, "ATD+15559876;"));
}

#[test]
fn test_the_head_unit_reads_the_operator_name_off_the_wire() {
    let kit = connected();
    assert!(said(&kit, "AT+COPS=3,0"));
    assert!(said(&kit, "AT+COPS?"));
    assert!(said(&kit, "+COPS: 0,0,\"Simble Mobile\""));
    assert_eq!(kit.car_operator.as_deref(), Some("Simble Mobile"));
}

#[test]
fn test_the_gain_knobs_are_the_profiles_own_commands() {
    let mut kit = connected();
    assert!(kit.set_speaker_gain(13));
    assert!(kit.set_microphone_muted(true));
    drive(&mut kit, |k| said(k, "AT+VGM=0"));
    assert!(said(&kit, "AT+VGS=13"));
}

#[test]
fn test_an_indicator_the_phone_moves_reaches_the_head_units_mirror() {
    let mut kit = connected();
    assert!(kit.set_indicator(AgIndicator::Signal, 1));
    drive(&mut kit, |k| {
        k.hf.ag_indicators
            .iter()
            .any(|s| s.indicator == AgIndicator::Signal && s.current_status == 1)
    });
    // signal is the fifth indicator.
    assert!(said(&kit, "+CIEV: 5,1"));
}

#[test]
fn test_the_call_indicators_are_not_settable_by_hand() {
    let mut kit = connected();
    assert!(!kit.set_indicator(AgIndicator::Call, 1));
    assert_eq!(kit.call_phase(), CallPhase::Idle);
}

#[test]
fn test_nothing_can_happen_before_the_link_is_up() {
    let mut kit = CarKit::new();
    assert!(!kit.incoming_call("+15551234"));
    assert!(!kit.answer());
    assert!(!kit.car_dial("123"));
    assert!(kit.transcript().next().is_none());
}

#[test]
fn test_the_dlc_negotiates_a_credit_window_rather_than_flowing_freely() {
    let kit = connected();
    let window = kit
        .hf_port
        .lock()
        .unwrap()
        .window()
        .expect("the data link is open");
    assert!(
        window.tx_credits > 0,
        "the head unit may only write while it holds credits"
    );
    assert!(window.rx_initial_credits > 0, "the phone granted credits");
    assert_eq!(
        window.dlci,
        AG_RFCOMM_CHANNEL << 1,
        "the DLCI is the server channel SDP advertised, doubled"
    );
}

#[test]
fn test_every_transcript_line_is_the_bytes_that_were_written() {
    let kit = connected();
    for line in kit.transcript() {
        let bytes: Vec<u8> = line
            .hex
            .split(' ')
            .map(|b| u8::from_str_radix(b, 16).expect("hex"))
            .collect();
        let decoded = String::from_utf8_lossy(&bytes);
        assert!(
            decoded.contains(&line.text),
            "{:?} is not in {decoded:?}",
            line.text
        );
        // Commands are \r-terminated, responses \r\n-wrapped: HFP 4.34.
        if line.from_hf {
            assert_eq!(*bytes.last().expect("nonempty"), b'\r');
        } else {
            assert!(bytes.starts_with(b"\r\n") && bytes.ends_with(b"\r\n"));
        }
    }
}

// --- the audio connection ----------------------------------------------

/// A Service Level Connection with no call has no audio. This is the
/// thing HFP separates the two connections *for*: a paired phone does
/// not hold a headset's microphone open all day.
#[test]
fn test_a_ready_link_carries_no_audio_until_there_is_a_call() {
    let kit = connected();
    assert_eq!(kit.audio_state(), AudioConnectionState::Disconnected);
    assert!(kit.audio_connection().is_none());
    assert_eq!(kit.audio_frames_received(), (0, 0));
}

/// The whole path, end to end: a call arrives, the codec is negotiated
/// over AT, the phone opens a synchronous link over HCI, and audio
/// crosses it in both directions on a handle that is not the ACL's.
#[test]
fn test_a_call_brings_up_a_real_sco_link_and_carries_audio_both_ways() {
    let mut kit = connected();
    assert!(kit.incoming_call("+15550142"));
    drive(&mut kit, |k| k.audio_connection().is_some());

    let sco = kit.audio_connection().expect("the audio link exists");
    let (acl_handle, _) = kit
        .scene
        .classic_device(kit.phone)
        .and_then(|d| d.host().connection())
        .expect("the ACL is still there");
    assert_ne!(
        sco.handle, acl_handle,
        "call audio has a handle of its own; addressing it to the ACL \
             handle is delivered to nobody"
    );
    assert_eq!(
        kit.audio_state(),
        AudioConnectionState::Connected,
        "and the profile has been told, not just the transport"
    );

    // Both ends agree, which is what separates a link from one end's
    // belief in one.
    let car_sco = kit
        .scene
        .classic_device(kit.head_unit)
        .and_then(ClassicDevice::sco)
        .expect("the head unit has the audio link too");
    assert_eq!(car_sco.handle, sco.handle);

    // Frames cross in both directions. `audio_frames_received` counts
    // what came *off* the link, so a count that moves is proof of
    // routing rather than of writing.
    drive(&mut kit, |k| {
        let (to_car, to_phone) = k.audio_frames_received();
        to_car > 2 && to_phone > 2
    });
}

/// mSBC needs an eSCO link and transparent air coding, and the codec
/// choice has to reach the *controller* — as a Voice Setting and a
/// packet-type mask — or the call comes up narrowband with everyone
/// still calling it wideband.
#[test]
fn test_the_negotiated_codec_decides_the_link_type_the_controller_makes() {
    let mut kit = connected();
    assert!(kit.incoming_call("+15550142"));
    drive(&mut kit, |k| k.audio_connection().is_some());

    let sco = kit.audio_connection().expect("the audio link exists");
    let codec = kit.ag.negotiated_codec();
    assert_eq!(
        codec,
        AudioCodec::Msbc,
        "both ends offer mSBC, so the AG must pick it over CVSD"
    );
    assert_eq!(sco.link_type, 0x02, "wideband speech rides eSCO, not SCO");
    assert_eq!(
        sco.air_mode, 0x03,
        "and transparent air coding, because the controller must not \
             touch an mSBC frame"
    );
}

/// Hanging up takes the audio and nothing else. A head unit that let the
/// ACL go here would pay for a whole inquiry-page-SDP-RFCOMM bring-up on
/// the next call.
#[test]
fn test_ending_a_call_drops_the_audio_and_keeps_the_service_level_connection() {
    let mut kit = connected();
    assert!(kit.incoming_call("+15550142"));
    drive(&mut kit, |k| k.audio_connection().is_some());
    assert!(kit.hang_up());
    drive(&mut kit, |k| k.audio_connection().is_none());

    assert_eq!(kit.audio_state(), AudioConnectionState::Disconnected);
    assert_eq!(kit.phase(), LinkPhase::Ready, "the SLC is still up");
    assert!(
        kit.scene
            .classic_device(kit.head_unit)
            .and_then(|d| d.host().connection())
            .is_some(),
        "and so is the ACL under it"
    );
    assert!(
        kit.hf_port.lock().ok().and_then(|p| p.window()).is_some(),
        "and the RFCOMM data link, which is what makes the next call cheap"
    );

    // And the link really comes back for a second call, on a fresh
    // handle — proof that nothing was left half-open.
    assert!(kit.incoming_call("+15550143"));
    drive(&mut kit, |k| k.audio_connection().is_some());
}

/// The negative case: a head unit that refuses audio leaves nothing
/// half-open at either end, and the call's signalling carries on.
#[test]
fn test_a_refused_audio_connection_leaves_no_handle_anywhere() {
    let mut kit = connected();
    if let Some(device) = kit.scene.classic_device_mut(kit.head_unit) {
        // 0x0D — Connection Rejected due to Limited Resources.
        device.set_sco_policy(crate::device::ScoPolicy::Reject(0x0D));
    }
    assert!(kit.incoming_call("+15550142"));

    // Give it as long as a successful setup would have taken, twice over.
    for step in 0..60 {
        kit.tick(step * 100);
    }
    // The setup really was attempted and really was refused — without
    // this the rest of the test passes just as well on a build where no
    // audio is ever opened at all.
    assert_eq!(
        kit.scene
            .classic_device(kit.phone)
            .and_then(ClassicDevice::sco_failure),
        Some(0x0D),
        "the phone must be told *why*, in a Synchronous Connection \
             Complete carrying the head unit's reason"
    );
    assert!(
        kit.audio_connection().is_none(),
        "the phone must not hold a handle the head unit refused"
    );
    assert!(
        kit.scene
            .classic_device(kit.head_unit)
            .and_then(ClassicDevice::sco)
            .is_none(),
        "and the head unit must not hold one it rejected"
    );
    assert_eq!(kit.audio_frames_received(), (0, 0), "no audio moved");
    assert_eq!(
        kit.call_phase(),
        CallPhase::Incoming,
        "the call itself is unaffected: AT signalling does not need SCO"
    );
    assert_eq!(kit.phase(), LinkPhase::Ready);
}

/// The Car page draws its SCO box solid off `sco_handle` being present,
/// so the JSON contract is part of the feature rather than a detail of
/// it: a renamed field leaves the page showing a dashed box beside a
/// working link and nothing anywhere says why.
#[test]
fn test_the_pages_status_carries_the_audio_connection() {
    let mut kit = connected();
    let idle = kit.status_json(0);
    assert!(idle.contains("\"audio\":\"disconnected\""), "{idle}");
    assert!(idle.contains("\"sco_handle\":null"), "{idle}");

    assert!(kit.incoming_call("+15550142"));
    drive(&mut kit, |k| k.audio_connection().is_some());
    drive(&mut kit, |k| k.audio_frames_received().0 > 0);

    let json = kit.status_json(0);
    assert!(json.contains("\"audio\":\"connected\""), "{json}");
    assert!(json.contains("\"sco_link_type\":\"eSCO\""), "{json}");
    assert!(json.contains("\"sco_air_mode\":\"transparent\""), "{json}");
    assert!(json.contains("\"codec\":\"mSBC\""), "{json}");
    assert!(!json.contains("\"sco_handle\":null"), "{json}");
    assert!(!json.contains("\"audio_frames_to_car\":0"), "{json}");
}
