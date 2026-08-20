// Copyright 2026 The Android Open Source Project
// SPDX-License-Identifier: Apache-2.0

//! BLE HID Mouse virtual device template.
//!
//! Implements Human Interface Device over GATT (HOGP, 0x1812), Battery Service,
//! and Device Information Service to emulate a Bluetooth optical/trackpad mouse.

use crate::device::VirtualDevice;
use crate::devices::helpers::hid_reports::MOUSE_REPORT_MAP;
use crate::devices::keyboard::hogp_uuid;
use crate::gap::{AdvertisingData, flags};
use crate::gatt::{AttributePermissions, CharacteristicProperties};
use crate::profiles::{BatteryService, DeviceInformationService};
use crate::types::{Address, AddressType, Uuid};

/// Mouse button bitmasks.
pub mod button {
    pub const NONE: u8 = 0x00;
    pub const LEFT: u8 = 0x01;
    pub const RIGHT: u8 = 0x02;
    pub const MIDDLE: u8 = 0x04;
}

/// A simulated virtual BLE mouse peripheral.
#[derive(Debug, Clone)]
pub struct BleMouse {
    pub device: VirtualDevice,
    pub input_report_val_handle: u16,
    pub input_report_cccd_handle: u16,
    pub bas: BatteryService,
}

impl BleMouse {
    /// Creates and configures a new BLE Mouse virtual device.
    pub fn new(name: impl Into<String>, address: Address) -> Self {
        let name_str = name.into();
        let mut dev = VirtualDevice::new(&name_str, address, AddressType::Random);

        // 1. Device Information Service
        DeviceInformationService::new()
            .with_manufacturer_name("Google Simulated")
            .with_model_number("BLE-MOUSE-100")
            .with_firmware_revision("1.0.0")
            .register(&mut dev.gatt_db);

        // 2. Battery Service
        let bas = BatteryService::register(&mut dev.gatt_db, 100);

        // 3. Human Interface Device Service (0x1812)
        dev.gatt_db
            .add_service(Uuid::from_u16(hogp_uuid::HID_SERVICE), true);

        // Protocol Mode (0x2A4E)
        dev.gatt_db.add_characteristic(
            Uuid::from_u16(hogp_uuid::PROTOCOL_MODE),
            CharacteristicProperties(
                CharacteristicProperties::READ | CharacteristicProperties::WRITE_WITHOUT_RESPONSE,
            ),
            vec![0x01],
            AttributePermissions::default(),
        );

        // HID Information (0x2A4A)
        dev.gatt_db.add_characteristic(
            Uuid::from_u16(hogp_uuid::HID_INFORMATION),
            CharacteristicProperties(CharacteristicProperties::READ),
            vec![0x11, 0x01, 0x00, 0x02],
            AttributePermissions::default(),
        );

        // Report Map (0x2A4B): USB HID Mouse Report Descriptor
        dev.gatt_db.add_characteristic(
            Uuid::from_u16(hogp_uuid::REPORT_MAP),
            CharacteristicProperties(CharacteristicProperties::READ),
            MOUSE_REPORT_MAP.to_vec(),
            AttributePermissions::default(),
        );

        // Input Report (0x2A4D): 4-byte mouse report [buttons, dx, dy, wheel]
        let (_, input_report_val_handle) = dev.gatt_db.add_characteristic(
            Uuid::from_u16(hogp_uuid::REPORT),
            CharacteristicProperties(
                CharacteristicProperties::READ | CharacteristicProperties::NOTIFY,
            ),
            vec![0u8; 4],
            AttributePermissions::default(),
        );
        let input_report_cccd_handle = dev.gatt_db.add_cccd();

        // 4. Advertising Data with Appearance = Mouse (0x03C2)
        let ad = AdvertisingData::new()
            .with_flags(flags::LE_GENERAL_DISCOVERABLE | flags::BR_EDR_NOT_SUPPORTED)
            .with_name(&name_str)
            .with_service_uuid_16(hogp_uuid::HID_SERVICE)
            .with_service_uuid_16(0x180F);

        dev.advertising_data = Some(ad);
        dev.is_advertising = true;

        Self {
            device: dev,
            input_report_val_handle,
            input_report_cccd_handle,
            bas,
        }
    }

    /// Emits a 4-byte standard HID mouse report: [buttons, dx, dy, wheel].
    pub fn send_report(&mut self, buttons: u8, dx: i8, dy: i8, wheel: i8) -> Vec<u8> {
        let report = [buttons, dx as u8, dy as u8, wheel as u8];
        let _ = self
            .device
            .gatt_db
            .write(self.input_report_val_handle, &report);
        self.device
            .create_notification(self.input_report_val_handle, &report)
    }

    /// Emits a cursor movement delta.
    pub fn move_by(&mut self, dx: i8, dy: i8) -> Vec<u8> {
        self.send_report(button::NONE, dx, dy, 0)
    }

    /// Emits a left mouse click and release pair.
    pub fn click_left(&mut self) -> (Vec<u8>, Vec<u8>) {
        let down = self.send_report(button::LEFT, 0, 0, 0);
        let up = self.send_report(button::NONE, 0, 0, 0);
        (down, up)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::l2cap::L2capHeader;

    #[test]
    fn test_ble_mouse_movement_and_clicking() {
        let addr = Address::from_be_bytes([0xBB, 0xCC, 0xDD, 0x11, 0x22, 0x33]);
        let mut mouse = BleMouse::new("PixelMouse", addr);

        // Move right by 10, down by 5
        let notif = mouse.move_by(10, -5);
        let (_, payload) = L2capHeader::parse(&notif).expect("Valid L2CAP");
        assert_eq!(payload[0], crate::att::opcode::HANDLE_VALUE_NTF);
        // Payload: opcode(1) + handle(2) + [buttons, dx, dy, wheel]
        assert_eq!(payload[3], button::NONE);
        assert_eq!(payload[4] as i8, 10);
        assert_eq!(payload[5] as i8, -5);
    }
}
