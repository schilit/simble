// Copyright 2026 The Android Open Source Project
// SPDX-License-Identifier: Apache-2.0

//! BLE HID Keyboard virtual device template.
//!
//! Implements Human Interface Device over GATT (HOGP, 0x1812), Battery Service,
//! and Device Information Service to emulate a full Bluetooth keyboard.

use crate::device::VirtualDevice;
use crate::devices::helpers::hid_reports::{KEYBOARD_REPORT_MAP, ascii_to_hid, modifier};
use crate::gap::{AdvertisingData, flags};
use crate::gatt::{AttributePermissions, CharacteristicProperties};
use crate::profiles::{BatteryService, DeviceInformationService};
use crate::types::{Address, AddressType, Uuid};

/// Standard HOGP characteristic UUIDs.
pub mod hogp_uuid {
    pub const HID_SERVICE: u16 = 0x1812;
    pub const PROTOCOL_MODE: u16 = 0x2A4E;
    pub const REPORT: u16 = 0x2A4D;
    pub const REPORT_MAP: u16 = 0x2A4B;
    pub const HID_INFORMATION: u16 = 0x2A4A;
    pub const HID_CONTROL_POINT: u16 = 0x2A4C;
}

/// A simulated virtual BLE keyboard peripheral.
#[derive(Debug, Clone)]
pub struct BleKeyboard {
    pub device: VirtualDevice,
    pub input_report_val_handle: u16,
    pub input_report_cccd_handle: u16,
    pub bas: BatteryService,
}

impl BleKeyboard {
    /// Creates and configures a new BLE Keyboard virtual device.
    pub fn new(name: impl Into<String>, address: Address) -> Self {
        let name_str = name.into();
        let mut dev = VirtualDevice::new(&name_str, address, AddressType::Random);

        // 1. Device Information Service
        DeviceInformationService::new()
            .with_manufacturer_name("Google Simulated")
            .with_model_number("BLE-KBD-100")
            .with_firmware_revision("1.0.0")
            .register(&mut dev.gatt_db);

        // 2. Battery Service
        let bas = BatteryService::register(&mut dev.gatt_db, 100);

        // 3. Human Interface Device Service (0x1812)
        dev.gatt_db
            .add_service(Uuid::from_u16(hogp_uuid::HID_SERVICE), true);

        // Protocol Mode (0x2A4E): 0x01 = Report Protocol Mode
        dev.gatt_db.add_characteristic(
            Uuid::from_u16(hogp_uuid::PROTOCOL_MODE),
            CharacteristicProperties(
                CharacteristicProperties::READ | CharacteristicProperties::WRITE_WITHOUT_RESPONSE,
            ),
            vec![0x01],
            AttributePermissions::default(),
        );

        // HID Information (0x2A4A): bcdHID (0x0111), bCountryCode (0x00), Flags (0x02 = RemoteWake)
        dev.gatt_db.add_characteristic(
            Uuid::from_u16(hogp_uuid::HID_INFORMATION),
            CharacteristicProperties(CharacteristicProperties::READ),
            vec![0x11, 0x01, 0x00, 0x02],
            AttributePermissions::default(),
        );

        // HID Control Point (0x2A4C)
        dev.gatt_db.add_characteristic(
            Uuid::from_u16(hogp_uuid::HID_CONTROL_POINT),
            CharacteristicProperties(CharacteristicProperties::WRITE_WITHOUT_RESPONSE),
            vec![0x00],
            AttributePermissions::default(),
        );

        // Report Map (0x2A4B): USB HID Report Descriptor
        dev.gatt_db.add_characteristic(
            Uuid::from_u16(hogp_uuid::REPORT_MAP),
            CharacteristicProperties(CharacteristicProperties::READ),
            KEYBOARD_REPORT_MAP.to_vec(),
            AttributePermissions::default(),
        );

        // Input Report (0x2A4D): 8-byte standard keyboard input report
        let (_, input_report_val_handle) = dev.gatt_db.add_characteristic(
            Uuid::from_u16(hogp_uuid::REPORT),
            CharacteristicProperties(
                CharacteristicProperties::READ | CharacteristicProperties::NOTIFY,
            ),
            vec![0u8; 8],
            AttributePermissions::default(),
        );
        let input_report_cccd_handle = dev.gatt_db.add_cccd();

        // 4. Advertising Data with Appearance = Keyboard (0x03C1)
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

    /// Emits a keyboard report with the specified modifier and keycodes.
    pub fn send_report(&mut self, modifier_byte: u8, keycodes: &[u8]) -> Vec<u8> {
        let mut report = [0u8; 8];
        report[0] = modifier_byte;
        for (i, &key) in keycodes.iter().take(6).enumerate() {
            report[2 + i] = key;
        }

        let _ = self
            .device
            .gatt_db
            .write(self.input_report_val_handle, &report);
        self.device
            .create_notification(self.input_report_val_handle, &report)
    }

    /// Emits a key press event.
    pub fn press_key(&mut self, modifier_byte: u8, keycode: u8) -> Vec<u8> {
        self.send_report(modifier_byte, &[keycode])
    }

    /// Emits a key release event (all keys up).
    pub fn release_keys(&mut self) -> Vec<u8> {
        self.send_report(modifier::NONE, &[])
    }

    /// Simulates typing a sequence of ASCII characters, returning pairs of (press, release) packets.
    pub fn type_text(&mut self, text: &str) -> Vec<Vec<u8>> {
        let mut packets = Vec::new();
        for c in text.chars() {
            if let Some((mod_byte, key)) = ascii_to_hid(c) {
                packets.push(self.press_key(mod_byte, key));
                packets.push(self.release_keys());
            }
        }
        packets
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::helpers::hid_reports::keycode;
    use crate::l2cap::L2capHeader;

    #[test]
    fn test_ble_keyboard_keystrokes_and_typing() {
        let addr = Address::from_be_bytes([0xAA, 0xBB, 0xCC, 0x11, 0x22, 0x33]);
        let mut kbd = BleKeyboard::new("PixelKeyboard", addr);

        // Press 'A'
        let notif = kbd.press_key(modifier::NONE, keycode::KEY_A);
        let (_, payload) = L2capHeader::parse(&notif).expect("Valid L2CAP");
        assert_eq!(payload[0], crate::att::opcode::HANDLE_VALUE_NTF);
        // Payload: opcode(1) + handle(2) + report(8)
        assert_eq!(payload[3], modifier::NONE); // modifier
        assert_eq!(payload[5], keycode::KEY_A); // key 1

        // Type text "Hi" -> 'H' (Shift+h) and 'i' (i)
        let packets = kbd.type_text("Hi");
        assert_eq!(packets.len(), 4); // 2 chars * (press + release)
    }
}
