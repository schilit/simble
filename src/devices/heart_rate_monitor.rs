// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Heart Rate Monitor virtual device template.
//!
//! Combines Heart Rate Service (0x180D), Battery Service (0x180F), and
//! Device Information Service (0x180A) with configured GAP advertising.

use crate::device::VirtualDevice;
use crate::gap::{AdvertisingData, flags};
use crate::profiles::{
    BatteryService, BodySensorLocation, DeviceInformationService, HeartRateService,
};
use crate::types::{Address, AddressType};

/// A complete virtual Heart Rate Monitor BLE peripheral.
#[derive(Debug, Clone)]
pub struct HeartRateMonitor {
    /// The underlying virtual device hosting the sensor's GATT database.
    pub device: VirtualDevice,
    /// The Heart Rate Service exposing measurement notifications.
    pub hrs: HeartRateService,
    /// The Battery Service backing the sensor's battery level.
    pub bas: BatteryService,
}

impl HeartRateMonitor {
    /// Creates and configures a new Heart Rate Monitor virtual device.
    pub fn new(name: impl Into<String>, address: Address) -> Self {
        let name_str = name.into();
        let mut dev = VirtualDevice::new(&name_str, address, AddressType::Random);

        // 1. Register Device Information Service
        DeviceInformationService::new()
            .with_manufacturer_name("Google Simulated")
            .with_model_number("HRM-100")
            .with_firmware_revision("1.0.0")
            .register(&mut dev.gatt_db);

        // 2. Register Battery Service (starts at 100%)
        let bas = BatteryService::register(&mut dev.gatt_db, 100);

        // 3. Register Heart Rate Service (Chest location)
        let hrs = HeartRateService::register(&mut dev.gatt_db, Some(BodySensorLocation::Chest));

        // 4. Configure Standard Advertising Data
        let ad = AdvertisingData::new()
            .with_flags(flags::LE_GENERAL_DISCOVERABLE | flags::BR_EDR_NOT_SUPPORTED)
            .with_name(&name_str)
            .with_service_uuid_16(0x180D) // HRS
            .with_service_uuid_16(0x180F); // BAS

        dev.advertising_data = Some(ad);
        dev.is_advertising = true;

        Self {
            device: dev,
            hrs,
            bas,
        }
    }

    #[inline]
    fn notify_heart_rate(&mut self, payload: &[u8]) -> Vec<u8> {
        let _ = self
            .device
            .gatt_db
            .write(self.hrs.measurement_val_handle, payload);
        self.device
            .create_notification(self.hrs.measurement_val_handle, payload)
    }

    /// Emits a Heart Rate Measurement notification with standard 8-bit BPM.
    pub fn send_heart_rate(&mut self, bpm: u8) -> Vec<u8> {
        self.notify_heart_rate(&HeartRateService::encode_measurement_8bit(bpm))
    }

    /// Emits a Heart Rate Measurement notification with 16-bit BPM.
    pub fn send_heart_rate_16bit(&mut self, bpm: u16) -> Vec<u8> {
        self.notify_heart_rate(&HeartRateService::encode_measurement_16bit(bpm))
    }

    /// Updates battery percentage and returns an optional notification packet.
    pub fn set_battery_level(&mut self, level: u8) -> Vec<u8> {
        let _ = self.bas.set_level(&mut self.device.gatt_db, level);
        self.device
            .create_notification(self.bas.level_val_handle, &[level.min(100)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::l2cap::L2capHeader;

    #[test]
    fn test_heart_rate_monitor_device_setup_and_actions() {
        let addr = Address::from_be_bytes([0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
        let mut hrm = HeartRateMonitor::new("PixelPulse", addr);

        assert!(hrm.device.is_advertising);
        assert_eq!(hrm.device.name, "PixelPulse");

        // Send 75 BPM notification
        let notif = hrm.send_heart_rate(75);
        let (_, payload) = L2capHeader::parse(&notif).expect("Valid L2CAP notif");
        assert_eq!(payload[0], crate::att::opcode::HANDLE_VALUE_NTF);
        assert_eq!(&payload[3..], &[0x00, 75]);

        // Send 90% Battery update
        let bat_notif = hrm.set_battery_level(90);
        let (_, bat_payload) = L2capHeader::parse(&bat_notif).expect("Valid L2CAP bat notif");
        assert_eq!(&bat_payload[3..], &[90]);
    }
}
