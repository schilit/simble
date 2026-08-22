// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! The **client** half of ASCS: configuring a peer's audio endpoint.
//!
//! [`ascs`](super::ascs) is the server — a sink publishing its ASEs and
//! running their state machine. This is the Unicast Client's side: the
//! operations a source writes to the ASE Control Point to drive a peer's
//! sink ASE from Idle to Streaming.
//!
//! For a **Sink** ASE the sequence is Config Codec → Config QoS → Enable,
//! after which the client establishes the CIS and the server moves itself to
//! Streaming. Receiver Start Ready is deliberately absent: a client sends it
//! only for *Source* ASEs, where the client is the one receiving.
//!
//! The operations are pure encoders — no I/O, no connection — so the payload
//! layouts can be tested directly, which matters because a malformed control
//! point write is answered with a response code rather than a link failure
//! and is easy to mistake for a working stream.

/// ASE Control Point characteristic (ASCS Section 3.3).
pub const ASE_CONTROL_POINT_UUID: u16 = 0x2BC6;
/// Sink ASE characteristic (ASCS Section 3.1).
pub const SINK_ASE_UUID: u16 = 0x2BC4;

/// ASE Control Point opcodes (ASCS Table 4.1).
mod opcode {
    pub const CONFIG_CODEC: u8 = 0x01;
    pub const CONFIG_QOS: u8 = 0x02;
    pub const ENABLE: u8 = 0x03;
    pub const RELEASE: u8 = 0x08;
}

/// LC3's coding format in an LE Audio Codec_ID (Assigned Numbers).
const CODEC_ID_LC3: u8 = 0x06;

/// How the stream should be configured. The defaults describe LE Audio's
/// 16_2 configuration — 16 kHz, 10 ms, 40 octets — matching what simble's
/// PAC record advertises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AseConfig {
    /// Which ASE on the peer to drive.
    pub ase_id: u8,
    /// Sampling frequency code (0x03 = 16 kHz, 0x08 = 48 kHz).
    pub sampling_frequency: u8,
    /// Frame duration code (0x00 = 7.5 ms, 0x01 = 10 ms).
    pub frame_duration: u8,
    /// Audio channel allocation bitmap (0x01 = front left).
    pub channel_allocation: u32,
    /// Codec frame size, in octets.
    pub octets_per_frame: u16,
    /// Time between SDUs, in microseconds.
    pub sdu_interval_us: u32,
    /// Largest SDU, in octets — normally `octets_per_frame`.
    pub max_sdu: u16,
    /// Which isochronous group and stream this ASE is bound to.
    pub cig_id: u8,
    /// Stream identifier within the group.
    pub cis_id: u8,
    /// Retransmission number.
    pub retransmissions: u8,
    /// Transport latency budget, in milliseconds.
    pub max_transport_latency_ms: u16,
    /// Presentation delay, in microseconds.
    pub presentation_delay_us: u32,
    /// Streaming Audio Contexts bitmap (0x0004 = Media, 0x0002 = Conversational).
    pub audio_context: u16,
}

impl Default for AseConfig {
    fn default() -> Self {
        Self {
            ase_id: 1,
            sampling_frequency: 0x03,
            frame_duration: 0x01,
            channel_allocation: 0x0000_0001,
            octets_per_frame: 40,
            sdu_interval_us: 10_000,
            max_sdu: 40,
            cig_id: 1,
            cis_id: 1,
            retransmissions: 2,
            max_transport_latency_ms: 10,
            presentation_delay_us: 40_000,
            audio_context: 0x0004,
        }
    }
}

impl AseConfig {
    /// Config Codec (ASCS Section 5.1): tells the peer which codec and codec
    /// configuration the stream will use.
    pub fn config_codec(&self) -> Vec<u8> {
        // Codec_Specific_Configuration is an LTV list (BAP Section 4.3.1).
        let mut codec_config = Vec::with_capacity(16);
        codec_config.extend_from_slice(&[0x02, 0x01, self.sampling_frequency]);
        codec_config.extend_from_slice(&[0x02, 0x02, self.frame_duration]);
        codec_config.push(0x05);
        codec_config.push(0x03);
        codec_config.extend_from_slice(&self.channel_allocation.to_le_bytes());
        codec_config.push(0x03);
        codec_config.push(0x04);
        codec_config.extend_from_slice(&self.octets_per_frame.to_le_bytes());

        let mut op = Vec::with_capacity(11 + codec_config.len());
        op.push(opcode::CONFIG_CODEC);
        op.push(0x01); // one ASE
        op.push(self.ase_id);
        op.push(0x02); // target latency: balanced latency and reliability
        op.push(0x02); // target PHY: LE 2M
        op.extend_from_slice(&[CODEC_ID_LC3, 0x00, 0x00, 0x00, 0x00]);
        op.push(codec_config.len() as u8);
        op.extend_from_slice(&codec_config);
        op
    }

    /// Config QoS (ASCS Section 5.2): binds the ASE to a CIS and fixes the
    /// stream's timing. The CIG and CIS ids must match the ones the central
    /// uses in LE Set CIG Parameters, or the controller opens a stream the
    /// endpoint is not expecting.
    pub fn config_qos(&self) -> Vec<u8> {
        let interval = self.sdu_interval_us.to_le_bytes();
        let delay = self.presentation_delay_us.to_le_bytes();
        let mut op = Vec::with_capacity(19);
        op.push(opcode::CONFIG_QOS);
        op.push(0x01); // one ASE
        op.push(self.ase_id);
        op.push(self.cig_id);
        op.push(self.cis_id);
        op.extend_from_slice(&interval[..3]); // SDU interval is 24-bit
        op.push(0x00); // framing: unframed
        op.push(0x02); // PHY: LE 2M
        op.extend_from_slice(&self.max_sdu.to_le_bytes());
        op.push(self.retransmissions);
        op.extend_from_slice(&self.max_transport_latency_ms.to_le_bytes());
        op.extend_from_slice(&delay[..3]); // presentation delay is 24-bit
        op
    }

    /// Enable (ASCS Section 5.3): moves the ASE to Enabling, carrying the
    /// metadata that says what the stream is for.
    pub fn enable(&self) -> Vec<u8> {
        // Metadata is an LTV list; type 0x02 is Streaming Audio Contexts.
        let mut metadata = Vec::with_capacity(4);
        metadata.push(0x03);
        metadata.push(0x02);
        metadata.extend_from_slice(&self.audio_context.to_le_bytes());

        let mut op = Vec::with_capacity(4 + metadata.len());
        op.push(opcode::ENABLE);
        op.push(0x01); // one ASE
        op.push(self.ase_id);
        op.push(metadata.len() as u8);
        op.extend_from_slice(&metadata);
        op
    }

    /// Release (ASCS Section 5.8): tears the ASE back down to Idle, so a
    /// later stream starts from a known state rather than inheriting a
    /// half-configured endpoint.
    pub fn release(&self) -> Vec<u8> {
        vec![opcode::RELEASE, 0x01, self.ase_id]
    }
}

/// ASE states, as reported in byte 1 of a Sink ASE characteristic (ASCS
/// Table 4.2). Reading these back is how a client knows the peer actually
/// accepted an operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AseState {
    /// Nothing configured.
    Idle,
    /// Codec accepted.
    CodecConfigured,
    /// QoS accepted; ready for Enable.
    QosConfigured,
    /// Enabled, waiting for the CIS.
    Enabling,
    /// Carrying audio.
    Streaming,
    /// Being torn down.
    Disabling,
    /// Returning to Idle.
    Releasing,
    /// A value this build does not know.
    Unknown(u8),
}

impl AseState {
    /// Reads the state out of a Sink ASE characteristic value.
    pub fn from_characteristic(value: &[u8]) -> Option<Self> {
        Some(match *value.get(1)? {
            0x00 => Self::Idle,
            0x01 => Self::CodecConfigured,
            0x02 => Self::QosConfigured,
            0x03 => Self::Enabling,
            0x04 => Self::Streaming,
            0x05 => Self::Disabling,
            0x06 => Self::Releasing,
            other => Self::Unknown(other),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// These are the exact bytes that drove a real sink from Idle to
    /// Enabling over netsim, captured from the working interop run. Encoding
    /// them from `AseConfig` has to reproduce them byte for byte.
    #[test]
    fn test_operations_match_the_bytes_verified_on_the_wire() {
        let config = AseConfig::default();

        assert_eq!(
            config.config_codec(),
            vec![
                0x01, 0x01, // Config Codec, one ASE
                0x01, 0x02, 0x02, // ASE 1, balanced latency, 2M PHY
                0x06, 0x00, 0x00, 0x00, 0x00, // LC3
                0x10, // 16 octets of codec configuration
                0x02, 0x01, 0x03, // 16 kHz
                0x02, 0x02, 0x01, // 10 ms
                0x05, 0x03, 0x01, 0x00, 0x00, 0x00, // front left
                0x03, 0x04, 0x28, 0x00, // 40 octets per frame
            ]
        );

        assert_eq!(
            config.config_qos(),
            vec![
                0x02, 0x01, // Config QoS, one ASE
                0x01, 0x01, 0x01, // ASE 1, CIG 1, CIS 1
                0x10, 0x27, 0x00, // 10 000 us SDU interval
                0x00, 0x02, // unframed, 2M PHY
                0x28, 0x00, // 40 octet SDU
                0x02, // 2 retransmissions
                0x0A, 0x00, // 10 ms transport latency
                0x40, 0x9C, 0x00, // 40 000 us presentation delay
            ]
        );

        assert_eq!(
            config.enable(),
            vec![0x03, 0x01, 0x01, 0x04, 0x03, 0x02, 0x04, 0x00]
        );
    }

    #[test]
    fn test_the_codec_configuration_length_matches_its_contents() {
        // A wrong length byte is accepted by the ATT layer and rejected by
        // the endpoint, which looks like an unrelated failure much later.
        for frequency in [0x03, 0x08] {
            let config = AseConfig {
                sampling_frequency: frequency,
                octets_per_frame: 120,
                ..Default::default()
            };
            let op = config.config_codec();
            let declared = op[10] as usize;
            assert_eq!(declared, op.len() - 11, "declared length must match");
        }
    }

    #[test]
    fn test_a_48khz_configuration_encodes_its_own_values() {
        let config = AseConfig {
            sampling_frequency: 0x08,
            octets_per_frame: 120,
            max_sdu: 120,
            ..Default::default()
        };
        let codec = config.config_codec();
        assert!(codec.windows(3).any(|w| w == [0x02, 0x01, 0x08]), "48 kHz");
        assert!(
            codec.windows(4).any(|w| w == [0x03, 0x04, 0x78, 0x00]),
            "120 octets per frame"
        );
        // Max_SDU follows opcode, count, ASE/CIG/CIS, the 24-bit interval,
        // framing and PHY — offset 10.
        let qos = config.config_qos();
        assert_eq!(&qos[10..12], &[0x78, 0x00], "120 octet SDU");
    }

    #[test]
    fn test_ase_state_reads_the_second_byte() {
        assert_eq!(
            AseState::from_characteristic(&[0x01, 0x00]),
            Some(AseState::Idle)
        );
        assert_eq!(
            AseState::from_characteristic(&[0x01, 0x03]),
            Some(AseState::Enabling)
        );
        assert_eq!(
            AseState::from_characteristic(&[0x01, 0x04]),
            Some(AseState::Streaming)
        );
        // A one-byte value has no state field; that is not state Idle.
        assert_eq!(AseState::from_characteristic(&[0x01]), None);
    }
}
