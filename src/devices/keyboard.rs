// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! BLE HID Keyboard virtual device template.
//!
//! Implements Human Interface Device over GATT (HOGP, 0x1812), Battery Service,
//! and Device Information Service to emulate a full Bluetooth keyboard.

use crate::device::VirtualDevice;
use crate::devices::helpers::hid_device::{HidDeviceConfig, setup_common_hid_device};
use crate::devices::helpers::hid_reports::{KEYBOARD_REPORT_MAP, ascii_to_hid, modifier};
use crate::profiles::BatteryService;
use crate::types::Address;

pub use crate::devices::helpers::hid_device::hogp_uuid;

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
        let setup = setup_common_hid_device(HidDeviceConfig {
            name: name.into(),
            model_number: "BLE-KBD-100",
            address,
            report_map: KEYBOARD_REPORT_MAP,
            initial_report_len: 8,
        });

        Self {
            device: setup.device,
            input_report_val_handle: setup.input_report_val_handle,
            input_report_cccd_handle: setup.input_report_cccd_handle,
            bas: setup.bas,
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
