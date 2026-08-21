// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Host-side Bluetooth 6.0 Channel Sounding procedures, state tracking, and distance computation.

use serde::{Deserialize, Serialize};

/// Channel Sounding Role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum CsRole {
    /// Initiator.
    Initiator = 0,
    /// Reflector.
    Reflector = 1,
}

/// Channel Sounding Main Mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum CsMainMode {
    /// Rtt.
    Rtt = 1,
    /// Pbr.
    Pbr = 2,
    /// Rtt and pbr.
    RttAndPbr = 3,
}

/// Channel Sounding configuration parameters for a connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CsConfig {
    /// Config id.
    pub config_id: u8,
    /// Role.
    pub role: CsRole,
    /// Main mode.
    pub main_mode: CsMainMode,
    /// Min steps.
    pub min_steps: u8,
    /// Max steps.
    pub max_steps: u8,
    /// Channel map.
    pub channel_map: [u8; 10],
}

impl Default for CsConfig {
    fn default() -> Self {
        Self {
            config_id: 0,
            role: CsRole::Initiator,
            main_mode: CsMainMode::RttAndPbr,
            min_steps: 4,
            max_steps: 16,
            channel_map: [0xFF; 10],
        }
    }
}

/// A single step result reported during a CS subevent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CsStepResult {
    /// Step mode.
    pub step_mode: u8,
    /// Antenna path.
    pub antenna_path: u8,
    /// Phase measurement (radians or degrees).
    pub phase_measurement: f32, // Radians or degrees
    /// Packet rssi.
    pub packet_rssi: i8,
}

/// High-level distance measurement outcome calculated from CS subevents.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CsDistanceEstimate {
    /// Connection handle.
    pub connection_handle: u16,
    /// Distance meters.
    pub distance_meters: f32,
    /// Confidence score (0.0 - 1.0).
    pub confidence_score: f32, // 0.0 - 1.0
    /// Num steps.
    pub num_steps: usize,
}

/// Computes an estimated distance from a collection of Phase-Based Ranging (PBR) steps.
///
/// Implements standard phase slope distance calculation: $\Delta d = \frac{c \cdot \Delta \phi}{4\pi \cdot \Delta f}$
pub fn compute_pbr_distance(freq_delta_hz: f32, phase_delta_rad: f32) -> f32 {
    const SPEED_OF_LIGHT: f32 = crate::types::SPEED_OF_LIGHT_M_PER_S as f32;
    (SPEED_OF_LIGHT * phase_delta_rad.abs())
        / (4.0 * std::f32::consts::PI * freq_delta_hz.abs().max(1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cs_config_defaults_and_serde() {
        let config = CsConfig::default();
        assert_eq!(config.role, CsRole::Initiator);
        assert_eq!(config.main_mode, CsMainMode::RttAndPbr);

        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("RttAndPbr"));
    }

    #[test]
    fn test_pbr_distance_calculation() {
        // Frequency step: 1 MHz (1e6 Hz), Phase rotation: 0.0419 rad (~2.4 deg)
        // Expected distance: (3e8 * 0.0419) / (4 * pi * 1e6) ~= 1.000 meter
        let dist = compute_pbr_distance(1_000_000.0, 0.04192);
        assert!((dist - 1.0).abs() < 0.05, "Calculated distance: {dist}");
    }
}
