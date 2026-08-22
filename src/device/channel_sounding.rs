// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Driving a Channel Sounding procedure from the host: the **initiator**, who
//! configures and enables it, and the **reflector**, who is configured into it
//! and whose measurements have to be shipped back.
//!
//! Both are transport-free in the same way [`CisCentral`](super::CisCentral)
//! is: HCI packets in, HCI packets out. Neither carries GATT — the reflector
//! hands its host the Ranging Data *bytes* to notify, and the initiator takes
//! the bytes its host received, because who owns the ATT connection is the
//! caller's business.
//!
//! # The sequence
//!
//! ```text
//!   initiator                         controller            reflector
//!   ── LE CS Security Enable ────────────▶
//!      ◀──────────── LE CS Security Enable Complete
//!   ── LE CS Create Config (context: both) ──────────────────▶
//!      ◀──────────── LE CS Config Complete ─────── LE CS Config Complete ──▶
//!   ── LE CS Set Procedure Parameters ───▶
//!      ◀──────────── Command Complete
//!   ── LE CS Procedure Enable ───────────▶
//!      ◀──────────── LE CS Procedure Enable Complete ────────────────────▶
//!      ◀──────────── LE CS Subevent Result ─────── LE CS Subevent Result ──▶
//! ```
//!
//! After which each end has half a measurement. The reflector's half crosses
//! to the initiator over the Ranging Service; only then does a distance
//! exist. [`CsInitiator::estimate`] is where the two halves meet.
//!
//! Not driven here: LE CS Read Local/Remote Supported Capabilities and the
//! FAE table exchange. Simble's radio accepts any configuration, so
//! negotiating capabilities would be a sequence with nothing on the other end
//! of it; a host talking to a real controller must not skip them.

use crate::cs::ranging::PbrEstimate;
use crate::cs::tones::{SubeventResult, Tone, parse_subevent_result};
use crate::device::host::command;
use crate::packets::HciEvent;
use crate::profiles::ras::RangingData;

/// HCI opcodes for Channel Sounding (Vol 4, Part E, Section 7.8).
pub mod opcode {
    /// LE CS Security Enable.
    pub const LE_CS_SECURITY_ENABLE: [u8; 2] = [0x8C, 0x20];
    /// LE CS Create Config.
    pub const LE_CS_CREATE_CONFIG: [u8; 2] = [0x90, 0x20];
    /// LE CS Set Procedure Parameters.
    pub const LE_CS_SET_PROCEDURE_PARAMETERS: [u8; 2] = [0x93, 0x20];
    /// LE CS Procedure Enable.
    pub const LE_CS_PROCEDURE_ENABLE: [u8; 2] = [0x94, 0x20];
}

/// LE Meta subevent codes for Channel Sounding.
mod subevent {
    /// LE CS Security Enable Complete.
    pub const SECURITY_ENABLE_COMPLETE: u8 = 0x2E;
    /// LE CS Config Complete.
    pub const CONFIG_COMPLETE: u8 = 0x2F;
    /// LE CS Procedure Enable Complete.
    pub const PROCEDURE_ENABLE_COMPLETE: u8 = 0x30;
    /// LE CS Subevent Result.
    pub const SUBEVENT_RESULT: u8 = 0x31;
}

/// Role values in LE CS Create Config.
pub mod cs_role {
    /// The end that configures and enables the procedure.
    pub const INITIATOR: u8 = 0x00;
    /// The end that is configured into it.
    pub const REFLECTOR: u8 = 0x01;
}

/// Where an initiator's setup has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CsState {
    /// Nothing requested yet.
    Idle,
    /// LE CS Security Enable sent.
    Securing,
    /// LE CS Create Config sent.
    Configuring,
    /// LE CS Set Procedure Parameters sent.
    SettingParameters,
    /// LE CS Procedure Enable sent.
    Enabling,
    /// The procedure is running and subevent results are arriving.
    Measuring,
    /// The controller refused, with the status byte it gave.
    Failed(u8),
}

/// The initiator half of a Channel Sounding procedure.
#[derive(Debug)]
pub struct CsInitiator {
    state: CsState,
    connection_handle: u16,
    config_id: u8,
    /// The most recent subevent this device's own controller reported.
    local: Option<SubeventResult>,
    /// The most recent Ranging Data the peer sent over RAS.
    remote: Option<RangingData>,
    /// The last estimate computed, kept so a caller can read it without
    /// re-deriving, and so a page can show the previous value while the next
    /// procedure runs.
    estimate: Option<PbrEstimate>,
    /// Procedures whose two halves were combined.
    completed: u32,
    /// Procedures where the peer's data never lined up with the local half.
    mismatched: u32,
}

impl CsInitiator {
    /// An initiator that will set up configuration `config_id` when started.
    pub fn new(config_id: u8) -> Self {
        Self {
            state: CsState::Idle,
            connection_handle: 0,
            config_id,
            local: None,
            remote: None,
            estimate: None,
            completed: 0,
            mismatched: 0,
        }
    }

    /// Begins the sequence on an existing connection.
    pub fn start(&mut self, connection_handle: u16) -> Vec<Vec<u8>> {
        self.connection_handle = connection_handle;
        self.state = CsState::Securing;
        vec![command(
            opcode::LE_CS_SECURITY_ENABLE,
            &connection_handle.to_le_bytes(),
        )]
    }

    /// Where setup has got to.
    pub fn state(&self) -> CsState {
        self.state
    }

    /// True once the controller is producing measurements.
    pub fn is_measuring(&self) -> bool {
        self.state == CsState::Measuring
    }

    /// The configuration identifier this initiator created.
    pub fn config_id(&self) -> u8 {
        self.config_id
    }

    /// The tones this device's own controller last reported.
    pub fn local_tones(&self) -> &[Tone] {
        self.local.as_ref().map_or(&[], |s| s.tones.as_slice())
    }

    /// The tones the peer last sent over the Ranging Service.
    pub fn remote_tones(&self) -> &[Tone] {
        self.remote.as_ref().map_or(&[], |d| d.tones.as_slice())
    }

    /// The most recent distance estimate, if the two halves have met.
    pub fn estimate(&self) -> Option<&PbrEstimate> {
        self.estimate.as_ref()
    }

    /// How many procedures produced an estimate, and how many were dropped
    /// because the peer's data did not match the local half.
    pub fn procedure_counts(&self) -> (u32, u32) {
        (self.completed, self.mismatched)
    }

    /// Forgets the procedure, so a later connection starts over.
    pub fn reset(&mut self) {
        self.state = CsState::Idle;
        self.local = None;
        self.remote = None;
        self.estimate = None;
    }

    /// Feeds one HCI packet in, returning the commands to send back.
    pub fn on_packet(&mut self, packet: &[u8]) -> Vec<Vec<u8>> {
        let Some(event) = HciEvent::parse_h4(packet) else {
            return Vec::new();
        };
        match event {
            HciEvent::CommandComplete {
                header,
                return_parameters,
            } if header.command_opcode.get()
                == u16::from_le_bytes(opcode::LE_CS_SET_PROCEDURE_PARAMETERS)
                && self.state == CsState::SettingParameters =>
            {
                match return_parameters.first() {
                    Some(&0x00) => {
                        self.state = CsState::Enabling;
                        vec![command(
                            opcode::LE_CS_PROCEDURE_ENABLE,
                            &self.procedure_enable_params(true),
                        )]
                    }
                    Some(&status) => {
                        self.state = CsState::Failed(status);
                        Vec::new()
                    }
                    None => Vec::new(),
                }
            }
            HciEvent::Other { code: 0x3E, parameters } => self.on_le_meta(parameters),
            _ => Vec::new(),
        }
    }

    /// Dispatches one LE Meta subevent.
    fn on_le_meta(&mut self, parameters: &[u8]) -> Vec<Vec<u8>> {
        match parameters.first().copied() {
            Some(subevent::SECURITY_ENABLE_COMPLETE) if self.state == CsState::Securing => {
                match parameters.get(1) {
                    Some(&0x00) => {
                        self.state = CsState::Configuring;
                        vec![command(opcode::LE_CS_CREATE_CONFIG, &self.create_config_params())]
                    }
                    Some(&status) => {
                        self.state = CsState::Failed(status);
                        Vec::new()
                    }
                    None => Vec::new(),
                }
            }
            Some(subevent::CONFIG_COMPLETE) if self.state == CsState::Configuring => {
                match parameters.get(1) {
                    Some(&0x00) => {
                        self.state = CsState::SettingParameters;
                        vec![command(
                            opcode::LE_CS_SET_PROCEDURE_PARAMETERS,
                            &self.procedure_parameters(),
                        )]
                    }
                    Some(&status) => {
                        self.state = CsState::Failed(status);
                        Vec::new()
                    }
                    None => Vec::new(),
                }
            }
            Some(subevent::PROCEDURE_ENABLE_COMPLETE) if self.state == CsState::Enabling => {
                match parameters.get(1) {
                    Some(&0x00) => self.state = CsState::Measuring,
                    Some(&status) => self.state = CsState::Failed(status),
                    None => {}
                }
                Vec::new()
            }
            Some(subevent::SUBEVENT_RESULT) => {
                if let Some(result) = parse_subevent_result(parameters)
                    && result.connection_handle == self.connection_handle
                {
                    self.local = Some(result);
                    self.recompute();
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// Accepts a reassembled Ranging Data body the peer notified over RAS.
    ///
    /// Returns true if it parsed. This is the *other* half of every
    /// measurement; without it [`Self::estimate`] stays empty however many
    /// subevent results the local controller produces.
    pub fn on_ranging_data(&mut self, body: &[u8]) -> bool {
        let Some(data) = RangingData::parse(body) else {
            return false;
        };
        self.remote = Some(data);
        self.recompute();
        true
    }

    /// Combines the two halves when they describe the same procedure.
    ///
    /// The counter check is not bookkeeping: tones from two different
    /// procedures were measured with different oscillator phases, so summing
    /// them cancels nothing and produces a confident-looking number with no
    /// relationship to distance. Mismatches are counted, not combined.
    fn recompute(&mut self) {
        let (Some(local), Some(remote)) = (self.local.as_ref(), self.remote.as_ref()) else {
            return;
        };
        // The ranging counter is the procedure counter's low 12 bits.
        if local.procedure_counter & 0x0FFF != remote.ranging_counter {
            self.mismatched = self.mismatched.saturating_add(1);
            return;
        }
        if let Some(estimate) = crate::cs::estimate_from_tones(&local.tones, &remote.tones) {
            self.estimate = Some(estimate);
            self.completed = self.completed.saturating_add(1);
        }
    }

    /// LE CS Create Config's 28 parameter bytes (Vol 4, Part E, Section
    /// 7.8.137). `Create_Context` is 0x01 — write the configuration into the
    /// remote controller as well — which is the only way the reflector's host
    /// ever hears that a procedure exists.
    fn create_config_params(&self) -> Vec<u8> {
        let mut params = Vec::with_capacity(28);
        params.extend_from_slice(&self.connection_handle.to_le_bytes());
        params.push(self.config_id);
        params.push(0x01); // create context: local and remote
        params.push(0x02); // main mode: Phase-Based Ranging
        params.push(0xFF); // sub mode: none
        params.push(0x02); // min main mode steps
        params.push(0x14); // max main mode steps
        params.push(0x00); // main mode repetition
        params.push(0x03); // mode 0 steps
        params.push(cs_role::INITIATOR);
        params.push(0x00); // RTT type: coarse (unused with a PBR main mode)
        params.push(0x01); // CS sync PHY: LE 1M
        params.extend_from_slice(&[0xFF; 10]); // channel map: offer the band
        params.push(0x01); // channel map repetition
        params.push(0x00); // channel selection type: algorithm #3b
        params.push(0x00); // ch3c shape
        params.push(0x00); // ch3c jump
        params.push(0x00); // companion signal: not used
        debug_assert_eq!(params.len(), 28, "LE CS Create Config is 28 bytes");
        params
    }

    /// LE CS Set Procedure Parameters (Vol 4, Part E, Section 7.8.141).
    fn procedure_parameters(&self) -> Vec<u8> {
        let mut params = Vec::with_capacity(21);
        params.extend_from_slice(&self.connection_handle.to_le_bytes());
        params.push(self.config_id);
        params.extend_from_slice(&0x0028u16.to_le_bytes()); // max procedure length
        params.extend_from_slice(&0x0001u16.to_le_bytes()); // min procedure interval
        params.extend_from_slice(&0x0002u16.to_le_bytes()); // max procedure interval
        params.extend_from_slice(&0x0000u16.to_le_bytes()); // max procedure count: unlimited
        params.extend_from_slice(&[0x40, 0x0D, 0x00]); // min subevent length, µs
        params.extend_from_slice(&[0x40, 0x0D, 0x00]); // max subevent length, µs
        params.push(0x01); // tone antenna config selection: 1:1
        params.push(0x00); // PHY: LE 1M
        params.push(0x00); // TX power delta
        params.push(0x00); // preferred peer antenna
        params.push(0x00); // SNR control, initiator
        params.push(0x00); // SNR control, reflector
        params
    }

    /// LE CS Procedure Enable (Vol 4, Part E, Section 7.8.142).
    fn procedure_enable_params(&self, enable: bool) -> Vec<u8> {
        let mut params = Vec::with_capacity(4);
        params.extend_from_slice(&self.connection_handle.to_le_bytes());
        params.push(self.config_id);
        params.push(u8::from(enable));
        params
    }

    /// The command that stops the procedure.
    pub fn stop(&mut self) -> Vec<Vec<u8>> {
        if self.state != CsState::Measuring {
            return Vec::new();
        }
        self.state = CsState::Idle;
        vec![command(
            opcode::LE_CS_PROCEDURE_ENABLE,
            &self.procedure_enable_params(false),
        )]
    }
}

/// The reflector half: it issues no commands, it only collects what its
/// controller measured and hands it to its host to publish.
///
/// A reflector's measurements are worthless to itself — the distance is
/// computed at the initiator — so this type's whole output is
/// [`Self::take_ranging_data`], the bytes the Ranging Service must carry.
#[derive(Debug, Default)]
pub struct CsReflector {
    connection_handle: u16,
    config_id: Option<u8>,
    /// Ranging Data bodies not yet published, oldest first.
    pending: std::collections::VecDeque<Vec<u8>>,
    /// Subevents received since the configuration was created.
    subevents: u32,
}

impl CsReflector {
    /// A reflector waiting to be configured into a procedure.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether an initiator has created a configuration on this device.
    pub fn is_configured(&self) -> bool {
        self.config_id.is_some()
    }

    /// How many subevents this end has measured.
    pub fn subevent_count(&self) -> u32 {
        self.subevents
    }

    /// Forgets the configuration and any unsent data.
    pub fn reset(&mut self) {
        self.config_id = None;
        self.pending.clear();
        self.subevents = 0;
    }

    /// Consumes one HCI packet. The reflector never answers with a command:
    /// the initiator drives the whole procedure.
    pub fn on_packet(&mut self, packet: &[u8]) {
        let Some(HciEvent::Other {
            code: 0x3E,
            parameters,
        }) = HciEvent::parse_h4(packet)
        else {
            return;
        };
        match parameters.first().copied() {
            Some(subevent::CONFIG_COMPLETE) if parameters.get(1) == Some(&0x00) => {
                // Counting from the subevent code: status(1)
                // connection_handle(2) config_id(1) action(1)
                // main/sub mode(2) min/max steps(2) repetition(1)
                // mode_0_steps(1) puts role at offset 12.
                if parameters.len() > 12 && parameters[12] == cs_role::REFLECTOR {
                    self.connection_handle =
                        u16::from_le_bytes([parameters[2], parameters[3]]) & 0x0FFF;
                    self.config_id = Some(parameters[4]);
                }
            }
            Some(subevent::SUBEVENT_RESULT) => {
                let Some(result) = parse_subevent_result(parameters) else {
                    return;
                };
                if self.config_id != Some(result.config_id) {
                    return;
                }
                self.subevents = self.subevents.saturating_add(1);
                self.pending
                    .push_back(RangingData::from_subevent(&result, 0).to_bytes());
                // Only the newest procedure is worth sending: an initiator
                // discards data whose counter does not match its own current
                // subevent, so a backlog would be published and dropped.
                while self.pending.len() > 1 {
                    self.pending.pop_front();
                }
            }
            _ => {}
        }
    }

    /// Takes the next Ranging Data body to publish over RAS, if any.
    pub fn take_ranging_data(&mut self) -> Option<Vec<u8>> {
        self.pending.pop_front()
    }

    /// The connection the procedure runs on.
    pub fn connection_handle(&self) -> u16 {
        self.connection_handle
    }
}

#[cfg(test)]
mod tests {
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
            let packed = (u32::from(tone.i as u16) & 0x0FFF)
                | ((u32::from(tone.q as u16) & 0x0FFF) << 12);
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
                let phase = propagation_phase_rad(distance_m, channel_frequency_hz(channel))
                    + sign * offset;
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
            ("config", config_complete(0x0C, 0x0040, 1, cs_role::INITIATOR)),
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
        assert_eq!(initiator.local_tones().len(), 19);
        assert!(
            initiator.estimate().is_none(),
            "half a measurement is not a measurement"
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
        initiator.on_packet(&subevent_result(0x0040, 1, 9, &tones_at(6.0, &offsets, 1.0)));

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
    }

    #[test]
    fn test_results_for_another_connection_are_ignored() {
        let mut initiator = measuring_initiator();
        let tones = tones_at(3.0, &[0.0; 19], 1.0);
        initiator.on_packet(&subevent_result(0x0041, 1, 1, &tones));
        assert!(initiator.local_tones().is_empty());
        initiator.on_packet(&subevent_result(0x0040, 1, 1, &tones));
        assert_eq!(initiator.local_tones().len(), 19);
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
}
