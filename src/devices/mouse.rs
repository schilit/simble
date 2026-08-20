// Copyright 2026 The Android Open Source Project
// SPDX-License-Identifier: Apache-2.0

//! BLE HID Mouse virtual device template.
//!
//! Implements Human Interface Device over GATT (HOGP, 0x1812), Battery Service,
//! and Device Information Service to emulate a full Bluetooth mouse.

use crate::device::VirtualDevice;
use crate::devices::helpers::hid_device::{HidDeviceConfig, setup_common_hid_device};
use crate::devices::helpers::hid_reports::{MOUSE_REPORT_MAP, mouse_button};
use crate::profiles::BatteryService;
use crate::types::Address;

pub use crate::devices::helpers::hid_device::hogp_uuid;

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
        let setup = setup_common_hid_device(HidDeviceConfig {
            name: name.into(),
            model_number: "BLE-MOUSE-100",
            address,
            report_map: MOUSE_REPORT_MAP,
            initial_report_len: 4,
        });

        Self {
            device: setup.device,
            input_report_val_handle: setup.input_report_val_handle,
            input_report_cccd_handle: setup.input_report_cccd_handle,
            bas: setup.bas,
        }
    }

    /// Emits a 4-byte mouse report: `[buttons, dx, dy, wheel]`.
    pub fn send_report(&mut self, buttons: u8, dx: i8, dy: i8, wheel: i8) -> Vec<u8> {
        let report = [buttons, dx as u8, dy as u8, wheel as u8];
        let _ = self
            .device
            .gatt_db
            .write(self.input_report_val_handle, &report);
        self.device
            .create_notification(self.input_report_val_handle, &report)
    }

    /// Emits relative cursor movement.
    pub fn move_cursor(&mut self, dx: i8, dy: i8) -> Vec<u8> {
        self.send_report(mouse_button::NONE, dx, dy, 0)
    }

    /// Emits a mouse button click (down followed by up).
    pub fn click(&mut self, button: u8) -> Vec<Vec<u8>> {
        vec![
            self.send_report(button, 0, 0, 0),
            self.send_report(mouse_button::NONE, 0, 0, 0),
        ]
    }

    /// Emits a scroll wheel movement.
    pub fn scroll(&mut self, wheel: i8) -> Vec<u8> {
        self.send_report(mouse_button::NONE, 0, 0, wheel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::l2cap::L2capHeader;

    #[test]
    fn test_ble_mouse_movement_and_clicking() {
        let addr = Address::from_be_bytes([0xAA, 0xBB, 0xCC, 0x44, 0x55, 0x66]);
        let mut mouse = BleMouse::new("PixelMouse", addr);

        // Move cursor dx=10, dy=-5
        let move_pdu = mouse.move_cursor(10, -5);
        let (_, payload) = L2capHeader::parse(&move_pdu).expect("Valid L2CAP");
        assert_eq!(payload[0], crate::att::opcode::HANDLE_VALUE_NTF);
        assert_eq!(payload[3], 0); // buttons
        assert_eq!(payload[4], 10); // dx
        assert_eq!(payload[5], (-5i8) as u8); // dy

        // Click left button
        let click_pdus = mouse.click(mouse_button::LEFT);
        assert_eq!(click_pdus.len(), 2);
    }
}
