// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! The **host** half of HID over GATT — what a computer does with a Bluetooth
//! keyboard or mouse once it has connected to one.
//!
//! HOGP (HID over GATT Profile 1.0) splits the two roles sharply. The *HID
//! Device* is a GATT server: it publishes the HID Service (0x1812) with a
//! Report Map (0x2A4B) and one or more Report characteristics (0x2A4D), and
//! notifies a report whenever its state changes. The *HID Host* is a GATT
//! client, and its work is the part that turns bytes back into input:
//!
//! 1. **Read the Report Map.** It is a USB HID report descriptor, and its
//!    first Application Collection says whether this device is a keyboard, a
//!    mouse, or something else. Until the host has read it, the reports that
//!    follow are uninterpretable — the same eight bytes mean different things
//!    under different descriptors.
//! 2. **Subscribe to every input Report.** Reports are notified, so the host
//!    writes each Report characteristic's CCCD.
//! 3. **Difference consecutive reports.** An input report is a *level*, not an
//!    event: it lists the keys held right now. Presses and releases are the
//!    set difference against the previous report.
//!
//! This type is transport-free in the same way [`LeHost`](super::LeHost) and
//! [`CisCentral`](super::CisCentral) are: a discovered GATT view and
//! notification payloads go in, decoded [`HidEvent`]s come out. It performs no
//! GATT itself — a caller drives an existing client (the browser scene's
//! central, an MCP session, a test) and hands the results back here.

use crate::client::DiscoveredService;
use crate::devices::helpers::hid_device::hogp_uuid;
use crate::devices::helpers::hid_reports::{
    KEYBOARD_REPORT_LEN, KeyboardReport, MouseReport, hid_to_ascii, top_level_usage, usage_label,
};
use crate::gatt::CharacteristicProperties;
use crate::types::Uuid;

/// Generic Desktop Page (0x01) usages that name a whole device.
mod generic_desktop {
    /// Usage Page (Generic Desktop).
    pub const PAGE: u16 = 0x01;
    /// Usage (Mouse).
    pub const MOUSE: u16 = 0x02;
    /// Usage (Keyboard).
    pub const KEYBOARD: u16 = 0x06;
    /// Usage (Game Pad).
    pub const GAME_PAD: u16 = 0x05;
}

/// What the Report Map says the peer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HidKind {
    /// No Report Map read yet.
    #[default]
    Unknown,
    /// Generic Desktop / Keyboard.
    Keyboard,
    /// Generic Desktop / Mouse.
    Mouse,
    /// Generic Desktop / Game Pad — recognised, but not decoded here.
    GamePad,
    /// A descriptor this host has no decoder for. The raw reports still
    /// arrive; nothing can be said about what they mean.
    Unsupported,
}

impl HidKind {
    /// A short human label.
    pub fn label(self) -> &'static str {
        match self {
            HidKind::Unknown => "unknown",
            HidKind::Keyboard => "keyboard",
            HidKind::Mouse => "mouse",
            HidKind::GamePad => "game pad",
            HidKind::Unsupported => "unsupported",
        }
    }
}

/// One decoded input event, in the form a windowing system would consume.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HidEvent {
    /// The Report Map was read and the peer identified.
    Identified {
        /// What the descriptor says the device is.
        kind: HidKind,
        /// The Report Map's own bytes, so a caller can show them.
        report_map: Vec<u8>,
    },
    /// A key went down.
    KeyDown {
        /// HID usage ID from the Keyboard/Keypad page (0x07).
        usage: u8,
        /// Modifier bitmap in force when the key went down.
        modifiers: u8,
        /// The character it produces on a US layout, if any.
        character: Option<char>,
        /// A name for keys that produce no character ("Enter", "Backspace").
        label: Option<&'static str>,
    },
    /// A key came back up.
    KeyUp {
        /// HID usage ID that is no longer held.
        usage: u8,
    },
    /// The keyboard reported more keys than it can distinguish; the report
    /// carries no usable key state and was discarded.
    RollOver,
    /// The pointer moved, or the wheel turned.
    Pointer {
        /// Horizontal displacement, right positive.
        dx: i8,
        /// Vertical displacement, down positive.
        dy: i8,
        /// Wheel detents, away-from-user positive.
        wheel: i8,
    },
    /// A mouse button went down.
    ButtonDown {
        /// Button number, 1 = left, 2 = right, 3 = middle.
        button: u8,
    },
    /// A mouse button came back up.
    ButtonUp {
        /// Button number, 1 = left, 2 = right, 3 = middle.
        button: u8,
    },
}

/// The GATT operations a host must perform on a freshly discovered HID device
/// before any input can reach it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HidPlan {
    /// Report Map value handles to read.
    pub read: Vec<u16>,
    /// Input Report value handles to subscribe to.
    pub subscribe: Vec<u16>,
}

impl HidPlan {
    /// True when the peer exposes no HID service worth driving.
    pub fn is_empty(&self) -> bool {
        self.read.is_empty() && self.subscribe.is_empty()
    }
}

/// A HID host: decodes one peer's input reports into [`HidEvent`]s.
#[derive(Debug, Default)]
pub struct HidHost {
    kind: HidKind,
    report_map: Option<Vec<u8>>,
    report_map_handles: Vec<u16>,
    report_handles: Vec<u16>,
    /// The previous keyboard report, per report handle. A device may expose
    /// several input reports and each is differenced against its own history.
    previous_keys: std::collections::BTreeMap<u16, KeyboardReport>,
    previous_buttons: std::collections::BTreeMap<u16, u8>,
    /// The most recent input report, exactly as it arrived. Kept so a caller
    /// can show the bytes beside what they decoded to — the difference
    /// between a protocol demo and a typing toy.
    last_report: Option<(u16, Vec<u8>)>,
    events: std::collections::VecDeque<HidEvent>,
}

impl HidHost {
    /// Creates a host that has not yet seen a peer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads a discovered GATT view and returns what to read and subscribe to.
    ///
    /// Only Report characteristics that actually notify are subscribed: HOGP
    /// permits output and feature reports on the same UUID, and writing a CCCD
    /// on one of those earns an ATT error rather than input.
    pub fn plan(&mut self, services: &[DiscoveredService]) -> HidPlan {
        let hid_service = Uuid::from_u16(hogp_uuid::HID_SERVICE);
        let report_map = Uuid::from_u16(hogp_uuid::REPORT_MAP);
        let report = Uuid::from_u16(hogp_uuid::REPORT);
        let mut plan = HidPlan::default();
        for service in services.iter().filter(|s| s.uuid == hid_service) {
            for characteristic in &service.characteristics {
                if characteristic.uuid == report_map {
                    plan.read.push(characteristic.value_handle);
                } else if characteristic.uuid == report
                    && characteristic.properties & CharacteristicProperties::NOTIFY != 0
                {
                    plan.subscribe.push(characteristic.value_handle);
                }
            }
        }
        self.report_map_handles = plan.read.clone();
        self.report_handles = plan.subscribe.clone();
        plan
    }

    /// Feeds the host a value it read. Only the Report Map is of interest;
    /// anything else is ignored, so a caller may pass every read through.
    pub fn on_read(&mut self, value_handle: u16, value: &[u8]) {
        if !self.report_map_handles.contains(&value_handle) {
            return;
        }
        self.kind = match top_level_usage(value) {
            Some((generic_desktop::PAGE, generic_desktop::KEYBOARD)) => HidKind::Keyboard,
            Some((generic_desktop::PAGE, generic_desktop::MOUSE)) => HidKind::Mouse,
            Some((generic_desktop::PAGE, generic_desktop::GAME_PAD)) => HidKind::GamePad,
            _ => HidKind::Unsupported,
        };
        self.report_map = Some(value.to_vec());
        self.events.push_back(HidEvent::Identified {
            kind: self.kind,
            report_map: value.to_vec(),
        });
    }

    /// Feeds the host one notified input report.
    ///
    /// A report that arrives before the Report Map has been read is dropped
    /// rather than guessed at: an 8-byte payload is a keyboard report only
    /// because a descriptor said so.
    pub fn on_notification(&mut self, value_handle: u16, value: &[u8]) {
        if !self.report_handles.contains(&value_handle) {
            return;
        }
        self.last_report = Some((value_handle, value.to_vec()));
        match self.kind {
            HidKind::Keyboard => self.decode_keyboard(value_handle, value),
            HidKind::Mouse => self.decode_mouse(value_handle, value),
            HidKind::Unknown | HidKind::GamePad | HidKind::Unsupported => {}
        }
    }

    /// Differences an 8-byte keyboard report against the previous one.
    fn decode_keyboard(&mut self, value_handle: u16, value: &[u8]) {
        let Some(report) = KeyboardReport::parse(value) else {
            return;
        };
        if report.is_rollover_error() {
            // The held keys are unknowable, so the previous report is kept:
            // treating a rollover as "all keys released" would fire spurious
            // key-up events in the middle of a fast burst of typing.
            self.events.push_back(HidEvent::RollOver);
            return;
        }
        let previous = self.previous_keys.entry(value_handle).or_default();
        let pressed = report.newly_pressed(previous);
        let released = report.released(previous);
        *previous = report;
        for usage in released {
            self.events.push_back(HidEvent::KeyUp { usage });
        }
        for usage in pressed {
            self.events.push_back(HidEvent::KeyDown {
                usage,
                modifiers: report.modifiers,
                character: hid_to_ascii(usage, report.modifiers),
                label: usage_label(usage),
            });
        }
    }

    /// Turns relative motion into a pointer event and the button bitmap into
    /// edges.
    fn decode_mouse(&mut self, value_handle: u16, value: &[u8]) {
        let Some(report) = MouseReport::parse(value) else {
            return;
        };
        let previous = self.previous_buttons.entry(value_handle).or_default();
        let pressed = report.newly_pressed(*previous);
        let released = report.released(*previous);
        *previous = report.buttons;
        for button in 1..=3u8 {
            let bit = 1 << (button - 1);
            if released & bit != 0 {
                self.events.push_back(HidEvent::ButtonUp { button });
            }
            if pressed & bit != 0 {
                self.events.push_back(HidEvent::ButtonDown { button });
            }
        }
        // A report with no motion still carries button edges, but emitting a
        // zero-displacement Pointer event would make a still mouse look busy.
        if report.dx != 0 || report.dy != 0 || report.wheel != 0 {
            self.events.push_back(HidEvent::Pointer {
                dx: report.dx,
                dy: report.dy,
                wheel: report.wheel,
            });
        }
    }

    /// Takes the events decoded since the last call, oldest first.
    pub fn drain_events(&mut self) -> Vec<HidEvent> {
        self.events.drain(..).collect()
    }

    /// What the Report Map said the peer is.
    pub fn kind(&self) -> HidKind {
        self.kind
    }

    /// The Report Map bytes, once read.
    pub fn report_map(&self) -> Option<&[u8]> {
        self.report_map.as_deref()
    }

    /// The most recent input report and the handle it arrived on.
    pub fn last_report(&self) -> Option<(u16, &[u8])> {
        self.last_report
            .as_ref()
            .map(|(handle, bytes)| (*handle, bytes.as_slice()))
    }

    /// True once the peer has been identified and its reports subscribed.
    pub fn is_ready(&self) -> bool {
        self.report_map.is_some() && !self.report_handles.is_empty()
    }

    /// The largest input report this host expects, for a caller sizing a view
    /// of the raw bytes.
    pub fn report_len_hint(&self) -> usize {
        match self.kind {
            HidKind::Keyboard => KEYBOARD_REPORT_LEN,
            HidKind::Mouse => 4,
            _ => 0,
        }
    }
}

#[cfg(test)]
#[path = "hid_host_tests.rs"]
mod tests;
