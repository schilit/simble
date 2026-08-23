// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! An Auracast broadcast source and its receivers, in one process, talking to
//! each other through the simulated controller.
//!
//! # What these tests can and cannot prove
//!
//! Both ends here are this codebase, and so is the controller between them, so
//! nothing below is evidence that Simble's broadcast is on the air correctly.
//! A wire format that is wrong in a self-consistent way passes every one of
//! these tests. `tests/interop/auracast_source.py` and `auracast_sink.py` are
//! the checks that involve a foreign stack, and `test_base_matches_bumble` in
//! `big_broadcaster.rs` pins the announcement bytes against Bumble's own.
//!
//! What these tests are for is the other half: **sequence, state and
//! teardown**, deterministically and with no external process. A broadcast has
//! no back-channel, so every one of its failure modes is a state machine on
//! one side reaching a state the other side cannot see, and those are exactly
//! the bugs a scripted interop run — which only ever joins and streams — walks
//! straight past. The one already found by hand is here as
//! [`a_receiver_that_leaves_the_big_stops_reporting_that_it_is_receiving`]:
//! `LE BIG Terminate Sync` is answered by Command Complete and by nothing
//! else, and a receiver that did not act on it reported `Receiving` forever.
//!
//! One coupling here *is* load-bearing. The BIGInfo a receiver acts on is
//! built by the controller out of the source's own `LE Create BIG` parameters
//! — the BIS count and the encryption flag are read back out of what the
//! broadcaster asked for, never invented. So a broadcaster that fills that
//! command in wrongly is contradicted by its own receiver here, where the unit
//! tests on either side hand-feed a BIGInfo that agrees with them by
//! construction.

use simble::controller::sim::Link;
use simble::device::big_broadcaster::{BigBroadcaster, BroadcastConfig, BroadcastState};
use simble::device::big_receiver::{BigReceiver, ReceiverConfig, ReceiverState};
use simble::packets::big::big_encryption;
use simble::transport::HciChannel;
use simble::types::Address;
use std::sync::Arc;

/// A source and any number of sinks on one simulated medium.
struct BroadcastScene {
    link: Link,
    source_channel: Arc<HciChannel>,
    source: BigBroadcaster,
    sinks: Vec<Sink>,
}

/// One receiver and the channel it talks to its controller over.
struct Sink {
    channel: Arc<HciChannel>,
    receiver: BigReceiver,
}

impl BroadcastScene {
    /// Puts a broadcaster on the medium and starts its setup sequence.
    fn new(config: BroadcastConfig) -> Self {
        let mut link = Link::new();
        let source_channel = link.add_device(address(0x01));
        let mut source = BigBroadcaster::new(config);
        for packet in source.start() {
            source_channel.inject_host_packet(packet).expect("queued");
        }
        Self {
            link,
            source_channel,
            source,
            sinks: Vec::new(),
        }
    }

    /// Adds a receiver and starts it scanning. Returns its index.
    fn add_sink(&mut self, config: ReceiverConfig) -> usize {
        let channel = self.link.add_device(address(0x10 + self.sinks.len() as u8));
        let mut receiver = BigReceiver::new(config);
        for packet in receiver.start() {
            channel.inject_host_packet(packet).expect("queued");
        }
        self.sinks.push(Sink { channel, receiver });
        self.sinks.len() - 1
    }

    /// Advances the whole scene: every host consumes what its controller
    /// produced and answers, then the medium routes.
    fn tick(&mut self) {
        while let Some(packet) = self.source_channel.poll_controller_packet() {
            for reply in self.source.on_packet(&packet) {
                self.source_channel
                    .inject_host_packet(reply)
                    .expect("queued");
            }
        }
        for sink in &mut self.sinks {
            while let Some(packet) = sink.channel.poll_controller_packet() {
                for reply in sink.receiver.on_packet(&packet) {
                    sink.channel.inject_host_packet(reply).expect("queued");
                }
            }
        }
        self.link.tick();
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

    /// Ticks until the source is streaming and every sink is receiving.
    fn run_until_streaming(&mut self, ticks: usize) -> bool {
        self.run_until(ticks, |scene| {
            scene.source.is_streaming() && scene.sinks.iter().all(|s| s.receiver.is_receiving())
        })
    }

    /// Writes one SDU on each BIS and lets it cross the medium.
    fn stream_one_sdu_per_bis(&mut self, payload: &[u8]) {
        let num_bis = self.source.config().num_bis;
        for index in 1..=num_bis {
            let packet = self
                .source
                .send_sdu(index, payload)
                .expect("the source is streaming");
            self.source_channel
                .inject_host_packet(packet)
                .expect("queued");
        }
        self.tick();
        self.tick();
    }

    fn receiver(&self, index: usize) -> &BigReceiver {
        &self.sinks[index].receiver
    }

    /// Sends one host command for a sink, as its own host would.
    fn send_from_sink(&mut self, index: usize, packet: Vec<u8>) {
        self.sinks[index]
            .channel
            .inject_host_packet(packet)
            .expect("queued");
    }
}

/// A distinct public address per device in the scene.
fn address(last: u8) -> Address {
    format!("AA:BB:CC:00:00:{last:02X}")
        .parse()
        .expect("a valid address")
}

/// A source configuration a receiver can be pointed at by Broadcast_ID.
fn source_config() -> BroadcastConfig {
    BroadcastConfig {
        broadcast_id: 0x00AB_CDEF,
        broadcast_name: "Simble E2E".to_string(),
        ..Default::default()
    }
}

#[test]
fn the_establishment_ladder_runs_from_both_ends() {
    let mut scene = BroadcastScene::new(source_config());
    scene.add_sink(ReceiverConfig::default());
    assert!(scene.run_until_streaming(40), "never reached streaming");

    assert_eq!(scene.source.state(), BroadcastState::Streaming);
    assert_eq!(scene.receiver(0).state(), ReceiverState::Receiving);

    // Neither side's handles were invented by the test: the source's came from
    // LE Create BIG Complete, the receiver's from LE BIG Sync Established, and
    // the controller issued both. They must be distinct — a BIS handle names a
    // stream inside one controller, and two devices do not share a namespace.
    let source_handles = scene.source.bis_handles().to_vec();
    let sink_handles = scene.receiver(0).bis_handles().to_vec();
    assert_eq!(source_handles.len(), 2);
    assert_eq!(sink_handles.len(), 2);
    assert!(
        source_handles.iter().all(|h| !sink_handles.contains(h)),
        "source {source_handles:?} and sink {sink_handles:?} must not share handles"
    );

    // What the receiver found is what the source published, having crossed the
    // extended advertisement and then the periodic train.
    let found = scene.receiver(0).found().expect("a source was found");
    assert_eq!(found.broadcast_id, 0x00AB_CDEF);
    assert_eq!(found.address, {
        let mut bytes = address(0x01).to_be_bytes();
        bytes.reverse();
        bytes
    });
    assert_eq!(
        scene.receiver(0).base_bytes(),
        Some(&scene.source.config().periodic_advertising_data()[4..]),
        "the BASE arrived as the source wrote it"
    );
}

/// The BIGInfo a receiver decides on is built from the source's own
/// `LE Create BIG`, so this is the one assertion here that could fail if the
/// broadcaster filled that command in wrongly rather than merely inconsistently.
#[test]
fn the_biginfo_a_receiver_reads_comes_from_the_sources_create_big() {
    let config = BroadcastConfig {
        num_bis: 1,
        sdu_interval_us: 7_500,
        max_sdu: 60,
        ..source_config()
    };
    let mut scene = BroadcastScene::new(config);
    scene.add_sink(ReceiverConfig::default());
    assert!(scene.run_until_streaming(40));

    let big_info = scene.receiver(0).big_info().expect("BIGInfo arrived");
    assert_eq!(big_info.num_bis, 1, "the source created one BIS");
    assert_eq!(big_info.sdu_interval.get(), 7_500);
    assert_eq!(big_info.max_sdu.get(), 60);
    assert_eq!(big_info.encryption, big_encryption::UNENCRYPTED);
    assert_eq!(scene.source.bis_handles().len(), 1);
    assert_eq!(scene.receiver(0).bis_handles().len(), 1);
}

#[test]
fn audio_written_by_the_source_arrives_on_the_receivers_own_handles() {
    let mut scene = BroadcastScene::new(source_config());
    scene.add_sink(ReceiverConfig::default());
    assert!(scene.run_until_streaming(40));

    scene.stream_one_sdu_per_bis(&[0xA5; 100]);
    let sink_handles = scene.receiver(0).bis_handles().to_vec();
    assert_eq!(scene.receiver(0).sdu_count(), 2, "one SDU per BIS");

    let first = scene.sinks[0].receiver.poll_sdu().expect("left channel");
    let second = scene.sinks[0].receiver.poll_sdu().expect("right channel");
    assert_eq!(first.handle, sink_handles[0]);
    assert_eq!(second.handle, sink_handles[1]);
    assert_eq!(first.payload, vec![0xA5; 100]);
}

/// A broadcast has no back-channel: the source cannot tell one listener from
/// two, or from none. That is the property the whole architecture rests on,
/// and it is only visible with more than one receiver in the scene.
#[test]
fn one_source_reaches_every_receiver_and_hears_from_none_of_them() {
    let mut scene = BroadcastScene::new(source_config());
    for _ in 0..3 {
        scene.add_sink(ReceiverConfig {
            broadcast_id: Some(0x00AB_CDEF),
            ..Default::default()
        });
    }
    assert!(scene.run_until_streaming(60), "not all three joined");

    let before = scene.source.state();
    scene.stream_one_sdu_per_bis(&[0x11; 100]);

    for index in 0..3 {
        assert_eq!(
            scene.receiver(index).sdu_count(),
            2,
            "receiver {index} missed audio"
        );
    }
    assert_eq!(
        scene.source.state(),
        before,
        "nothing a receiver did may reach the source's state machine"
    );

    // Each receiver was given its own handles; a source's SDU is delivered on
    // whichever handle *that* receiver was told to expect.
    let handle_sets: Vec<Vec<u16>> = (0..3)
        .map(|index| scene.receiver(index).bis_handles().to_vec())
        .collect();
    for (a, first) in handle_sets.iter().enumerate() {
        for (b, second) in handle_sets.iter().enumerate() {
            if a != b {
                assert!(
                    first.iter().all(|h| !second.contains(h)),
                    "receivers {a} and {b} share a BIS handle"
                );
            }
        }
    }
}

/// The bug this whole file exists for. `LE BIG Terminate Sync` is answered by
/// Command Complete and by nothing else — no BIG Sync Lost follows a *local*
/// termination, because nothing was lost. A receiver that waits for one waits
/// forever, and reports `Receiving` while its controller has already dropped
/// the streams.
#[test]
fn a_receiver_that_leaves_the_big_stops_reporting_that_it_is_receiving() {
    let mut scene = BroadcastScene::new(source_config());
    scene.add_sink(ReceiverConfig::default());
    scene.add_sink(ReceiverConfig::default());
    assert!(scene.run_until_streaming(60));

    let terminate = scene.receiver(0).terminate();
    scene.send_from_sink(0, terminate);
    assert!(
        scene.run_until(10, |scene| {
            scene.receiver(0).state() == ReceiverState::Terminated
        }),
        "left the BIG but never said so: {:?}",
        scene.receiver(0).state()
    );
    assert!(!scene.receiver(0).is_receiving());
    assert!(scene.receiver(0).bis_handles().is_empty());

    // And leaving is a local act: the source keeps streaming to whoever is
    // still there, and the receiver that left gets nothing further.
    scene.stream_one_sdu_per_bis(&[0x22; 100]);
    assert_eq!(scene.receiver(0).sdu_count(), 0, "it left the BIG");
    assert_eq!(scene.receiver(1).sdu_count(), 2, "the other one did not");
    assert_eq!(scene.source.state(), BroadcastState::Streaming);
}

/// The other direction of teardown, and the one a receiver cannot anticipate:
/// the source goes away, and every synchronized receiver is told out of
/// nowhere, by its own controller, with Remote User Terminated Connection.
#[test]
fn the_source_tearing_down_reaches_every_receiver_as_big_sync_lost() {
    let mut scene = BroadcastScene::new(source_config());
    scene.add_sink(ReceiverConfig::default());
    scene.add_sink(ReceiverConfig::default());
    assert!(scene.run_until_streaming(60));

    let terminate = scene.source.terminate();
    scene
        .source_channel
        .inject_host_packet(terminate)
        .expect("queued");
    assert!(
        scene.run_until(10, |scene| {
            scene.source.state() == BroadcastState::Terminated
        }),
        "the source never confirmed its own teardown"
    );
    assert!(
        scene.source.bis_handles().is_empty(),
        "the handles are gone with the BIG"
    );

    for index in 0..2 {
        // 0x13 is Remote User Terminated Connection: the stream ended because
        // the far end chose to end it, which is the only thing a receiver ever
        // learns about why.
        assert_eq!(
            scene.receiver(index).state(),
            ReceiverState::Lost(0x13),
            "receiver {index}"
        );
        assert!(scene.receiver(index).bis_handles().is_empty());
    }
}

/// An encrypted source announces itself as encrypted in the BIGInfo, and a
/// receiver with no broadcast code refuses rather than joining a BIG it could
/// only produce noise from. The refusal is local — no command goes out — so
/// the source never learns anyone tried.
#[test]
fn a_receiver_with_no_broadcast_code_refuses_an_encrypted_source() {
    let mut scene = BroadcastScene::new(BroadcastConfig {
        broadcast_code: Some([0x42; 16]),
        ..source_config()
    });
    scene.add_sink(ReceiverConfig::default());
    assert!(
        scene.run_until(40, |scene| {
            scene.receiver(0).state() == ReceiverState::Failed(0x1D)
        }),
        "expected Insufficient Security, got {:?}",
        scene.receiver(0).state()
    );

    let big_info = scene.receiver(0).big_info().expect("BIGInfo arrived");
    assert_eq!(
        big_info.encryption,
        big_encryption::ENCRYPTED,
        "the encryption flag must come from the source's LE Create BIG"
    );
    assert!(scene.receiver(0).bis_handles().is_empty());

    // The source goes on to stream to nobody, and cannot tell. The refusal
    // never left the receiver's host, so there was nothing for it to hear.
    assert!(
        scene.run_until(20, |scene| scene.source.is_streaming()),
        "the source's own setup must not depend on anyone joining"
    );
    assert_eq!(scene.receiver(0).state(), ReceiverState::Failed(0x1D));
}

/// The same source, with the code, joins normally — so the refusal above is
/// about the missing code and not about encryption being unimplemented.
#[test]
fn a_receiver_with_the_right_broadcast_code_joins_an_encrypted_source() {
    let mut scene = BroadcastScene::new(BroadcastConfig {
        broadcast_code: Some([0x42; 16]),
        ..source_config()
    });
    scene.add_sink(ReceiverConfig {
        broadcast_code: Some([0x42; 16]),
        ..Default::default()
    });
    assert!(
        scene.run_until_streaming(40),
        "never joined: {:?}",
        scene.receiver(0).state()
    );
    scene.stream_one_sdu_per_bis(&[0x33; 100]);
    assert_eq!(scene.receiver(0).sdu_count(), 2);
}

/// A source's audio is only for the BIG it belongs to. A second, unrelated
/// broadcast in the same scene must not leak into it — the check that ISO
/// routing is by BIS handle and BIG membership, not by "whatever is on air".
#[test]
fn a_receiver_hears_only_the_broadcast_it_joined() {
    let mut scene = BroadcastScene::new(source_config());
    scene.add_sink(ReceiverConfig {
        broadcast_id: Some(0x00AB_CDEF),
        ..Default::default()
    });
    assert!(scene.run_until_streaming(40));

    // A second source, with a different Broadcast_ID, on the same medium.
    let other_channel = scene.link.add_device(address(0x02));
    let mut other = BigBroadcaster::new(BroadcastConfig {
        broadcast_id: 0x0012_3456,
        advertising_sid: 1,
        ..Default::default()
    });
    for packet in other.start() {
        other_channel.inject_host_packet(packet).expect("queued");
    }
    for _ in 0..40 {
        while let Some(packet) = other_channel.poll_controller_packet() {
            for reply in other.on_packet(&packet) {
                other_channel.inject_host_packet(reply).expect("queued");
            }
        }
        scene.tick();
    }
    assert!(other.is_streaming(), "the second source never started");

    let before = scene.receiver(0).sdu_count();
    for index in 1..=other.config().num_bis {
        let packet = other.send_sdu(index, &[0xEE; 100]).expect("streaming");
        other_channel.inject_host_packet(packet).expect("queued");
    }
    scene.tick();
    scene.tick();
    assert_eq!(
        scene.receiver(0).sdu_count(),
        before,
        "audio from a BIG this receiver never joined must not reach it"
    );

    // And the receiver it *did* join still works.
    scene.stream_one_sdu_per_bis(&[0x44; 100]);
    assert_eq!(scene.receiver(0).sdu_count(), before + 2);
}

/// LE Create BIG rides in the ACAD of a periodic train, so there has to be one
/// running. A host that asks too early is refused with Command Status, and no
/// LE Create BIG Complete ever arrives — the failure path the broadcaster's
/// `on_command_status` exists for, exercised against a controller that
/// actually decides rather than a hand-written status byte.
#[test]
fn creating_a_big_before_the_periodic_train_is_refused() {
    let mut link = Link::new();
    let channel = link.add_device(address(0x01));
    let mut broadcaster = BigBroadcaster::new(source_config());

    // Drive the setup sequence, but drop the two enable commands on the floor
    // so the train never starts.
    for packet in broadcaster.start() {
        channel.inject_host_packet(packet).expect("queued");
    }
    for _ in 0..20 {
        while let Some(packet) = channel.poll_controller_packet() {
            for reply in broadcaster.on_packet(&packet) {
                let is_periodic_enable = reply.len() >= 3
                    && u16::from_le_bytes([reply[1], reply[2]])
                        == simble::packets::ext_adv::ext_adv_opcode::LE_SET_PERIODIC_ADVERTISING_ENABLE
                            .get();
                if is_periodic_enable {
                    // Answer it ourselves with success, so the broadcaster
                    // goes on to LE Create BIG believing the train is up.
                    let mut complete = vec![0x04, 0x0E, 0x04, 0x01];
                    complete.extend_from_slice(&reply[1..3]);
                    complete.push(0x00);
                    channel.receive_from_controller(complete).expect("queued");
                    continue;
                }
                channel.inject_host_packet(reply).expect("queued");
            }
        }
        link.tick();
        if matches!(broadcaster.state(), BroadcastState::Failed(_)) {
            break;
        }
    }
    // 0x0C is Command Disallowed.
    assert_eq!(broadcaster.state(), BroadcastState::Failed(0x0C));
    assert!(broadcaster.bis_handles().is_empty());
    assert!(
        broadcaster.send_sdu(1, &[0x00; 100]).is_none(),
        "a failed source must not pretend to send audio"
    );
}
