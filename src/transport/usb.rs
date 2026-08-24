// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Real-hardware HCI transport driving a physical USB Bluetooth dongle, so a
//! Simble peripheral can be reached by real phones over real RF.
//!
//! USB Bluetooth controllers do NOT use H4 framing on the wire. The Bluetooth
//! USB transport layer (Core Spec Vol 4, Part B) instead maps each HCI packet
//! type to its own USB endpoint, and the packet type is implied by which
//! endpoint carried it:
//!
//! - **Commands**: control transfer OUT with `bmRequestType` 0x20 (Class
//!   request, Device recipient), `bRequest` 0, `wValue` 0, `wIndex` 0; the
//!   payload is the bare HCI command packet (Vol 4, Part B, Section 2.2).
//! - **Events**: the interrupt IN endpoint (usually address 0x81); each
//!   transfer carries one HCI event packet (Section 2.3).
//! - **ACL data**: bulk OUT (usually 0x02) and bulk IN (usually 0x82)
//!   endpoints (Section 2.4). An inbound ACL packet may span multiple USB
//!   transfers (its total size can exceed the transfer buffer, and a packet
//!   whose size is an exact multiple of the endpoint's max packet size only
//!   ends on a zero-length packet), so inbound bulk data is treated as a byte
//!   stream and reassembled using the ACL header's 16-bit length field.
//!
//! [`UsbTransport::pump`] bridges those endpoints to an
//! [`HciChannel`](super::HciChannel), stripping the H4 type byte from
//! host-to-controller packets to route them to the right endpoint and
//! restoring it on controller-to-host packets, so the rest of Simble sees the
//! same H4 packet flow as the `rootcanal`/`netsim` transports.
//!
//! USB access uses the `nusb` crate — pure Rust with no libusb/C dependency
//! (IOKit backend on macOS, usbfs on Linux) — rather than `rusb`. nusb 0.2's
//! IO entry points return `MaybeFuture` values that run synchronously via
//! `.wait()`, and its endpoints support submitting transfers up front and
//! polling for completions with a zero timeout, so it fits Simble's
//! synchronous, non-blocking `pump` design with no async runtime.

use crate::transport::h4_type;
use crate::types::SimbleError;
use nusb::MaybeFuture;
use nusb::transfer::{
    Buffer, Bulk, BulkOrInterrupt, ControlOut, ControlType, In, Interrupt, Out, Recipient,
};
use std::time::Duration;

/// Class/subclass/protocol triple identifying a Bluetooth HCI controller:
/// Wireless Controller / RF Controller / Bluetooth Primary Controller
/// (Vol 4, Part B, Section 3.1's required device descriptor values).
const BT_HCI_CLASS_TRIPLE: (u8, u8, u8) = (0xE0, 0x01, 0x01);

/// USB endpoint descriptor `bmAttributes` transfer-type field (USB 2.0
/// Section 9.6.6): the low two bits select the transfer type.
const TRANSFER_TYPE_MASK: u8 = 0x03;
const TRANSFER_TYPE_BULK: u8 = 0x02;
const TRANSFER_TYPE_INTERRUPT: u8 = 0x03;

/// USB endpoint address direction bit (USB 2.0 Section 9.6.6).
const ENDPOINT_IN: u8 = 0x80;

/// How many IN transfers to keep submitted per IN endpoint, so the host
/// controller driver always has a buffer ready when the dongle has data —
/// with only one in flight, a packet arriving between completion and
/// resubmission could be NAKed and delayed by a full polling interval.
const IN_FLIGHT_TRANSFERS: usize = 2;

/// Largest HCI event packet: 2-byte header + 255-byte max parameter length
/// (Vol 4, Part E, Section 5.4.4), so one interrupt IN transfer of at least
/// this size always fits a complete event.
const MAX_EVENT_PACKET: usize = 2 + 255;

/// Inbound bulk transfer size target. Not a protocol limit — ACL packets
/// larger than one transfer are reassembled across transfers — just large
/// enough that typical controller ACL buffers (~1021 bytes) arrive in one.
const ACL_TRANSFER_TARGET: usize = 4096;

/// Blocking-transfer timeout for outbound commands and ACL data. OUT
/// transfers to a healthy dongle complete in microseconds; this bounds how
/// long `pump` can stall on hardware that has wedged.
const OUT_TIMEOUT: Duration = Duration::from_secs(1);

/// Whether a USB device is a Bluetooth HCI controller, judged from
/// descriptor-shaped data: the device-level class triple, or any
/// interface-level triple. Both levels must be checked because dongles vary —
/// single-function dongles typically declare E0/01/01 at the device level,
/// while composite devices declare a generic device class and put E0/01/01 on
/// the HCI interface (same dual check as Bumble's transport/usb.py).
fn is_bluetooth_hci(device: (u8, u8, u8), interfaces: &[(u8, u8, u8)]) -> bool {
    device == BT_HCI_CLASS_TRIPLE || interfaces.contains(&BT_HCI_CLASS_TRIPLE)
}

/// The three endpoint addresses the HCI transport needs on interface 0
/// (Vol 4, Part B, Section 2.1.1: interrupt IN for events, bulk IN/OUT for
/// ACL data; SCO's isochronous endpoints live on interface 1 and are not
/// claimed here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EndpointAddresses {
    event_in: u8,
    acl_in: u8,
    acl_out: u8,
}

/// Selects the interrupt IN / bulk IN / bulk OUT endpoint addresses from
/// `(bEndpointAddress, bmAttributes)` descriptor pairs, taking the first of
/// each kind (the spec fixes one of each on interface 0; scanning rather
/// than hardcoding 0x81/0x82/0x02 tolerates dongles that number differently).
fn select_endpoints(endpoints: &[(u8, u8)]) -> Result<EndpointAddresses, SimbleError> {
    let mut event_in = None;
    let mut acl_in = None;
    let mut acl_out = None;
    for &(address, attributes) in endpoints {
        let is_in = address & ENDPOINT_IN != 0;
        match attributes & TRANSFER_TYPE_MASK {
            TRANSFER_TYPE_INTERRUPT if is_in => event_in.get_or_insert(address),
            TRANSFER_TYPE_BULK if is_in => acl_in.get_or_insert(address),
            TRANSFER_TYPE_BULK => acl_out.get_or_insert(address),
            _ => continue,
        };
    }
    match (event_in, acl_in, acl_out) {
        (Some(event_in), Some(acl_in), Some(acl_out)) => Ok(EndpointAddresses {
            event_in,
            acl_in,
            acl_out,
        }),
        _ => Err(SimbleError::Transport(format!(
            "USB interface 0 lacks the HCI endpoint set (interrupt IN: {}, bulk IN: {}, bulk OUT: {})",
            event_in.is_some(),
            acl_in.is_some(),
            acl_out.is_some()
        ))),
    }
}

/// Parses a `"vid:pid"` device selector (hex, e.g. `"0a12:0001"`) for
/// explicitly choosing a dongle instead of probing by device class.
pub fn parse_vid_pid(spec: &str) -> Result<(u16, u16), SimbleError> {
    let err = || {
        SimbleError::Transport(format!(
            "invalid vid:pid selector {spec:?} (expected hex, e.g. 0a12:0001)"
        ))
    };
    let (vid, pid) = spec.split_once(':').ok_or_else(err)?;
    Ok((
        u16::from_str_radix(vid, 16).map_err(|_| err())?,
        u16::from_str_radix(pid, 16).map_err(|_| err())?,
    ))
}

/// IN transfers must request a nonzero multiple of the endpoint's max packet
/// size (a USB transfer only ends on a short packet or when the requested
/// length fills), so round the desired size up to one.
fn in_transfer_len(max_packet_size: usize, desired: usize) -> usize {
    max_packet_size * desired.max(1).div_ceil(max_packet_size)
}

/// Incrementally reassembles complete ACL packets from the inbound bulk byte
/// stream. Unlike `rootcanal::H4FrameReader`, the stream carries no H4 type
/// bytes — every byte belongs to a bare ACL packet (the endpoint itself
/// implies the type), so only the ACL header's length field (Vol 4, Part E,
/// Section 5.4.2: 2-byte handle+flags, then a 16-bit little-endian data
/// length) marks packet boundaries. Emits H4-framed packets, type byte
/// restored, ready for `HciChannel::receive_from_controller`.
#[derive(Debug, Default)]
struct AclInReassembler {
    buffer: Vec<u8>,
}

/// ACL data packet header length: 2-byte handle/flags + 2-byte data length
/// (Vol 4, Part E, Section 5.4.2).
const ACL_HEADER_LEN: usize = 4;

impl AclInReassembler {
    fn feed(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    fn next_packet(&mut self) -> Option<Vec<u8>> {
        if self.buffer.len() < ACL_HEADER_LEN {
            return None;
        }
        let payload_len = u16::from_le_bytes([self.buffer[2], self.buffer[3]]) as usize;
        let total_len = ACL_HEADER_LEN + payload_len;
        if self.buffer.len() < total_len {
            return None;
        }
        let mut packet = Vec::with_capacity(1 + total_len);
        packet.push(h4_type::HCI_ACL_DATA);
        packet.extend_from_slice(&self.buffer[..total_len]);
        self.buffer.drain(..total_len);
        Some(packet)
    }
}

/// The endpoint-level I/O surface of the transport, factored out so the
/// packet routing and reassembly logic above it is unit-testable without
/// hardware (mock impl in the tests, nusb-backed [`NusbEndpoints`] for real
/// dongles). Payloads are bare HCI packets — no H4 type byte on either side.
pub(crate) trait UsbEndpoints {
    /// Sends one HCI command packet over the control endpoint.
    fn send_command(&mut self, cmd: &[u8]) -> Result<(), SimbleError>;
    /// Sends one HCI ACL data packet over the bulk OUT endpoint.
    fn send_acl(&mut self, acl: &[u8]) -> Result<(), SimbleError>;
    /// Returns one complete HCI event packet if one has arrived (non-blocking).
    fn try_recv_event(&mut self) -> Result<Option<Vec<u8>>, SimbleError>;
    /// Returns the next chunk of raw inbound bulk bytes if any have arrived
    /// (non-blocking). Chunks are transfer-sized fragments, not necessarily
    /// whole ACL packets — the caller reassembles.
    fn try_recv_acl(&mut self) -> Result<Option<Vec<u8>>, SimbleError>;
}

/// The real-hardware [`UsbEndpoints`]: nusb endpoints on interface 0 of the
/// dongle. Keeps [`IN_FLIGHT_TRANSFERS`] IN transfers submitted on each IN
/// endpoint and polls completions with a zero timeout, so `pump` never blocks
/// waiting for the controller.
struct NusbEndpoints {
    interface: nusb::Interface,
    event_in: nusb::Endpoint<Interrupt, In>,
    acl_in: nusb::Endpoint<Bulk, In>,
    acl_out: nusb::Endpoint<Bulk, Out>,
    event_transfer_len: usize,
    acl_transfer_len: usize,
}

/// Polls one IN endpoint for a completed transfer without blocking, keeping
/// its queue of submitted transfers full. A zero-length completion (the
/// zero-length packet terminating a transfer whose payload was an exact
/// multiple of the max packet size, Vol 4, Part B, Section 2.4) is skipped.
fn try_recv_in<T: BulkOrInterrupt>(
    endpoint: &mut nusb::Endpoint<T, In>,
    transfer_len: usize,
) -> Result<Option<Vec<u8>>, SimbleError> {
    loop {
        while endpoint.pending() < IN_FLIGHT_TRANSFERS {
            endpoint.submit(Buffer::new(transfer_len));
        }
        let Some(completion) = endpoint.wait_next_complete(Duration::ZERO) else {
            return Ok(None);
        };
        let actual_len = completion.actual_len;
        completion
            .status
            .map_err(|e| SimbleError::Transport(format!("USB IN transfer failed: {e}")))?;
        let mut data = completion.buffer.into_vec();
        data.truncate(actual_len);
        if !data.is_empty() {
            return Ok(Some(data));
        }
    }
}

impl UsbEndpoints for NusbEndpoints {
    fn send_command(&mut self, cmd: &[u8]) -> Result<(), SimbleError> {
        // Vol 4, Part B, Section 2.2: bmRequestType 0x20 (Class, Device
        // recipient), bRequest/wValue/wIndex all zero, payload = HCI command.
        self.interface
            .control_out(
                ControlOut {
                    control_type: ControlType::Class,
                    recipient: Recipient::Device,
                    request: 0,
                    value: 0,
                    index: 0,
                    data: cmd,
                },
                OUT_TIMEOUT,
            )
            .wait()
            .map_err(|e| SimbleError::Transport(format!("USB HCI command transfer failed: {e}")))
    }

    fn send_acl(&mut self, acl: &[u8]) -> Result<(), SimbleError> {
        let mut buffer = self.acl_out.allocate(acl.len());
        buffer.extend_from_slice(acl);
        self.acl_out
            .transfer_blocking(buffer, OUT_TIMEOUT)
            .status
            .map_err(|e| SimbleError::Transport(format!("USB ACL OUT transfer failed: {e}")))
    }

    fn try_recv_event(&mut self) -> Result<Option<Vec<u8>>, SimbleError> {
        try_recv_in(&mut self.event_in, self.event_transfer_len)
    }

    fn try_recv_acl(&mut self) -> Result<Option<Vec<u8>>, SimbleError> {
        try_recv_in(&mut self.acl_in, self.acl_transfer_len)
    }
}

/// Bidirectional HCI transport to a physical USB Bluetooth dongle, exposing
/// the same [`pump`](Self::pump) contract as `RootcanalTransport` and
/// `NetsimTransport` so callers can swap a simulated controller for real
/// hardware without touching the rest of the stack.
pub struct UsbTransport {
    endpoints: Box<dyn UsbEndpoints>,
    acl_reassembler: AclInReassembler,
}

impl UsbTransport {
    /// Opens the first USB device whose descriptors identify it as a
    /// Bluetooth HCI controller (Wireless Controller / RF / Bluetooth class).
    pub fn open_first() -> Result<Self, SimbleError> {
        let info = list_usb_devices()?
            .into_iter()
            .find(|d| {
                let interfaces: Vec<_> = d
                    .interfaces()
                    .map(|i| (i.class(), i.subclass(), i.protocol()))
                    .collect();
                is_bluetooth_hci((d.class(), d.subclass(), d.protocol()), &interfaces)
            })
            .ok_or_else(|| {
                SimbleError::Transport(
                    "no Bluetooth-class USB device found (class E0/01/01 at device or interface level)"
                        .to_string(),
                )
            })?;
        Self::open_device(&info)
    }

    /// Opens the USB device with the given vendor/product ID, for dongles
    /// that hide behind vendor-specific class codes (see also
    /// [`parse_vid_pid`] for the `"vid:pid"` string form).
    pub fn open(vid: u16, pid: u16) -> Result<Self, SimbleError> {
        let info = list_usb_devices()?
            .into_iter()
            .find(|d| d.vendor_id() == vid && d.product_id() == pid)
            .ok_or_else(|| {
                SimbleError::Transport(format!("USB device {vid:04x}:{pid:04x} not found"))
            })?;
        Self::open_device(&info)
    }

    fn open_device(info: &nusb::DeviceInfo) -> Result<Self, SimbleError> {
        let vid = info.vendor_id();
        let pid = info.product_id();
        let device = info.open().wait().map_err(|e| {
            SimbleError::Transport(format!(
                "opening USB device {vid:04x}:{pid:04x} failed: {e} \
                 (is the dongle claimed by the OS Bluetooth stack, or missing usbfs permissions on Linux?)"
            ))
        })?;
        // A device with no active configuration has no interfaces to claim, and
        // the error for that reads like the dongle is the wrong kind:
        // "interface not found". On Linux the kernel's btusb driver configures
        // a Bluetooth dongle as it enumerates, so this never comes up. On
        // macOS nothing claims a generic dongle — `ioreg -p IOUSB` shows the
        // device with no IOUSBHostInterface children at all — so it stays
        // unconfigured until someone asks. Select the first configuration the
        // device advertises; Bluetooth dongles have exactly one.
        if device.active_configuration().is_err() {
            let first = device
                .configurations()
                .next()
                .map(|c| c.configuration_value())
                .ok_or_else(|| {
                    SimbleError::Transport(format!(
                        "USB device {vid:04x}:{pid:04x} advertises no configuration"
                    ))
                })?;
            device.set_configuration(first).wait().map_err(|e| {
                SimbleError::Transport(format!(
                    "setting configuration {first} on {vid:04x}:{pid:04x} failed: {e}"
                ))
            })?;
        }
        // detach_and_claim_interface rather than claim_interface: on Linux
        // the kernel's btusb driver typically owns a new dongle and must be
        // detached first; on other platforms it degrades to a plain claim.
        let interface = device.detach_and_claim_interface(0).wait().map_err(|e| {
            SimbleError::Transport(format!(
                "claiming interface 0 of {vid:04x}:{pid:04x} failed: {e}"
            ))
        })?;

        let descriptor = interface.descriptor().ok_or_else(|| {
            SimbleError::Transport("USB interface 0 has no descriptor".to_string())
        })?;
        let endpoint_info: Vec<(u8, u8)> = descriptor
            .endpoints()
            .map(|e| (e.address(), e.attributes()))
            .collect();
        let max_packet_size = |address: u8| {
            descriptor
                .endpoints()
                .find(|e| e.address() == address)
                .map(|e| e.max_packet_size())
                .unwrap_or(64)
        };
        let addresses = select_endpoints(&endpoint_info)?;

        let event_transfer_len =
            in_transfer_len(max_packet_size(addresses.event_in), MAX_EVENT_PACKET);
        let acl_transfer_len =
            in_transfer_len(max_packet_size(addresses.acl_in), ACL_TRANSFER_TARGET);
        let claim = |what: &str, e: nusb::Error| {
            SimbleError::Transport(format!("claiming {what} endpoint failed: {e}"))
        };
        let event_in = interface
            .endpoint::<Interrupt, In>(addresses.event_in)
            .map_err(|e| claim("interrupt IN", e))?;
        let acl_in = interface
            .endpoint::<Bulk, In>(addresses.acl_in)
            .map_err(|e| claim("bulk IN", e))?;
        let acl_out = interface
            .endpoint::<Bulk, Out>(addresses.acl_out)
            .map_err(|e| claim("bulk OUT", e))?;

        Ok(Self::with_endpoints(Box::new(NusbEndpoints {
            interface,
            event_in,
            acl_in,
            acl_out,
            event_transfer_len,
            acl_transfer_len,
        })))
    }

    fn with_endpoints(endpoints: Box<dyn UsbEndpoints>) -> Self {
        Self {
            endpoints,
            acl_reassembler: AclInReassembler::default(),
        }
    }

    /// Moves packets in both directions between the dongle and `channel`,
    /// mirroring the `rootcanal`/`netsim` pump contract: drains every
    /// H4-framed packet `channel` has queued for the controller, strips the
    /// type byte, and routes commands to the control endpoint and ACL data
    /// to bulk OUT; then polls the interrupt and bulk IN endpoints without
    /// blocking, restores the H4 type byte (reassembling fragmented ACL
    /// packets), and hands complete packets to `channel`.
    pub fn pump(&mut self, channel: &super::HciChannel) -> Result<(), SimbleError> {
        while let Some(packet) = channel.poll_host_packet() {
            let Some((&packet_type, payload)) = packet.split_first() else {
                return Err(SimbleError::Transport("empty H4 packet".to_string()));
            };
            match packet_type {
                h4_type::HCI_COMMAND => self.endpoints.send_command(payload)?,
                h4_type::HCI_ACL_DATA => self.endpoints.send_acl(payload)?,
                // SCO/ISO would need interface 1's isochronous endpoints,
                // which this transport does not claim.
                other => {
                    return Err(SimbleError::Transport(format!(
                        "H4 packet type {other:#04x} is not supported over the USB transport"
                    )));
                }
            }
        }

        while let Some(event) = self.endpoints.try_recv_event()? {
            let mut packet = Vec::with_capacity(1 + event.len());
            packet.push(h4_type::HCI_EVENT);
            packet.extend_from_slice(&event);
            channel.receive_from_controller(packet)?;
        }

        while let Some(chunk) = self.endpoints.try_recv_acl()? {
            self.acl_reassembler.feed(&chunk);
            while let Some(packet) = self.acl_reassembler.next_packet() {
                channel.receive_from_controller(packet)?;
            }
        }
        Ok(())
    }
}

// --- Scene over a physical dongle -------------------------------------------

/// A live scene whose backend is a
/// real USB dongle: the scripted peripheral's host logic runs in this process
/// and the dongle is its controller, so a phone in the room sees a real
/// advertiser on real RF.
///
/// The netsim counterpart (`NetsimScene`)
/// gives every peripheral its own transport, because netsimd's ether can hold
/// any number of them. **A dongle is exactly one controller**, so this scene
/// holds exactly one device — which is why the tool that selects it also lets
/// you say *which* dongle. Peripheral-only, like every live backend: the
/// central is a real phone or laptop, not something this process hosts.
///
/// No USB access happens until the first peripheral is added, so selecting
/// this backend works (and is testable) on a machine with no dongle plugged
/// in; the "is one plugged in" failure surfaces where it means something.
pub struct UsbScene {
    /// The `vid:pid` to open, or `None` to take the first Bluetooth-class
    /// device found.
    device: Option<(u16, u16)>,
    scene: crate::transport::live_scene::LiveScene<UsbTransport>,
}

impl UsbScene {
    /// Creates an empty dongle-backed scene. `device` is the `vid:pid` pair
    /// from [`parse_vid_pid`], or `None` to auto-detect.
    pub fn new(device: Option<(u16, u16)>) -> Self {
        Self {
            device,
            scene: crate::transport::live_scene::LiveScene::new(),
        }
    }

    /// How the dongle will be chosen, for reporting back to whoever selected
    /// this backend before anything has been opened.
    pub fn selector(&self) -> String {
        match self.device {
            Some((vid, pid)) => format!("{vid:04x}:{pid:04x}"),
            None => "the first Bluetooth-class dongle found".to_string(),
        }
    }

    /// Runs `script` and puts the resulting peripheral on the dongle, opening
    /// it on this first call. Rejects a second device: one controller, one
    /// device.
    pub fn add_peripheral(
        &mut self,
        address: crate::types::Address,
        script: &str,
    ) -> Result<usize, String> {
        if self.scene.device_count() > 0 {
            return Err(
                "a USB dongle is one controller and already hosts a device — \
                        run_on(\"usb\") again to start over, or run_on(\"self\") for a \
                        scene with several devices"
                    .to_string(),
            );
        }
        let device = self.device;
        self.scene.add_peripheral(address, script, |_peripheral| {
            match device {
                Some((vid, pid)) => UsbTransport::open(vid, pid),
                None => UsbTransport::open_first(),
            }
            .map_err(|e| {
                format!(
                    "{e} — is a Bluetooth dongle plugged in and free? \
                     (the OS Bluetooth stack usually claims the built-in adapter)"
                )
            })
        })
    }

    /// Moves packets for the device without advancing the script clock (the
    /// shared `LiveScene::pump`).
    pub fn pump(&mut self) {
        self.scene.pump();
    }

    /// Advances the script clock by `seconds`, then pumps (the shared
    /// `LiveScene::tick`).
    pub fn tick(&mut self, seconds: f64) {
        self.scene.tick(seconds);
    }

    /// The current script-clock time in seconds.
    pub fn now(&self) -> f64 {
        self.scene.now()
    }

    /// The number of peripherals in the scene (0 or 1).
    pub fn device_count(&self) -> usize {
        self.scene.device_count()
    }

    /// The GATT status JSON of peripheral `index`, or `None` for an unknown
    /// index.
    pub fn peripheral_status_json(&self, index: usize) -> Option<String> {
        self.scene.peripheral_status_json(index)
    }
}

fn list_usb_devices() -> Result<Vec<nusb::DeviceInfo>, SimbleError> {
    Ok(nusb::list_devices()
        .wait()
        .map_err(|e| SimbleError::Transport(format!("USB device enumeration failed: {e}")))?
        .collect())
}

#[cfg(test)]
#[path = "usb_tests.rs"]
mod tests;
