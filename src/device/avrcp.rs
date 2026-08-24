// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! AVRCP as [`ProtocolHandler`]s — the two ends of a media remote control.
//!
//! [`crate::classic::avrcp`] has been a 3 300-line two-role state machine
//! since before any scene existed, and nothing could host it. Its only
//! consumer was `tests/avrcp_test.rs`, which drives two [`Protocol`]s
//! back-to-back with no L2CAP underneath: the profile was tested but
//! **unreachable** — no scene could put it on a link, and no foreign stack
//! had ever spoken to it. These two types are that seam.
//!
//! ## The two roles
//!
//! [`AvrcpController`] is the end with the buttons — a car head unit, a
//! speaker with a play/pause key, an Android device using
//! `BluetoothAvrcpController`. It sends AV/C PASS THROUGH operations and the
//! metadata queries, and reads back what the player says.
//!
//! [`AvrcpTarget`] is the end with the music: it answers PASS THROUGH, serves
//! `GetPlayStatus` and `GetElementAttributes` out of a real playlist of
//! [`Track`]s, and emits CHANGED notifications to whoever registered for
//! them. Pressing PAUSE on the controller moves the target's
//! playback status, because that is what a media player does — the state is
//! not decorative.
//!
//! ## One PSM, and why [`ProtocolHandler::psms`] still matters
//!
//! AVRCP's control channel is AVCTP on PSM 0x0017 and its **browsing**
//! channel is AVCTP on 0x001B. Both handlers here declare only 0x0017.
//! Browsing is **deliberately not wired**: [`crate::classic::avrcp`] models
//! the browsing PDUs (`GetFolderItems`, `SetBrowsedPlayer`, the
//! [`BrowseableItem`](crate::classic::avrcp::BrowseableItem) payloads) at the
//! parameter level, but browsing uses a different framing on the wire — no
//! AV/C frame, a 3-byte header straight on L2CAP — and [`Protocol::receive`]
//! parses every inbound SDU as AV/C. Registering 0x001B without that framing
//! would accept a browsing connection and then answer nothing on it, which is
//! worse for a peer than refusing the channel outright.
//!
//! ## Not modelled
//!
//! Cover art (BIP/OBEX), the browsing channel above, and AVRCP's own
//! connection policy: a real controller waits for A2DP to be up before
//! opening AVCTP, and re-opens it after a link loss. Here the handler asks
//! for its channel once and does not retry.

use crate::classic::avc::{CommandType, ResponseCode, operation_id};
use crate::classic::avctp::AVCTP_PSM;
use crate::classic::avrcp::{
    AvrcpEvent, Command, MediaAttribute, Protocol, character_set_id, event_id, media_attribute_id,
    play_status,
};
use crate::device::classic_host::{HandlerChannel, ProtocolHandler};
use crate::types::SimbleError;

/// The L2CAP MTU an AVRCP control channel is built with before the real one
/// is known. AVRCP 4.4.1 requires a control channel to carry a 512-byte AV/C
/// frame; 672 is the BR/EDR default and comfortably above it.
const AVRCP_MTU: u16 = 672;

// ---------------------------------------------------------------------------
// Shared: what a media player is
// ---------------------------------------------------------------------------

/// One track, as a media player would describe it to a remote control.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Track {
    /// Title (AVRCP media attribute 0x01).
    pub title: String,
    /// Artist (0x02).
    pub artist: String,
    /// Album (0x03).
    pub album: String,
    /// Track length in milliseconds, served in `GetPlayStatus`.
    pub duration_ms: u32,
}

impl Track {
    /// A track with a title, artist and album.
    pub fn new(title: &str, artist: &str, album: &str, duration_ms: u32) -> Self {
        Self {
            title: title.into(),
            artist: artist.into(),
            album: album.into(),
            duration_ms,
        }
    }

    /// This track as the AVRCP media attribute list a `GetElementAttributes`
    /// answers with. Empty fields are omitted rather than sent as empty
    /// strings: AVRCP 6.6.1 lets a target return only the attributes it has,
    /// and an empty title is a claim, not an absence.
    pub fn to_media_attributes(&self) -> Vec<MediaAttribute> {
        let fields = [
            (media_attribute_id::TITLE, &self.title),
            (media_attribute_id::ARTIST_NAME, &self.artist),
            (media_attribute_id::ALBUM_NAME, &self.album),
        ];
        let mut attributes: Vec<MediaAttribute> = fields
            .iter()
            .filter(|(_, value)| !value.is_empty())
            .map(|(id, value)| MediaAttribute {
                attribute_id: *id,
                character_set_id: character_set_id::UTF_8,
                value: (*value).clone(),
            })
            .collect();
        if self.duration_ms > 0 {
            attributes.push(MediaAttribute {
                attribute_id: media_attribute_id::PLAYING_TIME,
                character_set_id: character_set_id::UTF_8,
                value: self.duration_ms.to_string(),
            });
        }
        attributes
    }
}

// ---------------------------------------------------------------------------
// Target
// ---------------------------------------------------------------------------

/// An AVRCP **target** — the end that holds the music.
///
/// It answers everything and initiates only notifications. Which is what
/// makes it the honest half to point a foreign controller at: every byte it
/// sends is a reply to something the peer asked for.
#[derive(Debug)]
pub struct AvrcpTarget {
    avrcp: Protocol,
    control_cid: Option<u16>,
    /// Whether this end opens the control channel itself. AVRCP 4.2 lets
    /// either role establish AVCTP, and a phone that has just accepted an
    /// A2DP connection routinely opens it even though it is the target.
    connects: bool,
    /// PSMs still to ask the host for.
    wanted_channels: Vec<u16>,
    /// PDUs queued to go out unprompted — CHANGED notifications, which are
    /// the only thing a target sends without being asked.
    outbound: Vec<Vec<u8>>,
    /// The playlist this player is working through. Empty means nothing is
    /// selected — `GetPlayStatus` answers with the "unavailable" sentinels and
    /// `TrackChanged` reports [`NO_TRACK_UID`](crate::classic::avrcp::NO_TRACK_UID).
    /// A one-track playlist is what [`Self::set_track`] leaves behind, and
    /// FORWARD/BACKWARD then have nowhere to go but are still accepted.
    playlist: Vec<Track>,
    index: usize,
    events: Vec<AvrcpEvent>,
}

impl Default for AvrcpTarget {
    fn default() -> Self {
        Self::new()
    }
}

impl AvrcpTarget {
    /// A target that waits to be connected to — a phone, from the point of
    /// view of the car that pages it.
    pub fn new() -> Self {
        let mut avrcp = Protocol::new(AVRCP_MTU);
        // What a media player supports. Declaring an event here is a promise:
        // `RegisterNotification` for anything not in this list is answered
        // NOT IMPLEMENTED, so a controller learns the truth in one round trip
        // instead of waiting out a timeout.
        avrcp.supported_events = vec![
            event_id::PLAYBACK_STATUS_CHANGED,
            event_id::TRACK_CHANGED,
            event_id::PLAYBACK_POS_CHANGED,
            event_id::VOLUME_CHANGED,
        ];
        avrcp.volume = crate::classic::avrcp::MAXIMUM_VOLUME / 2;
        Self {
            avrcp,
            control_cid: None,
            connects: false,
            wanted_channels: Vec::new(),
            outbound: Vec::new(),
            playlist: Vec::new(),
            index: 0,
            events: Vec::new(),
        }
    }

    /// As [`Self::new`], but this end opens the AVCTP control channel rather
    /// than waiting for the controller to.
    pub fn connecting() -> Self {
        let mut target = Self::new();
        target.connects = true;
        target.wanted_channels.push(AVCTP_PSM);
        target
    }

    /// Loads a playlist and selects its first track. NEXT and PREVIOUS then
    /// move through it, which is what makes a transport key observable as
    /// something other than an event log entry.
    pub fn set_playlist(&mut self, tracks: Vec<Track>) {
        self.playlist = tracks;
        self.index = 0;
        self.publish_track();
    }

    /// Makes `track` the one being played, replacing any playlist.
    pub fn set_track(&mut self, track: Track) {
        self.playlist = vec![track];
        self.index = 0;
        self.publish_track();
    }

    /// The track now selected, if any.
    pub fn track(&self) -> Option<&Track> {
        self.playlist.get(self.index)
    }

    /// Current playback status (see
    /// [`play_status`]).
    pub fn playback_status(&self) -> u8 {
        self.avrcp.playback_status
    }

    /// Whether the player is playing right now.
    pub fn is_playing(&self) -> bool {
        self.avrcp.playback_status == play_status::PLAYING
    }

    /// Sets the playback status locally — the media app being driven from its
    /// own screen rather than from the remote — and notifies any registered
    /// controller.
    pub fn set_playback_status(&mut self, status: u8) {
        if self.avrcp.playback_status == status {
            return;
        }
        self.avrcp.playback_status = status;
        let pdus = self.avrcp.notify_playback_status_changed(status);
        self.outbound.extend(pdus);
    }

    /// The absolute volume a controller last set (0 to
    /// [`MAXIMUM_VOLUME`](crate::classic::avrcp::MAXIMUM_VOLUME)).
    pub fn volume(&self) -> u8 {
        self.avrcp.volume
    }

    /// The AV/C response code this target answers key events with. Setting it
    /// to [`ResponseCode::NotImplemented`] is how a player that has no
    /// transport controls at all says so — and the only way to reach a
    /// controller's refusal path.
    pub fn set_key_event_response(&mut self, response: ResponseCode) {
        self.avrcp.key_event_response = response;
    }

    /// Every AVRCP event this target has seen, in order.
    pub fn events(&self) -> &[AvrcpEvent] {
        &self.events
    }

    /// The PASS THROUGH key **presses** this target received, in order.
    /// Releases are filtered out: a tap is one intent, not two, and a test
    /// that counted both would pass on a controller that sent only releases.
    pub fn key_presses(&self) -> Vec<u8> {
        self.events
            .iter()
            .filter_map(|event| match event {
                AvrcpEvent::KeyEvent {
                    operation_id,
                    pressed: true,
                    ..
                } => Some(*operation_id),
                _ => None,
            })
            .collect()
    }

    /// Whether the control channel is up.
    pub fn is_connected(&self) -> bool {
        self.control_cid.is_some()
    }

    /// The media-player state the target serves, for a caller that needs
    /// something these accessors do not expose.
    pub fn protocol(&self) -> &Protocol {
        &self.avrcp
    }

    /// Copies the selected track into the fields AVRCP answers `GetPlayStatus`
    /// and `GetElementAttributes` from, and returns its UID. Notifies nobody:
    /// this also runs when a peer has just gone away, and a TrackChanged
    /// queued for a departed controller would be delivered to the next one as
    /// though the track had changed under it.
    fn load_track_fields(&mut self) -> u64 {
        let (attributes, duration, uid) = match self.playlist.get(self.index) {
            Some(track) => (
                track.to_media_attributes(),
                track.duration_ms,
                self.index as u64,
            ),
            None => (
                Vec::new(),
                crate::classic::avrcp::PLAYBACK_POSITION_UNAVAILABLE,
                crate::classic::avrcp::NO_TRACK_UID,
            ),
        };
        self.avrcp.element_attributes = attributes;
        self.avrcp.set_song_length(duration);
        self.avrcp.set_song_position(0);
        self.avrcp.set_current_track_uid(uid);
        uid
    }

    /// As [`Self::load_track_fields`], and then tells whoever registered for
    /// TrackChanged that it moved.
    fn publish_track(&mut self) {
        let uid = self.load_track_fields();
        let pdus = self.avrcp.notify_track_changed(uid);
        self.outbound.extend(pdus);
    }

    /// Applies a transport key the way a media player would. AVRCP defines
    /// what the key *means*; what the player does about it is the player's,
    /// and a target whose PAUSE left it PLAYING would be answering ACCEPTED
    /// to a lie.
    fn apply_key(&mut self, operation: u8) {
        match operation {
            operation_id::PLAY => self.set_playback_status(play_status::PLAYING),
            operation_id::PAUSE => self.set_playback_status(play_status::PAUSED),
            operation_id::STOP => self.set_playback_status(play_status::STOPPED),
            operation_id::FORWARD => {
                if self.index + 1 < self.playlist.len() {
                    self.index += 1;
                    self.publish_track();
                }
            }
            operation_id::BACKWARD if self.index > 0 => {
                self.index -= 1;
                self.publish_track();
            }
            _ => {}
        }
    }

    fn absorb(&mut self, events: Vec<AvrcpEvent>) {
        // A target that answers NOT IMPLEMENTED or REJECTED and then acts on
        // the key anyway is telling the controller a lie it has no way to
        // check. Whatever the answer is, the player must match it.
        let honours_keys = self.avrcp.key_event_response == ResponseCode::Accepted;
        for event in &events {
            if let AvrcpEvent::KeyEvent {
                operation_id,
                pressed: true,
                ..
            } = event
                && honours_keys
            {
                // Act on the press, not the release: AV/C sends both for one
                // tap, and acting on both would skip two tracks per press.
                self.apply_key(*operation_id);
            }
        }
        self.events.extend(events);
    }
}

impl ProtocolHandler for AvrcpTarget {
    fn psm(&self) -> u16 {
        AVCTP_PSM
    }

    fn on_data(&mut self, _data: &[u8], _peer_mtu: u16) -> Vec<Vec<u8>> {
        // Never called: `on_channel_data` is overridden so the handler can
        // tell its control channel from anything else the host routes here.
        Vec::new()
    }

    fn poll_channel_requests(&mut self) -> Vec<u16> {
        std::mem::take(&mut self.wanted_channels)
    }

    fn on_channel_open(&mut self, channel: HandlerChannel) {
        if channel.psm != AVCTP_PSM || self.control_cid.is_some() {
            return;
        }
        self.control_cid = Some(channel.cid);
        // Now the real MTU is known, so the AVRCP layer can cut a response at
        // the peer's limit rather than at the guess it was built with.
        self.avrcp.set_peer_mtu(channel.peer_mtu);
    }

    fn on_channel_lost(&mut self, cid: u16) {
        if self.control_cid == Some(cid) {
            self.control_cid = None;
        }
    }

    fn on_channel_data(&mut self, channel: HandlerChannel, data: &[u8]) -> Vec<Vec<u8>> {
        if Some(channel.cid) != self.control_cid {
            return Vec::new();
        }
        let (out, events) = self.avrcp.receive(data);
        self.absorb(events);
        out
    }

    fn poll_channel_output(&mut self, channel: HandlerChannel) -> Vec<Vec<u8>> {
        if Some(channel.cid) != self.control_cid {
            return Vec::new();
        }
        std::mem::take(&mut self.outbound)
    }

    fn on_channel_closed(&mut self) {
        // The AVCTP session is bound to the L2CAP connection: transaction
        // labels, half-assembled PDUs and every notification registration go
        // with it. What does *not* go is the music — the playlist, the
        // playback status and the volume are the device, not the session.
        let playlist = std::mem::take(&mut self.playlist);
        let index = self.index;
        let status = self.avrcp.playback_status;
        let volume = self.avrcp.volume;
        let key_response = self.avrcp.key_event_response;
        let events = std::mem::take(&mut self.events);
        let connects = self.connects;

        *self = if connects {
            Self::connecting()
        } else {
            Self::new()
        };
        self.playlist = playlist;
        self.index = index;
        // What the departed controller did is still evidence; the session that
        // carried it is not.
        self.events = events;
        self.avrcp.volume = volume;
        self.avrcp.playback_status = status;
        self.avrcp.key_event_response = key_response;
        self.load_track_fields();
        self.outbound.clear();
    }
}

// ---------------------------------------------------------------------------
// Controller
// ---------------------------------------------------------------------------

/// What an [`AvrcpController`] has learned from the target it is driving.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RemoteMediaPlayer {
    /// The target's playback status, from `GetPlayStatus` or a
    /// PlaybackStatusChanged notification. `None` until one arrives.
    pub playback_status: Option<u8>,
    /// Track length in milliseconds, from `GetPlayStatus`.
    pub song_length: Option<u32>,
    /// Playback position in milliseconds, from `GetPlayStatus`.
    pub song_position: Option<u32>,
    /// Track metadata, from `GetElementAttributes`.
    pub attributes: Vec<MediaAttribute>,
    /// Notification event IDs the target said it supports, from
    /// `GetCapabilities`.
    pub supported_events: Vec<u8>,
    /// The UID of the track the target last reported changing to.
    pub track_uid: Option<u64>,
}

impl RemoteMediaPlayer {
    /// The value of one media attribute, if the target sent it.
    pub fn attribute(&self, attribute_id: u32) -> Option<&str> {
        self.attributes
            .iter()
            .find(|a| a.attribute_id == attribute_id)
            .map(|a| a.value.as_str())
    }

    /// The track title the target reported.
    pub fn title(&self) -> Option<&str> {
        self.attribute(media_attribute_id::TITLE)
    }

    /// The artist the target reported.
    pub fn artist(&self) -> Option<&str> {
        self.attribute(media_attribute_id::ARTIST_NAME)
    }
}

/// An AVRCP **controller** — the end with the buttons.
///
/// It opens the AVCTP control channel, sends PASS THROUGH operations and
/// metadata queries, and folds the answers into a [`RemoteMediaPlayer`].
#[derive(Debug)]
pub struct AvrcpController {
    avrcp: Protocol,
    control_cid: Option<u16>,
    /// Whether this end opens the control channel itself, kept as a field
    /// rather than inferred from `wanted_channels`: that queue is drained the
    /// first time the host polls it, so by the time the channel drops there
    /// is nothing left to infer from and a reconnecting controller would
    /// quietly become a listening one.
    connects: bool,
    wanted_channels: Vec<u16>,
    /// Commands queued to go out. A key pressed before the channel opened
    /// waits here rather than being dropped — a button pressed while
    /// connecting is still a button pressed.
    outbound: Vec<Vec<u8>>,
    remote: RemoteMediaPlayer,
    /// Event IDs to re-register for after every CHANGED; see [`Self::monitor`].
    monitored: Vec<u8>,
    events: Vec<AvrcpEvent>,
    error: Option<String>,
}

impl Default for AvrcpController {
    fn default() -> Self {
        Self::new()
    }
}

impl AvrcpController {
    /// A controller that asks the host for its own AVCTP control channel —
    /// which is what a car head unit does once the ACL is up.
    pub fn new() -> Self {
        Self {
            avrcp: Protocol::new(AVRCP_MTU),
            control_cid: None,
            connects: true,
            wanted_channels: vec![AVCTP_PSM],
            outbound: Vec::new(),
            remote: RemoteMediaPlayer::default(),
            monitored: Vec::new(),
            events: Vec::new(),
            error: None,
        }
    }

    /// As [`Self::new`], but waits for the target to open the channel. AVRCP
    /// 4.2 permits either end to; a controller sitting on a device that was
    /// paged has no reason to race the peer for it.
    pub fn listening() -> Self {
        let mut controller = Self::new();
        controller.connects = false;
        controller.wanted_channels.clear();
        controller
    }

    /// Whether the control channel is up.
    pub fn is_connected(&self) -> bool {
        self.control_cid.is_some()
    }

    /// What this controller has learned about the target's player.
    pub fn remote(&self) -> &RemoteMediaPlayer {
        &self.remote
    }

    /// Every AVRCP event this controller has seen, in order.
    pub fn events(&self) -> &[AvrcpEvent] {
        &self.events
    }

    /// Why a command could not be built, if one could not.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Presses and releases one AV/C PASS THROUGH operation — one button
    /// tap. AV/C models a key as a press and a release (AV/C Panel 9.4), and
    /// a target is entitled to ignore a press it never sees released.
    pub fn tap(&mut self, operation: u8) {
        self.key(operation, true);
        self.key(operation, false);
    }

    /// Sends one half of a key event, for a caller that wants a *held* key —
    /// fast-forward, which only means anything for as long as it is down.
    pub fn key(&mut self, operation: u8, pressed: bool) {
        match self.avrcp.send_key_event(operation, pressed) {
            Ok(pdus) => self.outbound.extend(pdus),
            Err(e) => self.fail(e),
        }
    }

    /// PLAY.
    pub fn play(&mut self) {
        self.tap(operation_id::PLAY);
    }

    /// PAUSE.
    pub fn pause(&mut self) {
        self.tap(operation_id::PAUSE);
    }

    /// STOP.
    pub fn stop(&mut self) {
        self.tap(operation_id::STOP);
    }

    /// Next track (AV/C FORWARD).
    pub fn next_track(&mut self) {
        self.tap(operation_id::FORWARD);
    }

    /// Previous track (AV/C BACKWARD).
    pub fn previous_track(&mut self) {
        self.tap(operation_id::BACKWARD);
    }

    /// Asks the target which notification events it supports.
    pub fn query_supported_events(&mut self) {
        let result = self.avrcp.get_supported_events();
        self.queue(result);
    }

    /// Asks the target what it is playing and where it has got to.
    pub fn query_play_status(&mut self) {
        let result = self.avrcp.get_play_status();
        self.queue(result);
    }

    /// Asks the target for the current track's metadata. An empty
    /// `attribute_ids` asks for everything, which is what makes this the
    /// query that trips AVRCP's fragmentation.
    pub fn query_metadata(&mut self, attribute_ids: &[u32]) {
        let result = self
            .avrcp
            .get_element_attributes(crate::classic::avrcp::NO_TRACK_UID, attribute_ids);
        self.queue(result);
    }

    /// Registers for one notification event. The target answers with an
    /// INTERIM snapshot now and one CHANGED when it fires; AVRCP 6.7.2 spends
    /// the registration on that CHANGED, so continuous monitoring means
    /// registering again — which [`Self::monitor`] does.
    pub fn register_notification(&mut self, event: u8) {
        let result = self.avrcp.register_notification(event, 0);
        self.queue(result);
    }

    /// Registers for `event` and keeps re-registering after every CHANGED.
    pub fn monitor(&mut self, event: u8) {
        if !self.monitored.contains(&event) {
            self.monitored.push(event);
        }
        self.register_notification(event);
    }

    /// Sets the target's absolute volume, 0 to
    /// [`MAXIMUM_VOLUME`](crate::classic::avrcp::MAXIMUM_VOLUME).
    pub fn set_volume(&mut self, volume: u8) {
        let result = self.avrcp.set_absolute_volume(volume);
        self.queue(result);
    }

    /// Sends any AVRCP command, for the ones the named methods do not cover.
    pub fn send(&mut self, ctype: CommandType, command: &Command) {
        let result = self.avrcp.send_avrcp_command(ctype, command);
        self.queue(result);
    }

    fn queue(&mut self, result: Result<Vec<Vec<u8>>, SimbleError>) {
        match result {
            Ok(pdus) => self.outbound.extend(pdus),
            Err(e) => self.fail(e),
        }
    }

    fn fail(&mut self, error: SimbleError) {
        if self.error.is_none() {
            self.error = Some(error.to_string());
        }
    }

    fn absorb(&mut self, events: Vec<AvrcpEvent>) {
        for event in &events {
            match event {
                AvrcpEvent::SupportedEventsReceived(ids) => {
                    self.remote.supported_events = ids.clone();
                }
                AvrcpEvent::PlayStatusReceived {
                    song_length,
                    song_position,
                    play_status,
                } => {
                    self.remote.song_length = Some(*song_length);
                    self.remote.song_position = Some(*song_position);
                    self.remote.playback_status = Some(*play_status);
                }
                AvrcpEvent::ElementAttributesReceived(attributes) => {
                    self.remote.attributes = attributes.clone();
                }
                AvrcpEvent::NotificationReceived { event, interim } => {
                    match event {
                        crate::classic::avrcp::Event::PlaybackStatusChanged { play_status } => {
                            self.remote.playback_status = Some(*play_status);
                        }
                        crate::classic::avrcp::Event::TrackChanged { uid } => {
                            self.remote.track_uid = Some(*uid);
                        }
                        _ => {}
                    }
                    // A CHANGED spends the registration; re-arm it, or the
                    // second track change is never reported and the state
                    // above silently stops tracking the player.
                    if !*interim && self.monitored.contains(&event.event_id()) {
                        let id = event.event_id();
                        let result = self.avrcp.register_notification(id, 0);
                        self.queue(result);
                    }
                }
                _ => {}
            }
        }
        self.events.extend(events);
    }
}

impl ProtocolHandler for AvrcpController {
    fn psm(&self) -> u16 {
        AVCTP_PSM
    }

    fn on_data(&mut self, _data: &[u8], _peer_mtu: u16) -> Vec<Vec<u8>> {
        Vec::new()
    }

    fn poll_channel_requests(&mut self) -> Vec<u16> {
        std::mem::take(&mut self.wanted_channels)
    }

    fn on_channel_open(&mut self, channel: HandlerChannel) {
        if channel.psm != AVCTP_PSM || self.control_cid.is_some() {
            return;
        }
        self.control_cid = Some(channel.cid);
        self.avrcp.set_peer_mtu(channel.peer_mtu);
    }

    fn on_channel_lost(&mut self, cid: u16) {
        if self.control_cid == Some(cid) {
            self.control_cid = None;
        }
    }

    fn on_channel_data(&mut self, channel: HandlerChannel, data: &[u8]) -> Vec<Vec<u8>> {
        if Some(channel.cid) != self.control_cid {
            return Vec::new();
        }
        let (out, events) = self.avrcp.receive(data);
        self.absorb(events);
        // `out` here is not a reply to a command — a controller answers
        // almost nothing — but it *is* where the automatic
        // RequestContinuingResponse leaves, so returning it is what makes
        // fragmented metadata arrive at all.
        out
    }

    fn poll_channel_output(&mut self, channel: HandlerChannel) -> Vec<Vec<u8>> {
        if Some(channel.cid) != self.control_cid {
            return Vec::new();
        }
        std::mem::take(&mut self.outbound)
    }

    fn on_channel_closed(&mut self) {
        let events = std::mem::take(&mut self.events);
        let remote = std::mem::take(&mut self.remote);
        let error = self.error.take();
        let monitored = std::mem::take(&mut self.monitored);
        *self = if self.connects {
            Self::new()
        } else {
            Self::listening()
        };
        // What the departed player said is still evidence; the session that
        // carried it is not.
        self.events = events;
        self.remote = remote;
        self.error = error;
        self.monitored = monitored;
    }
}
