// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Classic HID as [`ProtocolHandler`]s — a Bluetooth keyboard and the
//! computer it types into.
//!
//! ## Two PSMs, and why that was the blocker
//!
//! HIDP runs on **two** L2CAP channels with **different PSMs**: Control
//! (0x0011) carries request/response transactions — GET_REPORT, SET_REPORT,
//! SET_PROTOCOL — and Interrupt (0x0013) carries the asynchronous DATA that
//! is the actual typing (HID Profile v1.1.1 §5.2.2). One device, two PSMs,
//! and the old dispatch resolved a handler by `psm() == psm`, so a HID
//! device could only ever have been half-registered. That is what
//! [`ProtocolHandler::psms`] is for.
//!
//! Which channel a PDU arrived on is not a detail: the same `DATA` header
//! means "here is the report you asked for" on Control and "a key just went
//! down" on Interrupt. A handler that could not tell them apart would report
//! phantom keystrokes for every GET_REPORT it answered.
//!
//! ## The two roles
//!
//! [`ClassicHidDevice`] is the keyboard: it answers control transactions
//! from a stored report set and sends input reports on the interrupt
//! channel. It initiates no L2CAP channel — a keyboard is paged.
//!
//! [`ClassicHidHost`] is the computer: it opens Control, then Interrupt (in
//! that order, which the profile requires), issues transactions, and decodes
//! the input reports that come back with
//! [`crate::devices::helpers::hid_reports`], the same decoder the LE HOGP
//! host uses.
//!
//! ## The name
//!
//! [`crate::device::HidHost`] is already taken by the **LE/HOGP** host, and
//! `android::BluetoothHidHost` — see [`crate::scripting::hid`] — is the
//! script binding for that one. Android's proxy genuinely spans both
//! transports (`HidHostService.java` has no transport-specific code; the
//! split happens down in `bta/hh/`), so `ClassicHidHost` is *the same role on
//! the other transport*, not a different profile. The prefix is here because
//! Rust needs two names, not because Bluetooth has two roles.
//!
//! Not modelled: reconnection initiated by the device (either side may
//! reconnect per §5.3.4.13, but nothing here does), the SDP search that
//! would find the report descriptor, boot-protocol report reformatting, and
//! Virtual Cable state — an unplug is reported as an event and changes
//! nothing.

use crate::classic::hid::{
    HID_CONTROL_PSM, HID_INTERRUPT_PSM, HidDevice, HidDeviceEvent, HidHost, HidHostEvent,
    InterruptData, receive_interrupt, report_type,
};
use crate::device::classic_host::{HandlerChannel, ProtocolHandler};
use crate::devices::helpers::hid_reports::{KeyboardReport, MouseReport};

// ---------------------------------------------------------------------------
// Device role
// ---------------------------------------------------------------------------

/// A Classic HID **device** — a keyboard or mouse — as a two-PSM handler.
#[derive(Debug)]
pub struct ClassicHidDevice {
    device: HidDevice,
    control_cid: Option<u16>,
    interrupt_cid: Option<u16>,
    /// Input report PDUs waiting for the interrupt channel. A keystroke made
    /// before the host has opened Interrupt waits here rather than being
    /// dropped — a key pressed while connecting is still a key pressed.
    pending_input: Vec<Vec<u8>>,
    /// Control transactions this device has answered, in order.
    events: Vec<HidDeviceEvent>,
    /// DATA that arrived on the *interrupt* channel: output reports, which
    /// is how a host drives a keyboard's LEDs.
    interrupt_rx: Vec<InterruptData>,
}

impl Default for ClassicHidDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl ClassicHidDevice {
    /// A device with no reports declared. A host asking for one gets
    /// `HANDSHAKE(ERR_INVALID_REPORT_ID)` until [`Self::put_report`] says
    /// otherwise.
    pub fn new() -> Self {
        Self {
            device: HidDevice::new(),
            control_cid: None,
            interrupt_cid: None,
            pending_input: Vec::new(),
            events: Vec::new(),
            interrupt_rx: Vec::new(),
        }
    }

    /// A keyboard: an eight-byte all-zero Input report 0, and an Output
    /// report 0 for the LEDs, both declared so a host can read and write
    /// them.
    pub fn keyboard() -> Self {
        let mut device = Self::new();
        device.put_report(report_type::INPUT, 0, KeyboardReport::default().to_bytes());
        device.put_report(report_type::OUTPUT, 0, vec![0u8]);
        device
    }

    /// Declares (or replaces) the report served for `(report_type, id)`.
    pub fn put_report(&mut self, report_type: u8, report_id: u8, data: impl Into<Vec<u8>>) {
        self.device.put_report(report_type, report_id, data);
    }

    /// The stored data for a declared report.
    pub fn report(&self, report_type: u8, report_id: u8) -> Option<&[u8]> {
        self.device.report(report_type, report_id)
    }

    /// The device's current protocol mode (`protocol_mode::*`).
    pub fn protocol_mode(&self) -> u8 {
        self.device.protocol_mode
    }

    /// Whether both channels are open — the only state in which this device
    /// is usable. Control alone can answer transactions; typing needs both.
    pub fn is_connected(&self) -> bool {
        self.control_cid.is_some() && self.interrupt_cid.is_some()
    }

    /// Queues one input report to go out on the interrupt channel.
    pub fn send_input_report(&mut self, payload: impl Into<Vec<u8>>) {
        self.pending_input
            .push(self.device.send_input_report(payload));
    }

    /// Queues a keyboard report.
    pub fn press(&mut self, report: KeyboardReport) {
        self.send_input_report(report.to_bytes().to_vec());
    }

    /// Queues a mouse report.
    pub fn move_pointer(&mut self, report: MouseReport) {
        self.send_input_report(report.to_bytes().to_vec());
    }

    /// The control transactions this device has answered, in order.
    pub fn events(&self) -> &[HidDeviceEvent] {
        &self.events
    }

    /// Output reports the host sent on the interrupt channel — a keyboard's
    /// LED state, typically.
    pub fn output_reports(&self) -> &[InterruptData] {
        &self.interrupt_rx
    }
}

impl ProtocolHandler for ClassicHidDevice {
    fn psm(&self) -> u16 {
        HID_CONTROL_PSM
    }

    fn psms(&self) -> Vec<u16> {
        vec![HID_CONTROL_PSM, HID_INTERRUPT_PSM]
    }

    /// Never called: this handler serves two PSMs, so every SDU is routed by
    /// channel through [`ProtocolHandler::on_channel_data`].
    fn on_data(&mut self, _data: &[u8], _peer_mtu: u16) -> Vec<Vec<u8>> {
        Vec::new()
    }

    fn on_channel_open(&mut self, channel: HandlerChannel) {
        match channel.psm {
            HID_CONTROL_PSM => self.control_cid = Some(channel.cid),
            HID_INTERRUPT_PSM => self.interrupt_cid = Some(channel.cid),
            _ => {}
        }
    }

    fn on_channel_lost(&mut self, cid: u16) {
        if self.control_cid == Some(cid) {
            self.control_cid = None;
        }
        if self.interrupt_cid == Some(cid) {
            self.interrupt_cid = None;
        }
    }

    fn on_channel_data(&mut self, channel: HandlerChannel, data: &[u8]) -> Vec<Vec<u8>> {
        // The channel is the whole meaning of the PDU here. A `DATA` header
        // on Control is a host-initiated report transfer; the identical
        // header on Interrupt is an output report. Same bytes, different
        // transaction.
        if channel.psm == HID_INTERRUPT_PSM {
            if let Ok(Some(report)) = receive_interrupt(data) {
                self.interrupt_rx.push(report);
            }
            return Vec::new();
        }
        match self.device.receive_control(data) {
            Ok((response, events)) => {
                self.events.extend(events);
                response.into_iter().collect()
            }
            // A malformed control PDU draws nothing: HIDP defines no
            // response to a PDU it cannot parse a header out of.
            Err(_) => Vec::new(),
        }
    }

    fn poll_channel_output(&mut self, channel: HandlerChannel) -> Vec<Vec<u8>> {
        if channel.psm != HID_INTERRUPT_PSM {
            return Vec::new();
        }
        std::mem::take(&mut self.pending_input)
    }

    fn on_channel_closed(&mut self) {
        // The link is gone. The declared reports are the *device* and stay;
        // the channels and anything queued for a departed host do not.
        self.control_cid = None;
        self.interrupt_cid = None;
        self.pending_input.clear();
    }
}

// ---------------------------------------------------------------------------
// Host role
// ---------------------------------------------------------------------------

/// One decoded thing a [`ClassicHidHost`] saw arrive on the interrupt
/// channel. Raw bytes are kept alongside so a caller can check the decoder
/// rather than trust it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HidInput {
    /// An eight-byte boot keyboard report.
    Keyboard(KeyboardReport),
    /// A four-byte boot mouse report.
    Mouse(MouseReport),
    /// A report neither decoder recognised, kept verbatim.
    Raw(Vec<u8>),
}

/// A Classic HID **host** — the computer. It opens both channels in the
/// order the profile requires and decodes what arrives.
#[derive(Debug)]
pub struct ClassicHidHost {
    host: HidHost,
    control_cid: Option<u16>,
    interrupt_cid: Option<u16>,
    /// PSMs still to ask the host for. Control goes first and Interrupt is
    /// only added once Control is open: HID Profile v1.1.1 §5.2.2 fixes the
    /// order, and a device is entitled to refuse an interrupt channel that
    /// arrives before its control channel.
    wanted_channels: Vec<u16>,
    control_out: Vec<Vec<u8>>,
    interrupt_out: Vec<Vec<u8>>,
    events: Vec<HidHostEvent>,
    input: Vec<HidInput>,
}

impl Default for ClassicHidHost {
    fn default() -> Self {
        Self::new()
    }
}

impl ClassicHidHost {
    /// A host that will open Control as soon as there is an ACL to open it
    /// on, and Interrupt as soon as Control is up.
    pub fn new() -> Self {
        Self {
            host: HidHost::new(),
            control_cid: None,
            interrupt_cid: None,
            wanted_channels: vec![HID_CONTROL_PSM],
            control_out: Vec::new(),
            interrupt_out: Vec::new(),
            events: Vec::new(),
            input: Vec::new(),
        }
    }

    /// Whether both channels are open.
    pub fn is_connected(&self) -> bool {
        self.control_cid.is_some() && self.interrupt_cid.is_some()
    }

    /// Queues a GET_REPORT transaction on the control channel.
    pub fn get_report(&mut self, report_type: u8, report_id: u8, buffer_size: Option<u16>) {
        self.control_out
            .push(self.host.get_report(report_type, report_id, buffer_size));
    }

    /// Queues a SET_REPORT transaction on the control channel.
    pub fn set_report(&mut self, report_type: u8, report_id: u8, data: &[u8]) {
        self.control_out
            .push(self.host.set_report(report_type, report_id, data));
    }

    /// Queues a SET_PROTOCOL transaction selecting `mode`.
    pub fn set_protocol(&mut self, mode: u8) {
        self.control_out.push(self.host.set_protocol(mode));
    }

    /// Queues a GET_PROTOCOL transaction.
    pub fn get_protocol(&mut self) {
        self.control_out.push(self.host.get_protocol());
    }

    /// Queues an output report on the *interrupt* channel — how a host
    /// drives a keyboard's LEDs without a round trip.
    pub fn send_output_report(&mut self, payload: impl Into<Vec<u8>>) {
        self.interrupt_out
            .push(self.host.send_output_report(payload));
    }

    /// Control-channel responses this host has seen, in order.
    pub fn events(&self) -> &[HidHostEvent] {
        &self.events
    }

    /// Decoded input reports, in the order they arrived.
    pub fn input(&self) -> &[HidInput] {
        &self.input
    }

    /// Every key usage newly pressed across the keyboard reports received,
    /// in order — what a user actually typed.
    pub fn typed_usages(&self) -> Vec<u8> {
        let mut previous = KeyboardReport::default();
        let mut typed = Vec::new();
        for report in self.input.iter().filter_map(|input| match input {
            HidInput::Keyboard(report) => Some(report),
            _ => None,
        }) {
            typed.extend(report.newly_pressed(&previous));
            previous = *report;
        }
        typed
    }
}

/// Decodes an input report by length. A boot keyboard report is eight bytes
/// and a boot mouse report four, which is the only discriminator available
/// without the SDP report descriptor this host does not fetch — stated
/// because it is a real limit: an eight-byte *mouse* report would be read as
/// a keyboard.
fn decode_input(payload: &[u8]) -> HidInput {
    if let Some(report) = KeyboardReport::parse(payload) {
        return HidInput::Keyboard(report);
    }
    if let Some(report) = MouseReport::parse(payload) {
        return HidInput::Mouse(report);
    }
    HidInput::Raw(payload.to_vec())
}

impl ProtocolHandler for ClassicHidHost {
    fn psm(&self) -> u16 {
        HID_CONTROL_PSM
    }

    fn psms(&self) -> Vec<u16> {
        vec![HID_CONTROL_PSM, HID_INTERRUPT_PSM]
    }

    /// Never called; see [`ClassicHidDevice::on_data`].
    fn on_data(&mut self, _data: &[u8], _peer_mtu: u16) -> Vec<Vec<u8>> {
        Vec::new()
    }

    fn poll_channel_requests(&mut self) -> Vec<u16> {
        std::mem::take(&mut self.wanted_channels)
    }

    fn on_channel_open(&mut self, channel: HandlerChannel) {
        match channel.psm {
            HID_CONTROL_PSM => {
                self.control_cid = Some(channel.cid);
                // Only now: the interrupt channel must not be opened first.
                self.wanted_channels.push(HID_INTERRUPT_PSM);
            }
            HID_INTERRUPT_PSM => self.interrupt_cid = Some(channel.cid),
            _ => {}
        }
    }

    fn on_channel_lost(&mut self, cid: u16) {
        if self.control_cid == Some(cid) {
            self.control_cid = None;
        }
        if self.interrupt_cid == Some(cid) {
            self.interrupt_cid = None;
        }
    }

    fn on_channel_data(&mut self, channel: HandlerChannel, data: &[u8]) -> Vec<Vec<u8>> {
        if channel.psm == HID_INTERRUPT_PSM {
            if let Ok(Some(report)) = receive_interrupt(data)
                && report.report_type == report_type::INPUT
            {
                self.input.push(decode_input(&report.payload));
            }
            return Vec::new();
        }
        if let Ok(events) = self.host.receive_control(data) {
            self.events.extend(events);
        }
        Vec::new()
    }

    fn poll_channel_output(&mut self, channel: HandlerChannel) -> Vec<Vec<u8>> {
        match channel.psm {
            HID_CONTROL_PSM => std::mem::take(&mut self.control_out),
            HID_INTERRUPT_PSM => std::mem::take(&mut self.interrupt_out),
            _ => Vec::new(),
        }
    }

    fn on_channel_closed(&mut self) {
        self.control_cid = None;
        self.interrupt_cid = None;
        self.control_out.clear();
        self.interrupt_out.clear();
        // A fresh link means the channels must be opened again, in order.
        self.wanted_channels = vec![HID_CONTROL_PSM];
    }
}
