// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! # Simble: Native Zero-Copy Virtual Bluetooth Host Stack for Netsim
//!
//! Simble is a lightweight, memory-safe, zero-copy Bluetooth Host Stack and
//! Virtual Device simulation engine embedded within Netsim. It provides
//! declarative GATT service modeling, L2CAP multiplexing, and REST/gRPC
//! integration to enable mock Bluetooth device creation in any language.

#![allow(clippy::type_complexity, clippy::too_many_arguments)]

pub mod api;
pub mod att;
pub mod client;
pub mod crypto;
pub mod cs;
pub mod device;
pub mod devices;
pub mod gap;
pub mod gatt;
pub mod l2cap;
pub mod packets;
pub mod profiles;
pub mod service;
pub mod smp;
pub mod transport;
pub mod types;

pub use api::{
    CreateDeviceRequest, CreateDeviceResponse, DeviceEvent, DeviceRole, SetAdvertisingRequest,
};
pub use client::{DiscoveredCharacteristic, DiscoveredDescriptor, DiscoveredService, GattClient};
pub use cs::{
    CsConfig, CsDistanceEstimate, CsMainMode, CsRole, CsStepResult, compute_pbr_distance,
};
pub use device::VirtualDevice;
pub use devices::{BleKeyboard, BleMouse, EddystoneUidBeacon, HeartRateMonitor, IBeacon};
pub use gap::AdvertisingData;
pub use gatt::{AttributePermissions, CharacteristicProperties, GattDatabase};
pub use l2cap::{AclReassembler, CoCChannel, CoCManager};
pub use profiles::{
    BatteryService, BodySensorLocation, CoordinatedSetIdentificationService,
    DeviceInformationService, GenericAttributeProfileService, HeartRateService,
    PublishedAudioCapabilitiesService, RangingService, VolumeControlService,
};
pub use service::{DeviceSummary, ManagedDevice, SimbleManager};
pub use smp::{KeyStore, PairingKey, PairingKeys};
pub use transport::{HciChannel, h4_type};
pub use types::{Address, AddressType, SimbleError, Uuid};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simble_integration_smoke_test() {
        let addr = Address::from_be_bytes([0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
        let mut dev = VirtualDevice::new("SimbleSmoke", addr, AddressType::Random);

        let svc_h = dev.gatt_db.add_service(Uuid::from_u16(0x180F), true); // Battery Service
        let (_, val_h) = dev.gatt_db.add_characteristic(
            Uuid::from_u16(0x2A19), // Battery Level
            CharacteristicProperties(
                CharacteristicProperties::READ | CharacteristicProperties::NOTIFY,
            ),
            vec![100], // 100%
            AttributePermissions::default(),
        );

        assert_eq!(svc_h, 1);
        assert_eq!(val_h, 3);
        assert_eq!(dev.gatt_db.read(val_h, 0).unwrap(), &[100]);
    }
}
