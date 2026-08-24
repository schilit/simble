// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! A Bluetooth keyboard and the computer it types into, on one simulated
//! BR/EDR link.
//!
//! The counterpart of [`SpeakerScene`](crate::device::SpeakerScene), and the
//! reason the dispatch work had to come first: HID runs **two PSMs at once**
//! — Control 0x0011 and Interrupt 0x0013 — so until a
//! [`ProtocolHandler`](crate::device::ProtocolHandler) could claim more than
//! one PSM, no scene could host a keyboard at all.
//!
//! The transport half is [`ProfileScene`]; this file is only the keyboard:
//! who the two devices are, what the keyboard publishes in SDP, and
//! accessors named for the HID roles.
//!
//! ## Which end pages
//!
//! The **computer** is the initiator here: it inquires, pages, and opens
//! both L2CAP channels. A real keyboard reconnects to a bonded host on its
//! own — either side may (HID Profile v1.1.1 §5.3.4.13) — and nothing here
//! does that, because there is no bond to reconnect to.
//!
//! ## What is not here
//!
//! No pairing: a real keyboard bonds, and Android will not use one that has
//! not. No SDP search — the computer opens 0x0011 without reading the
//! keyboard's HID record, so it never learns the report descriptor and
//! decodes input reports by *length* instead (see
//! [`crate::device::classic_hid`]). No boot-protocol reformatting.

use crate::classic::hid::{DEVICE_SUBCLASS_COMBO, make_service_sdp_records};
use crate::classic::sdp::{SdpServer, Service};
use crate::device::classic_hid::{ClassicHidDevice, ClassicHidHost};
use crate::device::profile_scene::{DeviceSpec, LinkPhase, ProfileScene};
use crate::device::{ClassicHost, SdpHandler};
use crate::devices::helpers::hid_reports::KEYBOARD_REPORT_MAP;
use crate::types::Address;

/// The keyboard's BD_ADDR.
pub const KEYBOARD_ADDRESS: Address = Address::new([0x24, 0x11, 0x00, 0xCC, 0xBB, 0xAA]);
/// The computer's BD_ADDR.
pub const COMPUTER_ADDRESS: Address = Address::new([0x1C, 0x01, 0x00, 0xCC, 0xBB, 0xAA]);

/// The keyboard's Class of Device, 0x2540C0: major class Peripheral, minor
/// class Keyboard, Rendering + Audio service bits clear. This is the number
/// that puts a keyboard icon in a pairing list.
const KEYBOARD_CLASS_OF_DEVICE: [u8; 3] = [0xC0, 0x05, 0x25];
/// The computer's Class of Device, 0x10010C: major class Computer, minor
/// class Laptop.
const COMPUTER_CLASS_OF_DEVICE: [u8; 3] = [0x0C, 0x01, 0x10];

/// SDP record handle for the keyboard's HID record.
const HID_SERVICE_RECORD_HANDLE: u32 = 0x0001_1124;

/// A keyboard and a computer on one simulated BR/EDR link.
pub struct KeyboardScene {
    scene: ProfileScene,
}

impl Default for KeyboardScene {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyboardScene {
    /// A discoverable, connectable keyboard publishing a HID SDP record with
    /// the standard boot keyboard report map, and a computer that will find
    /// it and open both HID channels.
    pub fn new() -> Self {
        Self::with_scan_enable(crate::device::classic_host::scan_enable::INQUIRY_AND_PAGE)
    }

    /// As [`Self::new`], with the keyboard's Scan Enable chosen — the way to
    /// build a keyboard that is deliberately not findable.
    pub fn with_scan_enable(scan_enable: u8) -> Self {
        let mut sdp = SdpHandler::new(SdpServer::new());
        sdp.server_mut()
            .service_records
            .insert(HID_SERVICE_RECORD_HANDLE, keyboard_service_record());
        Self {
            scene: ProfileScene::new(
                DeviceSpec::initiator(
                    "Simble Computer",
                    COMPUTER_CLASS_OF_DEVICE,
                    COMPUTER_ADDRESS,
                    vec![Box::new(ClassicHidHost::new())],
                ),
                DeviceSpec::acceptor(
                    "Simble Keyboard",
                    KEYBOARD_CLASS_OF_DEVICE,
                    KEYBOARD_ADDRESS,
                    vec![Box::new(sdp), Box::new(ClassicHidDevice::keyboard())],
                )
                .with_scan_enable(scan_enable),
            ),
        }
    }

    /// How far the transport has got.
    pub fn phase(&self) -> LinkPhase {
        self.scene.phase()
    }

    /// Why the plan stopped, if it did.
    pub fn error(&self) -> Option<&str> {
        self.scene.error()
    }

    /// The computer's HID host.
    pub fn computer(&self) -> &ClassicHidHost {
        self.scene.initiator::<ClassicHidHost>()
    }

    /// The computer's HID host, mutably — for issuing transactions.
    pub fn computer_mut(&mut self) -> &mut ClassicHidHost {
        self.scene.initiator_mut::<ClassicHidHost>()
    }

    /// The keyboard.
    pub fn keyboard(&self) -> &ClassicHidDevice {
        self.scene.acceptor::<ClassicHidDevice>()
    }

    /// The keyboard, mutably — for queueing keystrokes.
    pub fn keyboard_mut(&mut self) -> &mut ClassicHidDevice {
        self.scene.acceptor_mut::<ClassicHidDevice>()
    }

    /// The computer's BR/EDR host, for assertions the plan does not cover.
    pub fn computer_host(&self) -> &ClassicHost {
        self.scene.initiator_host()
    }

    /// The keyboard's BR/EDR host.
    pub fn keyboard_host(&self) -> &ClassicHost {
        self.scene.acceptor_host()
    }

    /// Advances the scene one step.
    pub fn tick(&mut self) {
        self.scene.tick();
    }

    /// Runs until `done` is true or `steps` have passed.
    pub fn run_until(&mut self, steps: usize, mut done: impl FnMut(&Self) -> bool) -> bool {
        for _ in 0..steps {
            if done(self) {
                return true;
            }
            self.tick();
        }
        done(self)
    }

    /// Runs until both HID channels are open at both ends, or gives up after
    /// `steps`. Both ends is the point: a computer that has opened Control
    /// and Interrupt has still not connected if the keyboard never saw the
    /// second one.
    pub fn run_until_connected(&mut self, steps: usize) -> bool {
        self.run_until(steps, |scene| {
            scene.computer().is_connected() && scene.keyboard().is_connected()
        })
    }
}

/// The keyboard's HID SDP record: the report map a host would read to learn
/// what the reports on the interrupt channel mean.
fn keyboard_service_record() -> Service {
    make_service_sdp_records(
        HID_SERVICE_RECORD_HANDLE,
        KEYBOARD_REPORT_MAP,
        DEVICE_SUBCLASS_COMBO,
    )
}
