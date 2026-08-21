// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! REST / JSON Data Transfer Objects (DTOs) for controlling virtual Bluetooth devices.

use crate::types::{Address, AddressType, Uuid};
use serde::{Deserialize, Serialize};

/// Role of the virtual Bluetooth device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceRole {
    /// A device that advertises and accepts connections.
    Peripheral,
    /// A device that scans and initiates connections.
    Central,
}

/// Request to create a new virtual Bluetooth device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateDeviceRequest {
    /// Device name (used in GAP advertising).
    pub name: String,
    /// Explicit device address; a random one is assigned if omitted.
    pub address: Option<Address>,
    /// Device address type (defaults to random).
    #[serde(default = "default_address_type")]
    pub address_type: AddressType,
    /// GAP role for the device (defaults to peripheral).
    #[serde(default = "default_role")]
    pub role: DeviceRole,
    /// Optional predefined device template to instantiate (e.g. a heart rate monitor).
    #[serde(default)]
    pub predefined_template: Option<String>,
}

fn default_address_type() -> AddressType {
    AddressType::Random
}

fn default_role() -> DeviceRole {
    DeviceRole::Peripheral
}

/// Response returned when a device is created.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateDeviceResponse {
    /// Server-assigned identifier for the new device.
    pub device_id: String,
    /// The device's assigned Bluetooth address.
    pub address: Address,
    /// Human-readable creation status.
    pub status: String,
}

/// GATT Service declaration payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GattServiceConfig {
    /// The service UUID.
    pub uuid: Uuid,
    /// Whether this is a primary service (defaults to true).
    #[serde(default = "default_true")]
    pub primary: bool,
    /// Characteristics belonging to this service.
    #[serde(default)]
    pub characteristics: Vec<GattCharacteristicConfig>,
}

fn default_true() -> bool {
    true
}

/// GATT Characteristic configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GattCharacteristicConfig {
    /// The characteristic UUID.
    pub uuid: Uuid,
    /// Characteristic property names (e.g. "read", "notify").
    #[serde(default)]
    pub properties: Vec<String>,
    /// Initial characteristic value, hex-encoded.
    #[serde(default)]
    pub initial_value_hex: String,
}

/// Request to enable or update LE Advertising.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetAdvertisingRequest {
    /// Whether advertising should be enabled.
    pub enabled: bool,
    /// Advertising interval in milliseconds, if specified.
    pub interval_ms: Option<u16>,
    /// Complete local name to advertise, if any.
    pub complete_local_name: Option<String>,
    /// Service UUIDs to include in the advertisement.
    #[serde(default)]
    pub service_uuids: Vec<Uuid>,
    /// Manufacturer-specific data, hex-encoded, if any.
    pub manufacturer_data_hex: Option<String>,
}

/// Request to update a GATT characteristic's value and optionally notify centrals.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateCharacteristicRequest {
    /// New characteristic value, hex-encoded.
    pub value_hex: String,
    /// Whether to notify subscribed centrals of the change.
    #[serde(default)]
    pub notify: bool,
}

/// Asynchronous events emitted by the virtual device engine (streamed via SSE).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event_type", rename_all = "snake_case")]
pub enum DeviceEvent {
    /// A peer established a connection.
    Connected {
        /// The connecting peer's address.
        peer_address: Address,
        /// Handle assigned to the new connection.
        connection_handle: u16,
    },
    /// A connection was torn down.
    Disconnected {
        /// Handle of the closed connection.
        connection_handle: u16,
        /// HCI reason code for the disconnection.
        reason: u8,
    },
    /// A peer wrote to a characteristic.
    CharacteristicWritten {
        /// Handle of the connection that performed the write.
        connection_handle: u16,
        /// Attribute handle that was written.
        handle: u16,
        /// Written value, hex-encoded.
        value_hex: String,
    },
    /// A peer initiated SMP pairing.
    PairingRequested {
        /// Handle of the connection requesting pairing.
        connection_handle: u16,
        /// The peer's advertised IO capability.
        io_capability: u8,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn test_create_device_request_json_serde() {
        let json_data = r#"{
            "name": "HeartRateMonitor",
            "address": "F0:DE:F1:22:33:44",
            "address_type": "random",
            "role": "peripheral"
        }"#;

        let req: CreateDeviceRequest = serde_json::from_str(json_data).unwrap();
        assert_eq!(req.name, "HeartRateMonitor");
        assert_eq!(req.address.unwrap().to_string(), "F0:DE:F1:22:33:44");
        assert_eq!(req.role, DeviceRole::Peripheral);
    }

    #[test]
    fn test_device_event_json_serde() {
        let evt = DeviceEvent::Connected {
            peer_address: Address::from_str("00:11:22:33:44:55").unwrap(),
            connection_handle: 0x0040,
        };

        let json_str = serde_json::to_string(&evt).unwrap();
        assert!(json_str.contains("connected"));
        assert!(json_str.contains("00:11:22:33:44:55"));
    }
}
