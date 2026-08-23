// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! A Channel Sounding initiator and reflector, on one connection, talking to
//! each other through the simulated controller — with the reflector's half of
//! every measurement carried to the initiator the way the Channel Sounding
//! Profile carries it, in Ranging Service segments.
//!
//! # What these tests can and cannot prove
//!
//! Both hosts are this codebase and so is the controller between them. Nothing
//! here is evidence that Simble's Channel Sounding is correct on the air: a
//! wrong field offset that both ends agree about passes every test below.
//! `tests/bumble_vectors_test.rs` opens with the general form of this
//! argument, and neither Bumble nor Zephyr implements the Ranging Service, so
//! there is currently no foreign peer to check RAS against at all.
//!
//! What is being tested is **sequence, state and teardown**: that the four
//! commands of the setup ladder are issued in order and only in order, that
//! each refusal stops the sequence rather than hanging it, that a reflector
//! learns it is in a procedure only through the event the spec says tells it,
//! that a procedure ends when it is told to and dies with its connection, and
//! that a distance exists at exactly one place — the initiator, after both
//! halves have met.
//!
//! That last one is the profile's whole reason to exist, so it is asserted
//! from several directions in
//! [`no_distance_ever_crosses_the_link`] and
//! [`the_peers_half_alone_is_not_a_measurement`]: a test that showed a
//! distance arriving over RAS would be asserting a bug, not a feature.

use simble::controller::sim::Link;
use simble::cs::tones::Tone;
use simble::device::channel_sounding::{CsInitiator, CsReflector, CsState, cs_role, opcode};
use simble::device::host::command;
use simble::packets::HciEvent;
use simble::profiles::ras::{self, RangingData};
use simble::transport::HciChannel;
use simble::types::Address;
use std::sync::Arc;

/// The ATT MTU the notification capacity is taken from, so the Ranging Data
/// really is segmented rather than fitting in one write. `MTU - 3` is the
/// value capacity of a notification.
const NOTIFICATION_CAPACITY: usize = 64 - 3;

/// An initiator and a reflector on one simulated medium, with the Ranging
/// Service's segmentation between them.
struct CsScene {
    link: Link,
    initiator_channel: Arc<HciChannel>,
    reflector_channel: Arc<HciChannel>,
    initiator: CsInitiator,
    reflector: CsReflector,
    /// The connection both ends are on, once it is up.
    connection_handle: Option<u16>,
    /// Whether the initiator has been told to start ranging.
    started: bool,
    /// Whether the Ranging Data segments are actually delivered. Turning this
    /// off is how a test sees what the initiator has without the peer's half.
    deliver_ras: bool,
    /// Reassembles the segments on the initiator's side, as its host would.
    segments: ras::Reassembler,
    /// Every Ranging Data body the reflector published, in order.
    published: Vec<Vec<u8>>,
    /// Every segment as it went over the notional GATT link.
    wire: Vec<Vec<u8>>,
}

impl CsScene {
    /// Two devices two metres apart, advertising and connecting.
    fn new(separation_m: f64) -> Self {
        use simble::controller::propagation::Position;

        let mut link = Link::new();
        let reflector_channel = link.add_device(reflector_address());
        let initiator_channel = link.add_device(initiator_address());
        link.set_position(initiator_address(), Position::new(0.0, 0.0));
        link.set_position(reflector_address(), Position::new(separation_m, 0.0));
        link.set_noise_seed(7);

        // The reflector advertises; the initiator scans and connects.
        reflector_channel
            .send_command(&[0x0A, 0x20, 0x01, 0x01])
            .expect("advertising enable");
        initiator_channel
            .send_command(&[0x0C, 0x20, 0x02, 0x01, 0x00])
            .expect("scan enable");
        initiator_channel
            .send_command(&create_connection(reflector_address()))
            .expect("create connection");

        Self {
            link,
            initiator_channel,
            reflector_channel,
            initiator: CsInitiator::new(1),
            reflector: CsReflector::new(),
            connection_handle: None,
            started: false,
            deliver_ras: true,
            segments: ras::Reassembler::new(),
            published: Vec::new(),
            wire: Vec::new(),
        }
    }

    /// Advances the scene one step.
    fn tick(&mut self) {
        while let Some(packet) = self.initiator_channel.poll_controller_packet() {
            if let Some(HciEvent::LeConnectionComplete(event)) = HciEvent::parse_h4(&packet)
                && event.status == 0x00
            {
                self.connection_handle = Some(event.connection_handle.get() & 0x0FFF);
            }
            for reply in self.initiator.on_packet(&packet) {
                let _ = self.initiator_channel.send_command(&reply[1..]);
            }
        }
        while let Some(packet) = self.reflector_channel.poll_controller_packet() {
            self.reflector.on_packet(&packet);
        }

        // The reflector's half, over the Ranging Service: one body becomes
        // several notification-sized segments, which the initiator's host
        // reassembles. Nothing about this is a shortcut — a procedure's tones
        // are a few hundred octets and never fit one notification.
        while let Some(body) = self.reflector.take_ranging_data() {
            self.published.push(body.clone());
            for segment in ras::segment(&body, NOTIFICATION_CAPACITY) {
                self.wire.push(segment.clone());
                if !self.deliver_ras {
                    continue;
                }
                if let Some(reassembled) = self.segments.push(&segment) {
                    self.initiator.on_ranging_data(&reassembled);
                }
            }
        }

        self.link.tick();
    }

    /// Ticks until the connection is up, then starts the initiator's ladder.
    fn connect_and_start(&mut self) {
        for _ in 0..10 {
            self.tick();
            if let Some(handle) = self.connection_handle
                && !self.started
            {
                for packet in self.initiator.start(handle) {
                    let _ = self.initiator_channel.send_command(&packet[1..]);
                }
                self.started = true;
                return;
            }
        }
        panic!("the two devices never connected");
    }

    /// Ticks until `done` holds, or gives up after `ticks`.
    fn run_until(&mut self, ticks: usize, done: impl Fn(&Self) -> bool) -> bool {
        for _ in 0..ticks {
            self.tick();
            if done(self) {
                return true;
            }
        }
        false
    }

    /// Ticks until an estimate exists.
    fn run_until_measured(&mut self, ticks: usize) -> bool {
        self.run_until(ticks, |scene| scene.initiator.estimate().is_some())
    }

    /// Sends a raw command on the initiator's channel.
    fn send_from_initiator(&self, packet: &[u8]) {
        let _ = self.initiator_channel.send_command(packet);
    }
}

fn initiator_address() -> Address {
    "AA:BB:CC:00:00:01".parse().expect("a valid address")
}

fn reflector_address() -> Address {
    "AA:BB:CC:00:00:02".parse().expect("a valid address")
}

/// LE Create Connection's 25 parameter bytes, without the H4 type byte.
fn create_connection(target: Address) -> Vec<u8> {
    let mut params = Vec::with_capacity(25);
    params.extend_from_slice(&0x0060u16.to_le_bytes()); // scan interval
    params.extend_from_slice(&0x0030u16.to_le_bytes()); // scan window
    params.push(0x00); // filter policy: use the peer address
    params.push(0x00); // peer address type: public
    let mut peer = target.to_be_bytes();
    peer.reverse();
    params.extend_from_slice(&peer);
    params.push(0x00); // own address type: public
    params.extend_from_slice(&0x0018u16.to_le_bytes()); // min connection interval
    params.extend_from_slice(&0x0028u16.to_le_bytes()); // max connection interval
    params.extend_from_slice(&0x0000u16.to_le_bytes()); // max latency
    params.extend_from_slice(&0x00C8u16.to_le_bytes()); // supervision timeout
    params.extend_from_slice(&0x0000u16.to_le_bytes()); // min CE length
    params.extend_from_slice(&0x0000u16.to_le_bytes()); // max CE length
    let packet = command([0x0D, 0x20], &params);
    packet[1..].to_vec()
}

/// The initiator drives the whole procedure: four commands, each one issued
/// only after the event that answers the last. Nothing in the ladder may be
/// skipped or reordered — a controller refuses Set Procedure Parameters for a
/// configuration that does not exist, and Procedure Enable before parameters.
#[test]
fn the_setup_ladder_runs_in_order_from_both_ends() {
    let mut scene = CsScene::new(6.0);
    scene.connect_and_start();
    assert_eq!(scene.initiator.state(), CsState::Securing);

    // Each tick carries the ladder one rung further, and the states have to be
    // reached in this order.
    let mut seen = vec![scene.initiator.state()];
    for _ in 0..12 {
        scene.tick();
        let state = scene.initiator.state();
        if seen.last() != Some(&state) {
            seen.push(state);
        }
        if state == CsState::Measuring {
            break;
        }
    }
    assert_eq!(
        seen,
        vec![
            CsState::Securing,
            CsState::Configuring,
            CsState::SettingParameters,
            CsState::Enabling,
            CsState::Measuring,
        ],
        "the ladder must be climbed a rung at a time"
    );

    // The reflector is configured into the procedure by the initiator's
    // Create_Context, and learns of it only through LE CS Config Complete —
    // nothing in this harness tells it anything else, and it issues no
    // commands of its own (`CsReflector::on_packet` returns nothing to send,
    // which is the type system carrying that rule rather than a test).
    assert!(scene.reflector.is_configured());
    assert_eq!(
        scene.reflector.connection_handle(),
        scene.connection_handle.unwrap()
    );

    // Both ends measure. Half a measurement each.
    assert!(scene.run_until(10, |s| s.reflector.subevent_count() > 0));
    assert!(!scene.initiator.pending_local_tones().is_empty());
}

/// Every rung of the ladder ends in a real distance, and it ends up in exactly
/// one place: the initiator, after the reflector's half has crossed the link.
#[test]
fn both_halves_together_recover_the_true_separation() {
    for truth in [2.0, 7.5, 15.0] {
        let mut scene = CsScene::new(truth);
        scene.connect_and_start();
        assert!(
            scene.run_until_measured(30),
            "no estimate at {truth} m: {:?}",
            scene.initiator.state()
        );
        let estimate = scene.initiator.estimate().expect("both halves are in");
        assert!(
            (estimate.distance_m - truth).abs() < 0.5,
            "true {truth} m, estimated {} m",
            estimate.distance_m
        );
        let (combined, mismatched) = scene.initiator.procedure_counts();
        assert!(combined > 0);
        assert_eq!(mismatched, 0, "every published body matched a local half");
    }
}

/// The Ranging Service carries a peer's raw steps, never a distance. If a
/// distance crossed the link the profile would not need to exist, and a test
/// that asserted one had would be asserting a bug.
#[test]
fn no_distance_ever_crosses_the_link() {
    let truth = 9.0;
    let mut scene = CsScene::new(truth);
    scene.connect_and_start();
    assert!(scene.run_until_measured(30));

    let body = scene.published.first().expect("something was published");
    let parsed = RangingData::parse(body).expect("a well formed Ranging Data body");
    assert_eq!(parsed.tones.len(), 19, "one mode-2 step per tone");

    // Nothing in the octets that went over the link decodes as the distance,
    // at any alignment, in either float width. The tones are all that crossed.
    for wire_bytes in std::iter::once(body).chain(scene.wire.iter()) {
        for window in wire_bytes.windows(4) {
            let value = f32::from_le_bytes(window.try_into().expect("four bytes"));
            assert!(
                !(f64::from(value) - truth).abs().lt(&0.5),
                "a distance-like {value} was found in the bytes that crossed the link"
            );
        }
        for window in wire_bytes.windows(8) {
            let value = f64::from_le_bytes(window.try_into().expect("eight bytes"));
            assert!(
                !(value - truth).abs().lt(&0.5),
                "a distance-like {value} was found in the bytes that crossed the link"
            );
        }
    }
}

/// The other half of the same claim: the peer's data on its own means nothing.
/// A receiver that treated a Ranging Data body as a measurement — rather than
/// as one term of a sum it has to supply the other term of — would report a
/// number here, and there is no number to report.
#[test]
fn the_peers_half_alone_is_not_a_measurement() {
    let mut scene = CsScene::new(9.0);
    scene.deliver_ras = false;
    scene.connect_and_start();
    assert!(
        scene.run_until(30, |s| !s.published.is_empty()),
        "the reflector never published its half"
    );

    // The initiator's own controller is producing tones, and still there is no
    // distance: one end's measurements are uniform noise until they are summed
    // with the other end's.
    assert_eq!(scene.initiator.pending_local_tones().len(), 19);
    assert!(scene.initiator.estimate().is_none());
    assert_eq!(scene.initiator.procedure_counts(), (0, 0));

    // And the peer's body, handed to a host with no local half at all, still
    // yields nothing — it parses, and that is as far as it goes.
    let body = scene.published.first().expect("published");
    let mut bare = CsInitiator::new(1);
    assert!(bare.on_ranging_data(body), "the body is well formed");
    assert!(
        bare.estimate().is_none(),
        "a Ranging Data body is half a measurement, not a distance"
    );

    // Turning delivery back on completes the very next procedure.
    scene.deliver_ras = true;
    assert!(
        scene.run_until_measured(30),
        "delivering the peer's half must complete a measurement"
    );
}

/// Ranging runs on a connection. A host that asks to range on a handle that
/// names none is refused with Command Status, and the completion subevent it
/// would otherwise wait for never comes — so a state machine that ignored
/// Command Status would sit in `Securing` forever, indistinguishable from a
/// procedure that is merely slow to start.
#[test]
fn ranging_on_a_handle_that_is_not_connected_fails_rather_than_hangs() {
    let mut scene = CsScene::new(4.0);
    for packet in scene.initiator.start(0x0BAD) {
        scene.send_from_initiator(&packet[1..]);
    }
    scene.started = true;
    assert!(
        scene.run_until(10, |s| matches!(s.initiator.state(), CsState::Failed(_))),
        "still {:?} after ten ticks",
        scene.initiator.state()
    );
    // 0x02 is Unknown Connection Identifier.
    assert_eq!(scene.initiator.state(), CsState::Failed(0x02));
    assert!(scene.initiator.estimate().is_none());
}

/// A configuration this controller never created cannot be enabled. The
/// refusal has to be an *event*, not silence: the requesting host blocks on
/// LE CS Procedure Enable Complete, so a controller that says nothing hangs it.
///
/// This asserts the controller's answer directly rather than through a host,
/// because the host state machine can only reach `Enabling` by way of a
/// configuration that does exist.
#[test]
fn enabling_a_configuration_that_was_never_created_is_refused_out_loud() {
    let mut scene = CsScene::new(4.0);
    scene.connect_and_start();
    assert!(scene.run_until(15, |s| s.initiator.is_measuring()));
    let handle = scene.connection_handle.expect("connected");

    // Configuration 5 was never created; configuration 1 was.
    let mut params = handle.to_le_bytes().to_vec();
    params.extend_from_slice(&[5, 0x01]);
    scene.send_from_initiator(&command(opcode::LE_CS_PROCEDURE_ENABLE, &params)[1..]);

    let mut refusal = None;
    for _ in 0..5 {
        scene.link.tick();
        while let Some(packet) = scene.initiator_channel.poll_controller_packet() {
            // LE Meta, LE CS Procedure Enable Complete (0x30), for config 5.
            if packet.len() > 8 && packet[1] == 0x3E && packet[3] == 0x30 && packet[7] == 5 {
                refusal = Some(packet[4]);
            }
        }
    }
    // 0x0C is Command Disallowed.
    assert_eq!(
        refusal,
        Some(0x0C),
        "an unknown configuration must be refused with an event, not with silence"
    );
}

/// Stopping is a real command with a real effect: after LE CS Procedure Enable
/// with enable = 0, *both* controllers stop measuring. A stop that only
/// changed the initiator's mind would leave the reflector publishing forever.
#[test]
fn stopping_the_procedure_stops_both_ends_measuring() {
    let mut scene = CsScene::new(5.0);
    scene.connect_and_start();
    assert!(scene.run_until_measured(30));

    for packet in scene.initiator.stop() {
        scene.send_from_initiator(&packet[1..]);
    }
    assert_eq!(scene.initiator.state(), CsState::Idle);

    // Let the disable reach both controllers, then take a baseline.
    for _ in 0..3 {
        scene.tick();
    }
    let subevents = scene.reflector.subevent_count();
    let published = scene.published.len();
    let counter = scene.initiator.measured_counter();

    for _ in 0..10 {
        scene.tick();
    }
    assert_eq!(
        scene.reflector.subevent_count(),
        subevents,
        "the reflector must stop measuring too"
    );
    assert_eq!(scene.published.len(), published, "and stop publishing");
    assert_eq!(
        scene.initiator.measured_counter(),
        counter,
        "and no new measurement may appear at the initiator"
    );
}

/// A Channel Sounding configuration lives on a connection and dies with it.
/// After a disconnect the procedure is gone at both ends, and a reconnect
/// starts a fresh one rather than inheriting the old — which is what would
/// happen if the controller left the session behind and a later connection
/// reused the handle.
#[test]
fn a_disconnect_ends_the_procedure_and_a_reconnect_starts_a_new_one() {
    let mut scene = CsScene::new(5.0);
    scene.connect_and_start();
    assert!(scene.run_until_measured(30));
    let handle = scene.connection_handle.expect("connected");

    scene.send_from_initiator(
        &command([0x06, 0x04], &[handle as u8, (handle >> 8) as u8, 0x13])[1..],
    );
    for _ in 0..3 {
        scene.tick();
    }
    let subevents = scene.reflector.subevent_count();
    for _ in 0..10 {
        scene.tick();
    }
    assert_eq!(
        scene.reflector.subevent_count(),
        subevents,
        "a procedure cannot outlive its connection"
    );

    // Asking to range on the handle that just went away is now refused, which
    // is how a host finds out rather than waiting.
    scene.initiator.reset();
    for packet in scene.initiator.start(handle) {
        scene.send_from_initiator(&packet[1..]);
    }
    assert!(
        scene.run_until(10, |s| matches!(s.initiator.state(), CsState::Failed(_))),
        "still {:?}",
        scene.initiator.state()
    );
    assert_eq!(scene.initiator.state(), CsState::Failed(0x02));

    // And a fresh connection ranges normally.
    scene.reflector.reset();
    scene.initiator.reset();
    scene.segments.reset();
    scene.connection_handle = None;
    let _ = scene
        .reflector_channel
        .send_command(&[0x0A, 0x20, 0x01, 0x01]);
    scene.send_from_initiator(&create_connection(reflector_address()));
    scene.started = false;
    scene.connect_and_start();
    let new_handle = scene.connection_handle.expect("reconnected");
    assert_ne!(new_handle, handle, "a new connection gets a new handle");
    assert!(
        scene.run_until_measured(30),
        "the second procedure never measured: {:?}",
        scene.initiator.state()
    );
}

/// The peer's half is matched to the local half by ranging counter, and a body
/// from a different procedure is counted as a mismatch rather than summed.
/// Tones from two procedures were measured with different oscillator phases,
/// so their sum cancels nothing and produces a confident number with no
/// relationship to distance.
#[test]
fn a_body_from_another_procedure_is_counted_not_combined() {
    let mut scene = CsScene::new(5.0);
    scene.deliver_ras = false;
    scene.connect_and_start();
    assert!(scene.run_until(30, |s| s.published.len() >= 2));

    let body = scene.published.last().expect("published").clone();
    let real = RangingData::parse(&body).expect("well formed");
    let stale = RangingData {
        ranging_counter: real.ranging_counter.wrapping_sub(1) & 0x0FFF,
        ..real
    };
    assert!(scene.initiator.on_ranging_data(&stale.to_bytes()));
    let (combined, mismatched) = scene.initiator.procedure_counts();
    assert_eq!(combined, 0, "nothing may be combined across procedures");
    assert_eq!(mismatched, 1);
    assert!(scene.initiator.estimate().is_none());
    assert!(scene.initiator.combined_tones().is_empty());
}

/// Ranging Data is always segmented in practice — a procedure's tones are a
/// few hundred octets and a notification carries `MTU - 3`. A dropped segment
/// has to discard the body rather than stitch a hole into it, because the
/// tones either side of the gap belong to channels the reassembly would
/// misattribute.
#[test]
fn a_dropped_segment_discards_the_body_rather_than_producing_a_wrong_one() {
    let mut scene = CsScene::new(5.0);
    scene.deliver_ras = false;
    scene.connect_and_start();
    assert!(scene.run_until(30, |s| !s.published.is_empty()));

    let body = scene.published.first().expect("published");
    let segments = ras::segment(body, NOTIFICATION_CAPACITY);
    assert!(
        segments.len() >= 3,
        "a real procedure's data must span several notifications, got {}",
        segments.len()
    );

    let mut reassembler = ras::Reassembler::new();
    reassembler.push(&segments[0]);
    for segment in &segments[2..] {
        assert!(
            reassembler.push(segment).is_none(),
            "a gap must never produce a body"
        );
    }
    // The next complete stream is still accepted, and still parses.
    let mut delivered = None;
    for segment in &segments {
        delivered = reassembler.push(segment).or(delivered);
    }
    let rebuilt = delivered.expect("the next complete body");
    assert_eq!(&rebuilt, body);
    let tones: Vec<Tone> = RangingData::parse(&rebuilt).expect("parses").tones;
    assert_eq!(tones.len(), 19);
}

/// A reflector publishes only for the configuration it was actually placed in.
/// This is the role check, run against the controller that decides the roles
/// rather than against a hand-written Config Complete: the initiator's own
/// device must never start behaving like a reflector and publishing data
/// nobody asked for.
#[test]
fn only_the_reflector_publishes_its_half() {
    let mut scene = CsScene::new(5.0);
    // A second reflector object, fed the *initiator's* event stream.
    let mut initiator_side_reflector = CsReflector::new();
    scene.connect_and_start();

    for _ in 0..20 {
        while let Some(packet) = scene.initiator_channel.poll_controller_packet() {
            initiator_side_reflector.on_packet(&packet);
            for reply in scene.initiator.on_packet(&packet) {
                let _ = scene.initiator_channel.send_command(&reply[1..]);
            }
        }
        while let Some(packet) = scene.reflector_channel.poll_controller_packet() {
            scene.reflector.on_packet(&packet);
        }
        while let Some(body) = scene.reflector.take_ranging_data() {
            scene.published.push(body);
        }
        scene.link.tick();
    }

    assert!(scene.reflector.is_configured(), "the peer is the reflector");
    assert!(
        !initiator_side_reflector.is_configured(),
        "the initiator's device was told role {} and must not publish",
        cs_role::INITIATOR
    );
    assert!(initiator_side_reflector.take_ranging_data().is_none());
    assert!(
        !scene.published.is_empty(),
        "the real reflector did publish"
    );
}
