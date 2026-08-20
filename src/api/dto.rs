// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! REST / JSON Data Transfer Objects (DTOs) for controlling virtual Bluetooth devices.

use crate::types::{Address, AddressType, Uuid};
use serde::{Deserialize, Serialize};

/// Role of the virtual Bluetooth device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceRole {
    Peripheral,
    Central,
}

/// Request to create a new virtual Bluetooth device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateDeviceRequest {
    pub name: String,
    pub address: Option<Address>,
    #[serde(default = "default_address_type")]
    pub address_type: AddressType,
    #[serde(default = "default_role")]
    pub role: DeviceRole,
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
    pub device_id: String,
    pub address: Address,
    pub status: String,
}

/// GATT Service declaration payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GattServiceConfig {
    pub uuid: Uuid,
    #[serde(default = "default_true")]
    pub primary: bool,
    #[serde(default)]
    pub characteristics: Vec<GattCharacteristicConfig>,
}

fn default_true() -> bool {
    true
}

/// GATT Characteristic configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GattCharacteristicConfig {
    pub uuid: Uuid,
    #[serde(default)]
    pub properties: Vec<String>,
    #[serde(default)]
    pub initial_value_hex: String,
}

/// Request to enable or update LE Advertising.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetAdvertisingRequest {
    pub enabled: bool,
    pub interval_ms: Option<u16>,
    pub complete_local_name: Option<String>,
    #[serde(default)]
    pub service_uuids: Vec<Uuid>,
    pub manufacturer_data_hex: Option<String>,
}

/// Request to update a GATT characteristic's value and optionally notify centrals.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateCharacteristicRequest {
    pub value_hex: String,
    #[serde(default)]
    pub notify: bool,
}

/// Asynchronous events emitted by the virtual device engine (streamed via SSE).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event_type", rename_all = "snake_case")]
pub enum DeviceEvent {
    Connected {
        peer_address: Address,
        connection_handle: u16,
    },
    Disconnected {
        connection_handle: u16,
        reason: u8,
    },
    CharacteristicWritten {
        connection_handle: u16,
        handle: u16,
        value_hex: String,
    },
    PairingRequested {
        connection_handle: u16,
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
