// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! The Auracast profile bindings: a scripted broadcast source, and a scripted
//! Broadcast Assistant against a scripted Scan Delegator.
//!
//! # What these tests prove, and what they do not
//!
//! Both endpoints are simble's, and so is the controller between them, so
//! every test here would pass while both halves were wrong in the same way —
//! the failure mode this repo has hit repeatedly (an `LE Create Connection`
//! with 12 of its 25 parameter bytes passed every simulated test and was
//! rejected by the first real controller). Nothing below is evidence about the
//! wire.
//!
//! What they prove is the *binding surface* and one thing more:
//!
//! * that `start_broadcast`'s map literal reaches [`BigBroadcaster`] and gets a
//!   BIG on the air that an independent [`BigReceiver`] — the same one checked
//!   against Bumble in `tests/interop/auracast_*.py` — can find, join and
//!   decode the BASE of;
//! * that a script's `add_source` reaches BASS's control point as a real
//!   Add Source, and the Broadcast Receive State comes back over GATT;
//! * that **the Receive State only reads `SynchronizedToPa` when something
//!   reported a synchronisation**, which is the regression that would have
//!   caught `bass.rs` claiming it unconditionally. That claim was made to a
//!   *foreign* peer, and nothing simble-against-simble could see it, because
//!   there was no scripted Assistant to assert against it. There is now.
//!
//! The evidence about the wire is `tests/interop/auracast_source.py` and
//! `auracast_sink.py`. Nothing here substitutes for those.
//!
//! [`BigBroadcaster`]: simble::device::big_broadcaster::BigBroadcaster
//! [`BigReceiver`]: simble::device::big_receiver::BigReceiver

use simble::controller::sim::Link;
use simble::device::big_receiver::{BigReceiver, ReceiverConfig, ReceiverState};
use simble::transport::wasm_ws::{SceneEngine, ScriptedPeripheral};

mod common;
use common::address;

// ---------------------------------------------------------------------------
// android::BluetoothLeBroadcast
// ---------------------------------------------------------------------------

/// The catalog's own Auracast source, since a binding with no runnable example
/// is undocumented and this is the example.
fn catalog_source() -> &'static str {
    simble::devices::catalog::script("auracast_source").expect("the catalog has an Auracast source")
}

/// Runs a scripted peripheral and a real [`BigReceiver`] on one medium until
/// `done`, or gives up. Returns both so the test can look at either.
fn run_source_and_sink(
    script: &str,
    ticks: usize,
    done: impl Fn(&BigReceiver) -> bool,
) -> (ScriptedPeripheral, BigReceiver, bool) {
    let mut link = Link::new();
    let source_channel = link.add_device(address(0x01));
    let mut source = ScriptedPeripheral::run_script(script).expect("the source script runs");
    source.set_identity(address(0x01));
    source
        .queue_start(&source_channel)
        .expect("the source starts");

    let sink_channel = link.add_device(address(0x10));
    let mut sink = BigReceiver::new(ReceiverConfig::default());
    for packet in sink.start() {
        sink_channel.inject_host_packet(packet).expect("queued");
    }

    let mut reached = false;
    for step in 0..ticks {
        source
            .tick(&source_channel, step as f64 * 0.05)
            .expect("the source ticks");
        while let Some(packet) = sink_channel.poll_controller_packet() {
            for reply in sink.on_packet(&packet) {
                sink_channel.inject_host_packet(reply).expect("queued");
            }
        }
        link.tick();
        while let Some(packet) = source_channel.poll_controller_packet() {
            source
                .handle_packet(&source_channel, &packet)
                .expect("the source handles its controller's packets");
        }
        if done(&sink) {
            reached = true;
            break;
        }
    }
    (source, sink, reached)
}

#[test]
fn a_scripted_source_puts_a_big_on_the_air_that_a_real_receiver_joins() {
    let (source, sink, joined) =
        run_source_and_sink(catalog_source(), 80, BigReceiver::is_receiving);
    assert!(
        joined,
        "the scripted source never got a receiver onto its BIG: {:?}",
        sink.state()
    );
    assert_eq!(sink.state(), ReceiverState::Receiving);

    // What the receiver found is what the script asked for — the map literal
    // in `start_broadcast` crossed the extended advertisement, and nothing
    // between here and there invented it.
    let found = sink.found().expect("a source was found");
    assert_eq!(found.broadcast_id, 0x00C0_FFEE, "the script's Broadcast_ID");
    let big_info = sink.big_info().expect("BIGInfo arrived");
    assert_eq!(big_info.num_bis, 2, "the script asked for two BISes");
    assert_eq!(
        big_info.sdu_interval.get(),
        10_000,
        "lc3_48_2 is 10 ms frames"
    );
    assert_eq!(big_info.max_sdu.get(), 100, "lc3_48_2 is 100 octets");
    assert_eq!(sink.bis_handles().len(), 2);

    // And the peripheral half of the same device is unharmed: a broadcast
    // source that stopped being a GATT server would be a regression in the
    // hosting, not in the binding.
    let status = source.status_json();
    assert!(status.contains("180F"), "still a battery service: {status}");
    assert!(
        !status.contains("\"last_error\":\"") || status.contains("\"last_error\":null"),
        "the source script reported an error: {status}"
    );
}

#[test]
fn audio_the_script_writes_reaches_the_receivers_own_bis_handles() {
    let (_source, mut sink, joined) = run_source_and_sink(catalog_source(), 120, |sink| {
        sink.is_receiving() && sink.sdu_count() >= 2
    });
    assert!(joined, "no audio arrived: {:?}", sink.state());

    let handles = sink.bis_handles().to_vec();
    let left = sink.poll_sdu().expect("left channel");
    let right = sink.poll_sdu().expect("right channel");
    assert_eq!(left.handle, handles[0]);
    assert_eq!(right.handle, handles[1]);
    assert_eq!(left.payload.len(), 100, "one lc3_48_2 frame per SDU");
}

#[test]
fn a_settings_map_that_the_base_cannot_carry_is_refused_rather_than_published() {
    // The BASE assigns channel allocations by BIS index, so a script that
    // names something else is describing a broadcast that would not go on the
    // air. Silently publishing a different one is exactly the class of bug
    // this project keeps paying for.
    let error = simble::transport::wasm_ws::run_test_script(
        r#"
let broadcast = android::BluetoothLeBroadcast("Bad");
broadcast.start_broadcast(#{
    broadcast_id: 0x000001,
    subgroups: [ #{ bis: ["FRONT_RIGHT", "FRONT_LEFT"] } ],
});
"#,
    )
    .expect_err("the allocation disagrees");
    assert!(error.contains("Front Left"), "{error}");

    // Same for a codec configuration nobody has checked: a plausible-looking
    // name is worse than an error, because it ships.
    let error = simble::transport::wasm_ws::run_test_script(
        r#"
let broadcast = android::BluetoothLeBroadcast("Bad");
broadcast.start_broadcast(#{ broadcast_id: 1, subgroups: [ #{ codec: "lc3_16_2" } ] });
"#,
    )
    .expect_err("unverified codec name");
    assert!(error.contains("lc3_48_2"), "{error}");

    // A Broadcast_ID is 24 bits; a wider one would be truncated into a
    // different broadcast than the script named.
    let error = simble::transport::wasm_ws::run_test_script(
        r#"
let broadcast = android::BluetoothLeBroadcast("Bad");
broadcast.start_broadcast(#{ broadcast_id: 0x01000000 });
"#,
    )
    .expect_err("too wide");
    assert!(error.contains("24-bit"), "{error}");
}

#[test]
fn the_metadata_a_source_publishes_is_what_an_assistant_needs_to_add_it() {
    // The two bindings meet here: `get_all_broadcast_metadata` produces the
    // map `add_source` consumes. If these two disagree the pair is useless,
    // and nothing else would notice.
    let script = r#"
let server = android::BluetoothGattServer("Src");
let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
server.add_service(bas);
let broadcast = android::BluetoothLeBroadcast("Src");
broadcast.start_broadcast(#{ broadcast_id: 0xC0FFEE, advertising_sid: 3 });

let metadata = broadcast.get_all_broadcast_metadata();
assert(metadata.len() == 1, "one broadcast");
assert(metadata[0].broadcast_id == 0xC0FFEE, "the id the script chose");
assert(metadata[0].source_advertising_sid == 3, "the SID the script chose");
assert(metadata[0].encrypted == false, "no broadcast code was given");
assert(metadata[0].subgroups[0].bis_sync == 0x03, "two BISes, bits 0 and 1");

// An Assistant accepts it unchanged — the point of the pair.
let assistant = android::BluetoothLeBroadcastAssistant("Phone");
assistant.add_source("AA:BB:CC:00:00:09", metadata[0], false);
"#;
    simble::transport::wasm_ws::run_test_script(script).expect("the two halves agree");
}

// ---------------------------------------------------------------------------
// android::BluetoothLeBroadcastAssistant against a Scan Delegator
// ---------------------------------------------------------------------------

/// A Scan Delegator with a radio that never reports anything — the state a
/// real earbud is in while it is still hunting for the periodic train.
const SILENT_DELEGATOR: &str = r#"
let server = android::BluetoothGattServer("Earbud");
server.add_bass(1);
server.advertise_service_uuid(0x184F);
"#;

/// The same Delegator, with something standing in for a radio that eventually
/// succeeds. It reports the outcome once, after a delay, so the test can watch
/// the Receive State change from the Assistant's side rather than its own.
const SYNCING_DELEGATOR: &str = r#"
let server = android::BluetoothGattServer("Earbud");
server.add_bass(1);
server.advertise_service_uuid(0x184F);

fn tick(server, t) {
    if t < 2.0 { return; }
    for state in server.receive_states() {
        if state.pa_sync_state == "SynchronizedToPa" { continue; }
        server.report_sync_outcome(state.source_id, "SynchronizedToPa", 0x03);
    }
}
"#;

const ASSISTANT: &str = r#"
let assistant = android::BluetoothLeBroadcastAssistant("Phone");
assistant.add_source("AA:BB:CC:00:00:01", #{
    source_device: "AA:BB:CC:00:00:07",
    source_address_type: 0,
    source_advertising_sid: 0,
    broadcast_id: 0xC0FFEE,
    pa_sync_interval: 80,
    subgroups: [ #{ bis_sync: 0x03 } ],
}, false);

fn on_source_added(assistant, sink, source_id, reason) {
    assistant.emit("added", source_id);
}
fn on_receive_state_changed(assistant, sink, source_id, state) {
    assistant.emit("pa_sync_state", state.pa_sync_state);
    assistant.emit("bis_sync", state.subgroups[0].bis_sync);
}
"#;

/// Runs a Delegator and an Assistant together, returning what the Assistant
/// emitted and whether an assertion inside it failed.
fn run_pair(
    delegator: &str,
    assistant: &str,
    ticks: usize,
) -> (Vec<(String, String)>, Option<String>) {
    let mut scene = SceneEngine::new();
    scene
        .add_peripheral(address(0x01), delegator)
        .expect("the delegator script runs");
    let phone = scene
        .add_scripted_central(address(0x99), assistant)
        .expect("the assistant script runs");
    for step in 0..ticks {
        scene.tick(step as f64 * 0.05);
    }
    let failure = scene
        .scripted_central(phone)
        .and_then(|c| c.failure())
        .map(str::to_string);
    let emitted = scene
        .scripted_central_mut(phone)
        .expect("a scripted central")
        .take_emitted()
        .into_iter()
        .map(|line| {
            let value: serde_json::Value = serde_json::from_str(&line).expect("emit is JSON");
            let payload = match &value["payload"] {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            (
                value["event"].as_str().unwrap_or_default().to_string(),
                payload,
            )
        })
        .collect();
    (emitted, failure)
}

#[test]
fn an_add_source_from_a_script_reaches_bass_and_the_receive_state_comes_back() {
    let (emitted, failure) = run_pair(SILENT_DELEGATOR, ASSISTANT, 60);
    assert_eq!(failure, None, "no assertion failed");

    // BASS assigned the Source_ID, and the Assistant learned it from the
    // Receive State rather than inventing one.
    let added: Vec<&(String, String)> = emitted.iter().filter(|(k, _)| k == "added").collect();
    assert_eq!(added.len(), 1, "one source was added: {emitted:?}");
    assert_eq!(added[0].1, "0", "the first Source_ID BASS hands out");
}

/// The regression this pair exists for.
///
/// Add Source *requests* a synchronisation. Until this week the service
/// answered `SynchronizedToPa` the moment an Assistant asked — a claim about a
/// broadcast that need not exist, made to a foreign peer, and invisible to
/// every simble-against-simble test because nothing on this side ever read it
/// back. This is the test that would have failed.
#[test]
fn a_delegator_that_has_not_synchronised_does_not_claim_it_has() {
    let (emitted, failure) = run_pair(SILENT_DELEGATOR, ASSISTANT, 60);
    assert_eq!(failure, None);

    let states: Vec<&str> = emitted
        .iter()
        .filter(|(kind, _)| kind == "pa_sync_state")
        .map(|(_, value)| value.as_str())
        .collect();
    assert!(!states.is_empty(), "a Receive State arrived: {emitted:?}");
    assert!(
        states.iter().all(|state| *state == "NotSynchronizedToPa"),
        "a Delegator with no radio must not claim a sync; got {states:?}"
    );

    // And no BIS is synchronised either, because no PA is.
    let bis: Vec<&str> = emitted
        .iter()
        .filter(|(kind, _)| kind == "bis_sync")
        .map(|(_, value)| value.as_str())
        .collect();
    assert!(
        bis.iter().all(|bits| *bits == "0"),
        "no BIS can be synchronised while the PA is not; got {bis:?}"
    );
}

#[test]
fn the_receive_state_reaches_synchronised_only_when_a_sync_is_reported() {
    let (emitted, failure) = run_pair(SYNCING_DELEGATOR, ASSISTANT, 120);
    assert_eq!(failure, None);

    let states: Vec<&str> = emitted
        .iter()
        .filter(|(kind, _)| kind == "pa_sync_state")
        .map(|(_, value)| value.as_str())
        .collect();
    assert_eq!(
        states.first(),
        Some(&"NotSynchronizedToPa"),
        "Add Source only requests: {states:?}"
    );
    assert_eq!(
        states.last(),
        Some(&"SynchronizedToPa"),
        "report_sync_outcome is what moves it: {states:?}"
    );
    // The later state arrived as a *notification*, not a second read: the
    // Assistant reads the Receive State exactly once, on discovery.
    assert!(
        states.len() > 1,
        "the Delegator notified the change: {states:?}"
    );

    let bis: Vec<&str> = emitted
        .iter()
        .filter(|(kind, _)| kind == "bis_sync")
        .map(|(_, value)| value.as_str())
        .collect();
    assert_eq!(
        bis.last(),
        Some(&"3"),
        "the BIS bits the radio actually joined: {bis:?}"
    );
}

#[test]
fn remove_source_clears_the_slot_and_answers_the_script() {
    let assistant = r#"
let assistant = android::BluetoothLeBroadcastAssistant("Phone");
assistant.add_source("AA:BB:CC:00:00:01", #{
    source_device: "AA:BB:CC:00:00:07",
    broadcast_id: 0xC0FFEE,
    subgroups: [ #{ bis_sync: 0x03 } ],
}, false);

fn on_source_added(assistant, sink, source_id, reason) {
    assistant.emit("added", source_id);
    assistant.remove_source(sink, source_id);
}
fn on_source_removed(assistant, sink, source_id, reason) {
    assistant.emit("removed", source_id);
    assert(assistant.get_all_sources(sink).len() == 0, "the slot is free again");
}
fn on_source_remove_failed(assistant, sink, source_id, reason) {
    assert(false, "the removal was refused");
}
"#;
    let (emitted, failure) = run_pair(SILENT_DELEGATOR, assistant, 80);
    assert_eq!(failure, None);
    let kinds: Vec<&str> = emitted.iter().map(|(kind, _)| kind.as_str()).collect();
    assert!(kinds.contains(&"added"), "{emitted:?}");
    assert!(kinds.contains(&"removed"), "{emitted:?}");
}

#[test]
fn a_source_id_the_delegator_never_assigned_is_refused_rather_than_ignored() {
    // BASS answers an unknown Source_ID with application error 0x81. A binding
    // that swallowed it would leave a script waiting for a callback that never
    // comes — the failure mode `on_error` exists for in the central role.
    let assistant = r#"
let assistant = android::BluetoothLeBroadcastAssistant("Phone");
assistant.remove_source("AA:BB:CC:00:00:01", 42);

fn on_source_remove_failed(assistant, sink, source_id, reason) {
    assistant.emit("refused", reason);
}
"#;
    let (emitted, failure) = run_pair(SILENT_DELEGATOR, assistant, 60);
    assert_eq!(failure, None);
    let refused = emitted
        .iter()
        .find(|(kind, _)| kind == "refused")
        .expect("the removal was refused: {emitted:?}");
    assert_eq!(refused.1, "129", "BASS Invalid Source Id is 0x81");
}

#[test]
fn remote_scan_started_reaches_the_delegator() {
    let assistant = r#"
let assistant = android::BluetoothLeBroadcastAssistant("Phone");
assistant.add_source("AA:BB:CC:00:00:01", #{
    source_device: "AA:BB:CC:00:00:07",
    broadcast_id: 0xC0FFEE,
}, false);
assistant.start_searching_for_sources();

fn on_search_started(assistant, reason) {
    assistant.emit("searching", reason);
}
"#;
    let (emitted, failure) = run_pair(SILENT_DELEGATOR, assistant, 60);
    assert_eq!(failure, None);
    assert!(
        emitted.iter().any(|(kind, _)| kind == "searching"),
        "Remote Scan Started was accepted: {emitted:?}"
    );
}

#[test]
fn searching_before_a_sink_is_known_says_why_rather_than_doing_nothing() {
    let script = r#"
let assistant = android::BluetoothLeBroadcastAssistant("Phone");
assistant.start_searching_for_sources();
"#;
    let error =
        simble::transport::wasm_ws::run_test_script(script).expect_err("there is no sink to tell");
    assert!(error.contains("not talking to a sink"), "{error}");
}
