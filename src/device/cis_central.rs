// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Opening a Connected Isochronous Stream as the **initiator** — the source
//! half of LE Audio's media plane.
//!
//! [`LeHost`](super::LeHost) covers the acceptor: a sink answers LE CIS
//! Request and opens an Output data path. The initiator's job is the
//! opposite and strictly more work, because a central *defines* the stream
//! before either side can carry audio:
//!
//! 1. **LE Set CIG Parameters** declares the isochronous group — SDU
//!    interval, maximum SDU size, latency, PHY. The controller answers with
//!    the CIS connection handles, which is the only way to learn them.
//! 2. **LE Create CIS** binds one of those handles to an ACL connection. The
//!    peer sees LE CIS Request; when it accepts, both sides get LE CIS
//!    Established.
//! 3. **LE Setup ISO Data Path** with the *Input* direction opens the
//!    host-to-controller path, after which SDUs may be written.
//!
//! This type is transport-free in the same way `LeHost` is: HCI packets in,
//! HCI packets out. It carries no GATT, so a caller must have already walked
//! the peer's ASCS (Config Codec → Config QoS → Enable) — a controller will
//! open a CIS to a peer whose endpoint is not configured, and the audio then
//! goes nowhere.

use crate::device::host::{command, iso_data_path, le_host_feature, opcode};
use crate::packets::{HciEvent, build_iso_packet};

/// The shape of the stream to open. The defaults are LE Audio's 16_2
/// configuration — 16 kHz, 10 ms frames, 40 octets — which is what simble's
/// PAC record advertises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CisConfig {
    /// Identifies the isochronous group.
    pub cig_id: u8,
    /// Identifies the stream within the group.
    pub cis_id: u8,
    /// Time between SDUs, in microseconds. Must match the frame duration the
    /// codec was configured with, or audio drifts against the clock.
    pub sdu_interval_us: u32,
    /// Largest SDU the central will send, in octets.
    pub max_sdu: u16,
    /// Transport latency budget, in milliseconds.
    pub max_transport_latency_ms: u16,
    /// Retransmission number.
    pub retransmissions: u8,
    /// PHY bitfield: 0x01 = 1M, 0x02 = 2M, 0x04 = Coded.
    pub phy: u8,
}

impl Default for CisConfig {
    fn default() -> Self {
        Self {
            cig_id: 1,
            cis_id: 1,
            sdu_interval_us: 10_000,
            max_sdu: 40,
            max_transport_latency_ms: 10,
            retransmissions: 2,
            phy: 0x02,
        }
    }
}

/// Where the establishment sequence has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CisState {
    /// Nothing requested yet.
    Idle,
    /// LE Set CIG Parameters sent; waiting for the CIS handles.
    ConfiguringGroup,
    /// LE Create CIS sent; waiting for the peer to accept.
    Connecting,
    /// Established; waiting for the data path to open.
    OpeningDataPath,
    /// Ready — SDUs written now reach the peer.
    Streaming,
    /// The controller or the peer refused, with the status byte it gave.
    Failed(u8),
}

/// The initiator side of a CIS.
#[derive(Debug)]
pub struct CisCentral {
    config: CisConfig,
    state: CisState,
    acl_handle: u16,
    cis_handle: Option<u16>,
    tx_sequence: u16,
}

impl CisCentral {
    /// Creates an initiator that will open `config`'s stream when started.
    pub fn new(config: CisConfig) -> Self {
        Self {
            config,
            state: CisState::Idle,
            acl_handle: 0,
            cis_handle: None,
            tx_sequence: 0,
        }
    }

    /// Commands that must be issued while the controller is still
    /// unconnected.
    ///
    /// LE Set Host Feature is Command Disallowed once a connection exists
    /// (Vol 4, Part E, Section 7.8.115), so a central that waits until it
    /// wants audio has already missed its chance — and the failure surfaces
    /// much later, as LE Create CIS being refused.
    pub fn init_commands() -> Vec<Vec<u8>> {
        vec![command(
            opcode::LE_SET_HOST_FEATURE,
            &[le_host_feature::CONNECTED_ISOCHRONOUS_STREAM, 0x01],
        )]
    }

    /// Begins establishment over an existing ACL connection.
    pub fn start(&mut self, acl_handle: u16) -> Vec<Vec<u8>> {
        self.acl_handle = acl_handle;
        self.state = CisState::ConfiguringGroup;
        vec![command(
            opcode::LE_SET_CIG_PARAMETERS,
            &self.cig_parameters(),
        )]
    }

    /// LE Set CIG Parameters' parameter block (Vol 4, Part E, 7.8.97).
    fn cig_parameters(&self) -> Vec<u8> {
        let interval = self.config.sdu_interval_us.to_le_bytes();
        let mut params = Vec::with_capacity(15 + 9);
        params.push(self.config.cig_id);
        // SDU intervals are 24-bit. The peripheral-to-central direction is
        // still given an interval even though this source never receives.
        params.extend_from_slice(&interval[..3]);
        params.extend_from_slice(&interval[..3]);
        params.push(0x00); // worst case SCA: 251-500 ppm
        params.push(0x00); // packing: sequential
        params.push(0x00); // framing: unframed
        params.extend_from_slice(&self.config.max_transport_latency_ms.to_le_bytes());
        params.extend_from_slice(&self.config.max_transport_latency_ms.to_le_bytes());
        params.push(0x01); // CIS count

        params.push(self.config.cis_id);
        params.extend_from_slice(&self.config.max_sdu.to_le_bytes());
        // A pure source sends nothing in the peripheral-to-central
        // direction; a zero max SDU is how that is declared.
        params.extend_from_slice(&0u16.to_le_bytes());
        params.push(self.config.phy);
        params.push(self.config.phy);
        params.push(self.config.retransmissions);
        params.push(self.config.retransmissions);
        params
    }

    /// Feeds one HCI packet in, and returns the commands to send back.
    pub fn on_packet(&mut self, packet: &[u8]) -> Vec<Vec<u8>> {
        let Some(event) = HciEvent::parse_h4(packet) else {
            return Vec::new();
        };
        match event {
            HciEvent::CommandComplete {
                header,
                return_parameters,
            } if header.command_opcode.get()
                == u16::from_le_bytes(opcode::LE_SET_CIG_PARAMETERS)
                && self.state == CisState::ConfiguringGroup =>
            {
                self.on_cig_configured(return_parameters)
            }
            HciEvent::CommandComplete { header, .. }
                if header.command_opcode.get()
                    == u16::from_le_bytes(opcode::LE_SETUP_ISO_DATA_PATH)
                    && self.state == CisState::OpeningDataPath =>
            {
                self.state = CisState::Streaming;
                Vec::new()
            }
            HciEvent::LeCisEstablished(event) if self.state == CisState::Connecting => {
                if event.status != 0x00 {
                    self.state = CisState::Failed(event.status);
                    return Vec::new();
                }
                self.cis_handle = Some(event.cis_connection_handle.get());
                self.state = CisState::OpeningDataPath;
                vec![command(
                    opcode::LE_SETUP_ISO_DATA_PATH,
                    &self.input_data_path(),
                )]
            }
            _ => Vec::new(),
        }
    }

    /// Reads the CIS handles out of LE Set CIG Parameters' return
    /// parameters: status, CIG_ID, CIS_Count, then one handle per stream.
    fn on_cig_configured(&mut self, return_parameters: &[u8]) -> Vec<Vec<u8>> {
        let Some((&status, rest)) = return_parameters.split_first() else {
            return Vec::new();
        };
        if status != 0x00 {
            self.state = CisState::Failed(status);
            return Vec::new();
        }
        // rest: CIG_ID, CIS_Count, handles...
        if rest.len() < 4 {
            self.state = CisState::Failed(0xFF);
            return Vec::new();
        }
        let cis_handle = u16::from_le_bytes([rest[2], rest[3]]);
        self.cis_handle = Some(cis_handle);
        self.state = CisState::Connecting;

        let mut params = Vec::with_capacity(5);
        params.push(0x01); // CIS count
        params.extend_from_slice(&cis_handle.to_le_bytes());
        params.extend_from_slice(&self.acl_handle.to_le_bytes());
        vec![command(opcode::LE_CREATE_CIS, &params)]
    }

    /// LE Setup ISO Data Path for the source direction (Vol 4, Part E,
    /// 7.8.109). Codec ID 0x03 is "transparent": the codec runs in the host,
    /// which is where the LC3 encoder lives.
    fn input_data_path(&self) -> Vec<u8> {
        let mut params = Vec::with_capacity(13);
        params.extend_from_slice(&self.cis_handle.unwrap_or(0).to_le_bytes());
        params.push(iso_data_path::INPUT);
        params.push(iso_data_path::HCI);
        params.extend_from_slice(&[0x03, 0x00, 0x00, 0x00, 0x00]); // transparent
        params.extend_from_slice(&[0x00, 0x00, 0x00]); // controller delay
        params.push(0x00); // codec configuration length
        params
    }

    /// Wraps one SDU for the established stream, or `None` if the data path
    /// is not open yet — audio written early is discarded by the controller,
    /// so this refuses rather than pretending to send.
    pub fn send_sdu(&mut self, sdu: &[u8]) -> Option<Vec<u8>> {
        if self.state != CisState::Streaming {
            return None;
        }
        let handle = self.cis_handle?;
        let packet = build_iso_packet(handle, self.tx_sequence, sdu);
        self.tx_sequence = self.tx_sequence.wrapping_add(1);
        Some(packet)
    }

    /// Where establishment has got to.
    pub fn state(&self) -> CisState {
        self.state
    }

    /// True once SDUs will actually reach the peer.
    pub fn is_streaming(&self) -> bool {
        self.state == CisState::Streaming
    }

    /// The established stream's handle, if there is one.
    pub fn cis_handle(&self) -> Option<u16> {
        self.cis_handle
    }

    /// Forgets the stream, so a later connection starts over.
    pub fn reset(&mut self) {
        self.state = CisState::Idle;
        self.cis_handle = None;
        self.tx_sequence = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a Command Complete H4 packet for `opcode` with `params`.
    fn command_complete(opcode: [u8; 2], params: &[u8]) -> Vec<u8> {
        let mut packet = vec![0x04, 0x0E, (3 + params.len()) as u8, 0x01];
        packet.extend_from_slice(&opcode);
        packet.extend_from_slice(params);
        packet
    }

    /// LE CIS Established, as the controller reports it.
    fn cis_established(status: u8, handle: u16) -> Vec<u8> {
        let mut params = vec![0x19, status];
        params.extend_from_slice(&handle.to_le_bytes());
        params.extend_from_slice(&[0u8; 24]); // timing fields
        let mut packet = vec![0x04, 0x3E, params.len() as u8];
        packet.extend_from_slice(&params);
        packet
    }

    #[test]
    fn test_the_full_establishment_sequence() {
        let mut cis = CisCentral::new(CisConfig::default());
        assert_eq!(cis.state(), CisState::Idle);

        let started = cis.start(0x0040);
        assert_eq!(started.len(), 1);
        assert_eq!(&started[0][1..3], &opcode::LE_SET_CIG_PARAMETERS);
        assert_eq!(cis.state(), CisState::ConfiguringGroup);

        // Status, CIG_ID, CIS_Count, one handle.
        let next = cis.on_packet(&command_complete(
            opcode::LE_SET_CIG_PARAMETERS,
            &[0x00, 0x01, 0x01, 0x00, 0x0E],
        ));
        assert_eq!(cis.state(), CisState::Connecting);
        assert_eq!(&next[0][1..3], &opcode::LE_CREATE_CIS);
        // The command binds the CIS handle to the ACL handle start() was given.
        assert_eq!(&next[0][4..], &[0x01, 0x00, 0x0E, 0x40, 0x00]);

        let next = cis.on_packet(&cis_established(0x00, 0x0E00));
        assert_eq!(cis.state(), CisState::OpeningDataPath);
        assert_eq!(&next[0][1..3], &opcode::LE_SETUP_ISO_DATA_PATH);
        // A source opens the Input direction, not Output.
        assert_eq!(next[0][6], iso_data_path::INPUT);

        cis.on_packet(&command_complete(opcode::LE_SETUP_ISO_DATA_PATH, &[0x00]));
        assert_eq!(cis.state(), CisState::Streaming);
        assert_eq!(cis.cis_handle(), Some(0x0E00));
    }

    #[test]
    fn test_audio_is_refused_until_the_data_path_is_open() {
        let mut cis = CisCentral::new(CisConfig::default());
        assert!(cis.send_sdu(&[0xAA; 40]).is_none(), "idle");

        cis.start(0x0040);
        cis.on_packet(&command_complete(
            opcode::LE_SET_CIG_PARAMETERS,
            &[0x00, 0x01, 0x01, 0x00, 0x0E],
        ));
        cis.on_packet(&cis_established(0x00, 0x0E00));
        assert!(
            cis.send_sdu(&[0xAA; 40]).is_none(),
            "established is not the same as ready: the data path is still opening"
        );

        cis.on_packet(&command_complete(opcode::LE_SETUP_ISO_DATA_PATH, &[0x00]));
        let packet = cis.send_sdu(&[0xAA; 40]).expect("streaming");
        assert_eq!(packet[0], crate::transport::h4_type::HCI_ISO_DATA);
    }

    #[test]
    fn test_sequence_numbers_advance_per_sdu() {
        let mut cis = CisCentral::new(CisConfig::default());
        cis.start(0x0040);
        cis.on_packet(&command_complete(
            opcode::LE_SET_CIG_PARAMETERS,
            &[0x00, 0x01, 0x01, 0x00, 0x0E],
        ));
        cis.on_packet(&cis_established(0x00, 0x0E00));
        cis.on_packet(&command_complete(opcode::LE_SETUP_ISO_DATA_PATH, &[0x00]));

        // The sequence number lives in the ISO data load header, bytes 5-6.
        let first = cis.send_sdu(&[0x01; 8]).unwrap();
        let second = cis.send_sdu(&[0x02; 8]).unwrap();
        assert_eq!(u16::from_le_bytes([first[5], first[6]]), 0);
        assert_eq!(u16::from_le_bytes([second[5], second[6]]), 1);
    }

    #[test]
    fn test_a_refused_group_is_recorded_rather_than_retried() {
        let mut cis = CisCentral::new(CisConfig::default());
        cis.start(0x0040);
        // 0x12 = Invalid HCI Command Parameters, what a controller answers
        // when the SDU interval and latency cannot be met.
        let next = cis.on_packet(&command_complete(opcode::LE_SET_CIG_PARAMETERS, &[0x12]));
        assert!(next.is_empty());
        assert_eq!(cis.state(), CisState::Failed(0x12));
        assert!(cis.send_sdu(&[0xAA; 40]).is_none());
    }

    #[test]
    fn test_a_peer_that_rejects_the_stream_is_recorded() {
        let mut cis = CisCentral::new(CisConfig::default());
        cis.start(0x0040);
        cis.on_packet(&command_complete(
            opcode::LE_SET_CIG_PARAMETERS,
            &[0x00, 0x01, 0x01, 0x00, 0x0E],
        ));
        // 0x0D = Connection Rejected due to Limited Resources.
        cis.on_packet(&cis_established(0x0D, 0x0000));
        assert_eq!(cis.state(), CisState::Failed(0x0D));
    }

    #[test]
    fn test_host_feature_is_declared_before_connecting() {
        let init = CisCentral::init_commands();
        assert_eq!(&init[0][1..3], &opcode::LE_SET_HOST_FEATURE);
        assert_eq!(
            init[0][4],
            le_host_feature::CONNECTED_ISOCHRONOUS_STREAM,
            "bit 32 is what a controller checks before allowing LE Create CIS"
        );
    }
}
