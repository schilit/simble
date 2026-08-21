// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Error types for the Simble Bluetooth stack.

use thiserror::Error;

/// Simble errors across all layers (HCI, L2CAP, ATT, SMP, GATT).
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SimbleError {
    #[error("Packet parsing failed: {0}")]
    /// Packet parse error.
    PacketParseError(String),

    #[error("Invalid parameter: {0}")]
    /// Invalid parameter.
    InvalidParameter(String),

    #[error("ATT Protocol Error code: {0:#04x}")]
    /// Att error.
    AttError(u8),

    #[error("SMP Protocol Error: {0}")]
    /// Smp error.
    SmpError(String),

    #[error("Device State Error: {0}")]
    /// Device error.
    DeviceError(String),

    #[error("Device Not Found: {0}")]
    /// Device not found.
    DeviceNotFound(String),

    #[error("GATT Error: {0}")]
    /// Gatt.
    Gatt(String),

    #[error("Transport Error: {0}")]
    /// Transport.
    Transport(String),

    #[error("IO Error: {0}")]
    /// Io error.
    IoError(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simble_error_formatting() {
        let err = SimbleError::PacketParseError("Buffer too small".into());
        assert_eq!(err.to_string(), "Packet parsing failed: Buffer too small");
    }
}
