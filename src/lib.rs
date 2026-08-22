// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! # SimBLE: a native, zero-copy virtual Bluetooth host stack
//!
//! SimBLE creates virtual Bluetooth devices for testing — a simulated
//! heart-rate monitor, keyboard, LE Audio earbud, hands-free car kit, and more
//! — that other software can scan, connect to, and pair with. It implements the
//! Bluetooth host stack (HCI, L2CAP, ATT/GATT, SMP, plus the Classic and LE
//! Audio profiles) in pure, dependency-light Rust, with no async runtime and no
//! C.
//!
//! Devices can be built from the Rust API, an Android-shaped API, or short
//! [Rhai](https://rhai.rs) scripts. SimBLE connects to
//! [netsim](https://android.googlesource.com/platform/tools/netsim), the Android
//! emulator's network simulator, over a WebSocket, and can also reach real
//! hardware through a USB Bluetooth dongle.

#![allow(clippy::type_complexity, clippy::too_many_arguments)]
// Every public item carries a doc comment; combined with CI's `-D warnings`
// this makes a new undocumented public item fail the build.
#![warn(missing_docs)]

pub mod android;
pub mod api;
pub mod att;
pub mod classic;
pub mod client;
pub mod controller;
pub mod crypto;
pub mod cs;
pub mod device;
pub mod devices;
pub mod df;
pub mod gap;
pub mod gatt;
pub mod l2cap;
// The MCP server (`simble mcp`) uses std stdin/stdout and native transports.
#[cfg(not(target_arch = "wasm32"))]
pub mod mcp;
pub mod packets;
pub mod profiles;
pub mod scripting;
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
pub use smp::{KeyStore, PairingConfig, PairingKey, PairingKeys, PairingSession, Role as SmpRole};
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
