// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Two scenes that make [`crate::device::avrcp`] reachable.
//!
//! [`RemoteControlScene`] is AVRCP on its own: a head unit that pages a
//! phone, opens one AVCTP channel and drives its media player. It is the
//! smallest thing that proves the profile can be hosted at all.
//!
//! [`MediaPlayerScene`] is the honest one. A speaker that only takes audio is
//! half a speaker; the interesting claim is that **a PAUSE stops the music**,
//! and nothing smaller than A2DP plus AVRCP on one link can make it. So this
//! scene runs both: an AVDTP stream carrying SBC on PSM 0x0019 and an AVCTP
//! control channel on 0x0017, between the same two devices, over the same
//! simulated controller.
//!
//! ## Who is the controller
//!
//! The **speaker** is. That is not a quirk of this file — it is where the
//! buttons are. A phone streaming A2DP is the end that owns the media player,
//! so it is the AVRCP *target*; the car head unit or speaker that has a
//! play/pause key is the AVRCP *controller*, which is exactly the arrangement
//! Android's `BluetoothAvrcpController` proxy exists for (Android as the sink
//! end of a car kit). Getting this backwards is easy and produces a scene
//! where PAUSE has nowhere to land.
//!
//! ## What "the music stops" actually means here
//!
//! The speaker taps PAUSE. The phone's [`AvrcpTarget`] receives the AV/C
//! PASS THROUGH, answers ACCEPTED, and moves its playback status to PAUSED —
//! that much is protocol. [`MediaPlayerScene::play`] then declines to hand
//! any more PCM to the A2DP source, because a paused media player does not
//! produce audio. The observable consequence is on the far side: the
//! speaker's [`A2dpSink`] stops receiving frames.
//!
//! A real phone would *also* send AVDTP_SUSPEND, releasing the stream rather
//! than merely starving it. [`A2dpSource`] does not expose a suspend, so this
//! scene does not do that half, and the AVDTP stream stays STREAMING with
//! nothing flowing through it. Said plainly rather than implied: the audio
//! stopping is real and observable at the sink, but the *mechanism* is one
//! layer shallower than a production stack's.

use crate::classic::a2dp::make_audio_sink_service_sdp_records;
use crate::classic::avrcp::{
    ControllerServiceRecord, TargetServiceRecord, controller_features, play_status, target_features,
};
use crate::classic::sdp::{SdpServer, Service};
use crate::device::a2dp::{A2dpSink, A2dpSource, SourcePhase};
use crate::device::avrcp::{AvrcpController, AvrcpTarget, Track};
use crate::device::profile_scene::{DeviceSpec, LinkPhase, ProfileScene};
use crate::device::{ClassicHost, SdpHandler};
use crate::types::Address;

/// The phone's BD_ADDR in both scenes — the end with the music.
pub const PLAYER_ADDRESS: Address = Address::new([0x11, 0xAC, 0x00, 0xCC, 0xBB, 0xAA]);
/// The head unit's BD_ADDR in both scenes — the end with the buttons.
pub const REMOTE_ADDRESS: Address = Address::new([0x22, 0xAC, 0x00, 0xCC, 0xBB, 0xAA]);

/// Class of Device 0x5A020C: Phone / Smartphone.
const PHONE_CLASS_OF_DEVICE: [u8; 3] = [0x0C, 0x02, 0x5A];
/// Class of Device 0x200420: Audio/Video major class, Hands-free minor class
/// — a car head unit, which is what an AVRCP controller usually is.
const HEAD_UNIT_CLASS_OF_DEVICE: [u8; 3] = [0x20, 0x04, 0x20];
/// Class of Device 0x240414: Audio/Video, Loudspeaker.
const SPEAKER_CLASS_OF_DEVICE: [u8; 3] = [0x14, 0x04, 0x24];

/// SDP record handle for the phone's AVRCP Target record.
const TARGET_SERVICE_RECORD_HANDLE: u32 = 0x0001_000E;
/// SDP record handle for the head unit's AVRCP Controller record.
const CONTROLLER_SERVICE_RECORD_HANDLE: u32 = 0x0001_000F;
/// SDP record handle for the speaker's Audio Sink record.
const SINK_SERVICE_RECORD_HANDLE: u32 = 0x0001_000B;

/// The two tracks the scenes' phones are playing. Deliberately not "Track 1"
/// and "Track 2": a test that reads a title back has to be reading something
/// the *other* device sent, and identical placeholder strings on both sides
/// would hide a wire bug that swapped them.
fn default_playlist() -> Vec<Track> {
    vec![
        Track::new(
            "Careful With That Axe",
            "Simble Ensemble",
            "Unreachable Profiles",
            213_000,
        ),
        Track::new(
            "Continuation State",
            "The Fragmented",
            "Unreachable Profiles",
            187_000,
        ),
    ]
}

/// The phone's AVRCP Target SDP record. Category 1 is "player/recorder" —
/// the bit that says this device answers PLAY, PAUSE and the rest — and
/// category 2 is "monitor/amplifier", which is what carries volume.
fn target_service_record() -> Service {
    TargetServiceRecord::new(
        TARGET_SERVICE_RECORD_HANDLE,
        target_features::CATEGORY_1 | target_features::CATEGORY_2,
    )
    .to_service_attributes()
}

/// The head unit's AVRCP Controller SDP record.
fn controller_service_record() -> Service {
    ControllerServiceRecord::new(
        CONTROLLER_SERVICE_RECORD_HANDLE,
        controller_features::CATEGORY_1 | controller_features::CATEGORY_2,
    )
    .to_service_attributes()
}

// ---------------------------------------------------------------------------
// AVRCP alone
// ---------------------------------------------------------------------------

/// A head unit and a phone on one simulated BR/EDR link, with one AVCTP
/// control channel between them.
///
/// The head unit is the initiator: it inquires, resolves the name, pages, and
/// then opens PSM 0x0017 itself, which is what a controller does.
pub struct RemoteControlScene {
    scene: ProfileScene,
}

impl Default for RemoteControlScene {
    fn default() -> Self {
        Self::new()
    }
}

impl RemoteControlScene {
    /// A head unit and a phone with a two-track playlist loaded, stopped.
    pub fn new() -> Self {
        Self::with_player(default_playlist())
    }

    /// As [`Self::new`] with a playlist of the caller's choosing — including
    /// tracks whose metadata is too big for one AV/C frame, which is the only
    /// way to reach the continuation path from a scene.
    pub fn with_player(playlist: Vec<Track>) -> Self {
        Self::build(
            playlist,
            crate::device::classic_host::scan_enable::INQUIRY_AND_PAGE,
        )
    }

    /// As [`Self::with_player`], with the phone's Scan Enable chosen.
    ///
    /// Setting it to [`scan_enable::NONE`](crate::device::classic_host::scan_enable::NONE)
    /// is the switch that proves the transport is real: the head unit's
    /// inquiry finds nothing and the plan fails in
    /// [`LinkPhase::Inquiring`], rather than the profile connecting anyway
    /// because the two devices were wired together underneath.
    pub fn with_phone_scan_enable(playlist: Vec<Track>, scan_enable: u8) -> Self {
        Self::build(playlist, scan_enable)
    }

    fn build(playlist: Vec<Track>, scan_enable: u8) -> Self {
        let mut target = AvrcpTarget::new();
        target.set_playlist(playlist);

        let mut sdp = SdpHandler::new(SdpServer::new());
        sdp.server_mut()
            .service_records
            .insert(TARGET_SERVICE_RECORD_HANDLE, target_service_record());

        let mut head_unit_sdp = SdpHandler::new(SdpServer::new());
        head_unit_sdp.server_mut().service_records.insert(
            CONTROLLER_SERVICE_RECORD_HANDLE,
            controller_service_record(),
        );

        Self {
            scene: ProfileScene::new(
                DeviceSpec::initiator(
                    "Simble Head Unit",
                    HEAD_UNIT_CLASS_OF_DEVICE,
                    REMOTE_ADDRESS,
                    vec![Box::new(head_unit_sdp), Box::new(AvrcpController::new())],
                ),
                DeviceSpec::acceptor(
                    "Simble Phone",
                    PHONE_CLASS_OF_DEVICE,
                    PLAYER_ADDRESS,
                    vec![Box::new(sdp), Box::new(target)],
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

    /// The head unit's AVRCP controller.
    pub fn controller(&self) -> &AvrcpController {
        self.scene.initiator::<AvrcpController>()
    }

    /// The head unit's AVRCP controller, mutably — for pressing keys.
    pub fn controller_mut(&mut self) -> &mut AvrcpController {
        self.scene.initiator_mut::<AvrcpController>()
    }

    /// The phone's AVRCP target.
    pub fn target(&self) -> &AvrcpTarget {
        self.scene.acceptor::<AvrcpTarget>()
    }

    /// The phone's AVRCP target, mutably — for changing tracks from the
    /// phone's own screen.
    pub fn target_mut(&mut self) -> &mut AvrcpTarget {
        self.scene.acceptor_mut::<AvrcpTarget>()
    }

    /// The head unit's BR/EDR host.
    pub fn remote_host(&self) -> &ClassicHost {
        self.scene.initiator_host()
    }

    /// The phone's BR/EDR host.
    pub fn player_host(&self) -> &ClassicHost {
        self.scene.acceptor_host()
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

    /// Runs until the AVCTP control channel is up at both ends.
    pub fn run_until_connected(&mut self, steps: usize) -> bool {
        self.run_until(steps, |scene| {
            scene.controller().is_connected() && scene.target().is_connected()
        })
    }
}

// ---------------------------------------------------------------------------
// A2DP and AVRCP together
// ---------------------------------------------------------------------------

/// A phone streaming SBC to a speaker that can also tell it to stop.
///
/// The phone is the initiator: it pages the speaker, opens AVDTP signalling,
/// runs the A2DP source sequence to STREAMING, and opens the AVCTP control
/// channel for the speaker's buttons to arrive on.
pub struct MediaPlayerScene {
    scene: ProfileScene,
}

impl Default for MediaPlayerScene {
    fn default() -> Self {
        Self::new()
    }
}

impl MediaPlayerScene {
    /// A phone with a two-track playlist loaded and playing, and a speaker
    /// that renders SBC and has a play/pause key.
    pub fn new() -> Self {
        Self::with_player(default_playlist())
    }

    /// As [`Self::new`] with a playlist of the caller's choosing.
    pub fn with_player(playlist: Vec<Track>) -> Self {
        // The phone opens the control channel: it is the one that just
        // established A2DP, and AVRCP 4.2 lets either role establish AVCTP.
        // The alternative — the speaker opening it — works too, and is what
        // `RemoteControlScene` does.
        let mut target = AvrcpTarget::connecting();
        target.set_playlist(playlist);
        // The user pressed play on the phone before getting in the car.
        target.set_playback_status(play_status::PLAYING);

        let mut phone_sdp = SdpHandler::new(SdpServer::new());
        phone_sdp
            .server_mut()
            .service_records
            .insert(TARGET_SERVICE_RECORD_HANDLE, target_service_record());

        let mut speaker_sdp = SdpHandler::new(SdpServer::new());
        speaker_sdp.server_mut().service_records.insert(
            SINK_SERVICE_RECORD_HANDLE,
            make_audio_sink_service_sdp_records(SINK_SERVICE_RECORD_HANDLE, None),
        );
        speaker_sdp.server_mut().service_records.insert(
            CONTROLLER_SERVICE_RECORD_HANDLE,
            controller_service_record(),
        );

        Self {
            scene: ProfileScene::new(
                DeviceSpec::initiator(
                    "Simble Phone",
                    PHONE_CLASS_OF_DEVICE,
                    PLAYER_ADDRESS,
                    vec![
                        Box::new(phone_sdp),
                        Box::new(A2dpSource::new()),
                        Box::new(target),
                    ],
                ),
                DeviceSpec::acceptor(
                    "Simble Speaker",
                    SPEAKER_CLASS_OF_DEVICE,
                    REMOTE_ADDRESS,
                    vec![
                        Box::new(speaker_sdp),
                        Box::new(A2dpSink::new()),
                        Box::new(AvrcpController::listening()),
                    ],
                ),
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

    /// The speaker's A2DP sink.
    pub fn sink(&self) -> &A2dpSink {
        self.scene.acceptor::<A2dpSink>()
    }

    /// The speaker's A2DP sink, mutably — for draining received frames.
    pub fn sink_mut(&mut self) -> &mut A2dpSink {
        self.scene.acceptor_mut::<A2dpSink>()
    }

    /// The phone's AVRCP target — its media player.
    pub fn player(&self) -> &AvrcpTarget {
        self.scene.initiator::<AvrcpTarget>()
    }

    /// The phone's AVRCP target, mutably.
    pub fn player_mut(&mut self) -> &mut AvrcpTarget {
        self.scene.initiator_mut::<AvrcpTarget>()
    }

    /// The speaker's AVRCP controller — its buttons.
    pub fn remote(&self) -> &AvrcpController {
        self.scene.acceptor::<AvrcpController>()
    }

    /// The speaker's AVRCP controller, mutably — for pressing them.
    pub fn remote_mut(&mut self) -> &mut AvrcpController {
        self.scene.acceptor_mut::<AvrcpController>()
    }

    /// The phone's BR/EDR host.
    pub fn phone_host(&self) -> &ClassicHost {
        self.scene.initiator_host()
    }

    /// The speaker's BR/EDR host.
    pub fn speaker_host(&self) -> &ClassicHost {
        self.scene.acceptor_host()
    }

    /// Hands the phone's media player some interleaved stereo PCM to stream —
    /// **if it is playing**.
    ///
    /// A paused player produces no audio, so this is where the AVRCP PAUSE
    /// the speaker sent becomes silence at the speaker. Returns whether the
    /// samples were taken, so a caller can tell "the player is paused" from
    /// "the samples were queued and something else swallowed them".
    pub fn play(&mut self, samples: &[i16]) -> bool {
        if !self.player().is_playing() {
            return false;
        }
        self.scene.initiator_mut::<A2dpSource>().queue_pcm(samples);
        true
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

    /// Runs until audio is flowing *and* the remote control is connected —
    /// the state this scene exists to reach, and the one both halves of a
    /// transport-control test need.
    pub fn run_until_ready(&mut self, steps: usize) -> bool {
        self.run_until(steps, |scene| {
            scene.source().phase() == SourcePhase::Streaming
                && scene.sink().has_media_channel()
                && scene.remote().is_connected()
                && scene.player().is_connected()
        })
    }
}
