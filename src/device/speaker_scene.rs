// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! A phone and a Bluetooth speaker on one simulated BR/EDR link.
//!
//! This is the scene that makes [`crate::device::a2dp`] reachable: two
//! [`ClassicHost`](crate::device::ClassicHost)s on a shared
//! [`Link`](crate::controller::sim::Link), one running an [`A2dpSource`] and
//! one an [`A2dpSink`], with nothing wired directly together. Everything
//! between them — inquiry, the Remote Name Request, the page, both L2CAP
//! channel handshakes, every AVDTP signalling message and every RTP media
//! packet — crosses the simulated controller in [`crate::controller::sim`].
//! Turn off the speaker's inquiry scan and the phone never finds it.
//!
//! The transport half lives in [`ProfileScene`]; this file is only the
//! speaker: who the two devices are, what the speaker publishes in SDP, and
//! the accessors that name the A2DP roles rather than "the initiator" and
//! "the acceptor".
//!
//! ## What is not here
//!
//! No pairing and no encryption — a real speaker bonds. No SDP search: the
//! phone opens 0x0019 without asking whether the peer has an Audio Sink
//! record, which a real phone would (and which this scene's own speaker
//! *does* publish, for a peer that looks). No AVRCP, so there are no
//! transport keys. No real time: the source encodes when it is polled.

use crate::classic::a2dp::make_audio_sink_service_sdp_records;
use crate::classic::sdp::{SdpServer, Service};
use crate::device::a2dp::{A2dpSink, A2dpSource, SourcePhase};
use crate::device::profile_scene::{DeviceSpec, LinkPhase, ProfileScene};
use crate::device::{ClassicHost, SdpHandler};
use crate::types::Address;

/// The speaker's BD_ADDR.
pub const SPEAKER_ADDRESS: Address = Address::new([0x5B, 0x33, 0x00, 0xCC, 0xBB, 0xAA]);
/// The phone's BD_ADDR.
pub const PHONE_ADDRESS: Address = Address::new([0x0A, 0x11, 0x00, 0xCC, 0xBB, 0xAA]);

/// The speaker's Class of Device, 0x240414: major class Audio/Video, minor
/// class Loudspeaker, Audio + Rendering service bits. This is the number a
/// phone's pairing list turns into a speaker icon.
const SPEAKER_CLASS_OF_DEVICE: [u8; 3] = [0x14, 0x04, 0x24];
/// The phone's Class of Device, 0x5A020C: Phone / Smartphone.
const PHONE_CLASS_OF_DEVICE: [u8; 3] = [0x0C, 0x02, 0x5A];

/// SDP record handle for the speaker's Audio Sink record.
const SINK_SERVICE_RECORD_HANDLE: u32 = 0x0001_000B;

/// A phone and a speaker on one simulated BR/EDR link, streaming SBC.
pub struct SpeakerScene {
    scene: ProfileScene,
}

impl Default for SpeakerScene {
    fn default() -> Self {
        Self::new()
    }
}

impl SpeakerScene {
    /// Builds the scene: a discoverable, connectable speaker publishing an
    /// Audio Sink SDP record and serving AVDTP, and a phone that will find
    /// it and stream to it.
    pub fn new() -> Self {
        Self::with_sink(A2dpSink::new())
    }

    /// As [`Self::new`], but with a speaker built by the caller — the way to
    /// put a *fussy* sink in the scene, whose capabilities the phone's
    /// configuration cannot satisfy.
    pub fn with_sink(sink: A2dpSink) -> Self {
        Self::build(
            sink,
            crate::device::classic_host::scan_enable::INQUIRY_AND_PAGE,
        )
    }

    /// As [`Self::with_sink`], with the speaker's Scan Enable chosen — the
    /// way to build a speaker that is deliberately not findable, which is
    /// the only thing that proves the inquiry is real.
    pub fn with_speaker_scan_enable(sink: A2dpSink, scan_enable: u8) -> Self {
        Self::build(sink, scan_enable)
    }

    fn build(sink: A2dpSink, scan_enable: u8) -> Self {
        let mut sdp = SdpHandler::new(SdpServer::new());
        sdp.server_mut().service_records.insert(
            SINK_SERVICE_RECORD_HANDLE,
            sink_service_record(SINK_SERVICE_RECORD_HANDLE),
        );
        Self {
            scene: ProfileScene::new(
                DeviceSpec::initiator(
                    "Simble Phone",
                    PHONE_CLASS_OF_DEVICE,
                    PHONE_ADDRESS,
                    vec![Box::new(A2dpSource::new())],
                ),
                DeviceSpec::acceptor(
                    "Simble Speaker",
                    SPEAKER_CLASS_OF_DEVICE,
                    SPEAKER_ADDRESS,
                    vec![Box::new(sdp), Box::new(sink)],
                )
                .with_scan_enable(scan_enable),
            ),
        }
    }

    /// How far the transport has got.
    pub fn phase(&self) -> LinkPhase {
        self.scene.phase()
    }

    /// Why the plan stopped, if it did.
    pub fn error(&self) -> Option<&str> {
        self.scene.error()
    }

    /// The phone's A2DP source.
    pub fn source(&self) -> &A2dpSource {
        self.scene.initiator::<A2dpSource>()
    }

    /// The phone's A2DP source, mutably — for queueing PCM.
    pub fn source_mut(&mut self) -> &mut A2dpSource {
        self.scene.initiator_mut::<A2dpSource>()
    }

    /// The speaker's A2DP sink.
    pub fn sink(&self) -> &A2dpSink {
        self.scene.acceptor::<A2dpSink>()
    }

    /// The speaker's A2DP sink, mutably — for draining received frames.
    pub fn sink_mut(&mut self) -> &mut A2dpSink {
        self.scene.acceptor_mut::<A2dpSink>()
    }

    /// The phone's BR/EDR host, for assertions the plan does not cover.
    pub fn phone_host(&self) -> &ClassicHost {
        self.scene.initiator_host()
    }

    /// The speaker's BR/EDR host.
    pub fn speaker_host(&self) -> &ClassicHost {
        self.scene.acceptor_host()
    }

    /// Queues interleaved stereo PCM for the phone to encode and stream.
    pub fn play(&mut self, samples: &[i16]) {
        self.source_mut().queue_pcm(samples);
    }

    /// Advances the scene one step.
    pub fn tick(&mut self) {
        self.scene.tick();
    }

    /// Runs until `done` is true or `steps` have passed.
    pub fn run_until(&mut self, steps: usize, mut done: impl FnMut(&Self) -> bool) -> bool {
        for _ in 0..steps {
            if done(self) {
                return true;
            }
            self.tick();
        }
        done(self)
    }

    /// Runs until the stream is STREAMING with its transport channel
    /// attached, or gives up after `steps`.
    pub fn run_until_streaming(&mut self, steps: usize) -> bool {
        self.run_until(steps, |scene| {
            scene.source().phase() == SourcePhase::Streaming && scene.sink().has_media_channel()
        })
    }
}

/// The speaker's Audio Sink SDP record: what a phone reads to learn this
/// device renders A2DP audio, and on which PSM.
fn sink_service_record(handle: u32) -> Service {
    make_audio_sink_service_sdp_records(handle, None)
}
