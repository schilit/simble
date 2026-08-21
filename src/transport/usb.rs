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

fn list_usb_devices() -> Result<Vec<nusb::DeviceInfo>, SimbleError> {
    Ok(nusb::list_devices()
        .wait()
        .map_err(|e| SimbleError::Transport(format!("USB device enumeration failed: {e}")))?
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::HciChannel;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    /// Shared recorder/scripter behind the mock, the `DuplexMock` philosophy
    /// of the sibling transports' tests: outbound packets are recorded,
    /// inbound completions are scripted per endpoint.
    #[derive(Default)]
    struct MockState {
        commands: Vec<Vec<u8>>,
        acl_out: Vec<Vec<u8>>,
        events_in: VecDeque<Vec<u8>>,
        acl_in: VecDeque<Vec<u8>>,
        fail_event_recv: bool,
        fail_command_send: bool,
    }

    struct MockEndpoints(Rc<RefCell<MockState>>);

    impl UsbEndpoints for MockEndpoints {
        fn send_command(&mut self, cmd: &[u8]) -> Result<(), SimbleError> {
            let mut state = self.0.borrow_mut();
            if state.fail_command_send {
                return Err(SimbleError::Transport("mock command failure".to_string()));
            }
            state.commands.push(cmd.to_vec());
            Ok(())
        }
        fn send_acl(&mut self, acl: &[u8]) -> Result<(), SimbleError> {
            self.0.borrow_mut().acl_out.push(acl.to_vec());
            Ok(())
        }
        fn try_recv_event(&mut self) -> Result<Option<Vec<u8>>, SimbleError> {
            let mut state = self.0.borrow_mut();
            if state.fail_event_recv {
                return Err(SimbleError::Transport("mock event failure".to_string()));
            }
            Ok(state.events_in.pop_front())
        }
        fn try_recv_acl(&mut self) -> Result<Option<Vec<u8>>, SimbleError> {
            Ok(self.0.borrow_mut().acl_in.pop_front())
        }
    }

    fn mock_transport() -> (UsbTransport, Rc<RefCell<MockState>>) {
        let state = Rc::new(RefCell::new(MockState::default()));
        let transport = UsbTransport::with_endpoints(Box::new(MockEndpoints(state.clone())));
        (transport, state)
    }

    #[test]
    fn test_pump_routes_command_to_control_endpoint_without_h4_byte() {
        let (mut transport, state) = mock_transport();
        let channel = HciChannel::new();
        channel.send_command(&[0x03, 0x0C, 0x00]).unwrap();

        transport.pump(&channel).unwrap();

        assert_eq!(state.borrow().commands, vec![vec![0x03, 0x0C, 0x00]]);
        assert!(state.borrow().acl_out.is_empty());
    }

    #[test]
    fn test_pump_routes_acl_to_bulk_out_without_h4_byte() {
        let (mut transport, state) = mock_transport();
        let channel = HciChannel::new();
        channel
            .send_acl_data(&[0x40, 0x00, 0x02, 0x00, 0xAA, 0xBB])
            .unwrap();

        transport.pump(&channel).unwrap();

        assert_eq!(
            state.borrow().acl_out,
            vec![vec![0x40, 0x00, 0x02, 0x00, 0xAA, 0xBB]]
        );
        assert!(state.borrow().commands.is_empty());
    }

    #[test]
    fn test_pump_rejects_sco_and_unknown_h4_types() {
        for bad_type in [h4_type::HCI_SCO_DATA, h4_type::HCI_ISO_DATA, 0xFF] {
            let (mut transport, _state) = mock_transport();
            let channel = HciChannel::new();
            channel
                .host_to_ctrl_tx
                .send(vec![bad_type, 0x00, 0x00])
                .unwrap();
            assert!(matches!(
                transport.pump(&channel),
                Err(SimbleError::Transport(_))
            ));
        }
    }

    #[test]
    fn test_pump_passes_events_through_with_h4_byte_restored() {
        let (mut transport, state) = mock_transport();
        // Command Complete for Reset.
        let event = vec![0x0E, 0x04, 0x01, 0x03, 0x0C, 0x00];
        state.borrow_mut().events_in.push_back(event.clone());
        let channel = HciChannel::new();

        transport.pump(&channel).unwrap();

        let mut expected = vec![h4_type::HCI_EVENT];
        expected.extend_from_slice(&event);
        assert_eq!(channel.poll_controller_packet().unwrap(), expected);
        assert!(channel.poll_controller_packet().is_none());
    }

    #[test]
    fn test_pump_reassembles_acl_fragmented_across_transfers() {
        let (mut transport, state) = mock_transport();
        // One 8-byte-payload ACL packet split mid-header and mid-payload.
        let acl = [0x40, 0x00, 0x08, 0x00, 1, 2, 3, 4, 5, 6, 7, 8];
        {
            let mut s = state.borrow_mut();
            s.acl_in.push_back(acl[..3].to_vec());
            s.acl_in.push_back(acl[3..9].to_vec());
            s.acl_in.push_back(acl[9..].to_vec());
        }
        let channel = HciChannel::new();

        transport.pump(&channel).unwrap();

        let mut expected = vec![h4_type::HCI_ACL_DATA];
        expected.extend_from_slice(&acl);
        assert_eq!(channel.poll_controller_packet().unwrap(), expected);
        assert!(channel.poll_controller_packet().is_none());
    }

    #[test]
    fn test_pump_splits_two_acl_packets_arriving_in_one_transfer() {
        let (mut transport, state) = mock_transport();
        let first = [0x40, 0x00, 0x02, 0x00, 0xAA, 0xBB];
        let second = [0x41, 0x00, 0x01, 0x00, 0xCC];
        let mut combined = first.to_vec();
        combined.extend_from_slice(&second);
        state.borrow_mut().acl_in.push_back(combined);
        let channel = HciChannel::new();

        transport.pump(&channel).unwrap();

        let mut expected_first = vec![h4_type::HCI_ACL_DATA];
        expected_first.extend_from_slice(&first);
        let mut expected_second = vec![h4_type::HCI_ACL_DATA];
        expected_second.extend_from_slice(&second);
        assert_eq!(channel.poll_controller_packet().unwrap(), expected_first);
        assert_eq!(channel.poll_controller_packet().unwrap(), expected_second);
    }

    #[test]
    fn test_partial_acl_packet_is_held_until_completed_on_a_later_pump() {
        let (mut transport, state) = mock_transport();
        let acl = [0x40, 0x00, 0x03, 0x00, 0x01, 0x02, 0x03];
        state.borrow_mut().acl_in.push_back(acl[..5].to_vec());
        let channel = HciChannel::new();

        transport.pump(&channel).unwrap();
        assert!(channel.poll_controller_packet().is_none());

        state.borrow_mut().acl_in.push_back(acl[5..].to_vec());
        transport.pump(&channel).unwrap();

        let mut expected = vec![h4_type::HCI_ACL_DATA];
        expected.extend_from_slice(&acl);
        assert_eq!(channel.poll_controller_packet().unwrap(), expected);
    }

    #[test]
    fn test_pump_propagates_endpoint_errors() {
        let (mut transport, state) = mock_transport();
        state.borrow_mut().fail_event_recv = true;
        let channel = HciChannel::new();
        assert!(matches!(
            transport.pump(&channel),
            Err(SimbleError::Transport(_))
        ));

        let (mut transport, state) = mock_transport();
        state.borrow_mut().fail_command_send = true;
        let channel = HciChannel::new();
        channel.send_command(&[0x03, 0x0C, 0x00]).unwrap();
        assert!(matches!(
            transport.pump(&channel),
            Err(SimbleError::Transport(_))
        ));
    }

    #[test]
    fn test_is_bluetooth_hci_matches_device_level_class() {
        assert!(is_bluetooth_hci((0xE0, 0x01, 0x01), &[]));
    }

    #[test]
    fn test_is_bluetooth_hci_matches_interface_level_class_on_composite_device() {
        // Composite dongle: generic device class, HCI declared per-interface.
        assert!(is_bluetooth_hci(
            (0x00, 0x00, 0x00),
            &[(0xFF, 0x00, 0x00), (0xE0, 0x01, 0x01)]
        ));
    }

    #[test]
    fn test_is_bluetooth_hci_rejects_non_bluetooth_devices() {
        assert!(!is_bluetooth_hci((0x03, 0x01, 0x01), &[(0x03, 0x01, 0x01)]));
        assert!(!is_bluetooth_hci((0xE0, 0x01, 0x03), &[])); // AMP controller, not primary
        assert!(!is_bluetooth_hci((0x00, 0x00, 0x00), &[]));
    }

    #[test]
    fn test_select_endpoints_finds_standard_hci_layout() {
        // Typical dongle interface 0: interrupt IN 0x81, bulk IN 0x82,
        // bulk OUT 0x02, plus an unrelated isochronous pair to ignore.
        let endpoints = [
            (0x81, TRANSFER_TYPE_INTERRUPT),
            (0x82, TRANSFER_TYPE_BULK),
            (0x02, TRANSFER_TYPE_BULK),
            (0x83, 0x01),
            (0x03, 0x01),
        ];
        assert_eq!(
            select_endpoints(&endpoints).unwrap(),
            EndpointAddresses {
                event_in: 0x81,
                acl_in: 0x82,
                acl_out: 0x02
            }
        );
    }

    #[test]
    fn test_select_endpoints_tolerates_nonstandard_addresses() {
        let endpoints = [
            (0x02 | ENDPOINT_IN, TRANSFER_TYPE_INTERRUPT),
            (0x01 | ENDPOINT_IN, TRANSFER_TYPE_BULK),
            (0x01, TRANSFER_TYPE_BULK),
        ];
        assert_eq!(
            select_endpoints(&endpoints).unwrap(),
            EndpointAddresses {
                event_in: 0x82,
                acl_in: 0x81,
                acl_out: 0x01
            }
        );
    }

    #[test]
    fn test_select_endpoints_errors_when_incomplete() {
        let missing_bulk_out = [(0x81, TRANSFER_TYPE_INTERRUPT), (0x82, TRANSFER_TYPE_BULK)];
        assert!(matches!(
            select_endpoints(&missing_bulk_out),
            Err(SimbleError::Transport(_))
        ));
        assert!(select_endpoints(&[]).is_err());
    }

    #[test]
    fn test_parse_vid_pid() {
        assert_eq!(parse_vid_pid("0a12:0001").unwrap(), (0x0A12, 0x0001));
        assert_eq!(parse_vid_pid("8087:0025").unwrap(), (0x8087, 0x0025));
        assert!(parse_vid_pid("0a120001").is_err());
        assert!(parse_vid_pid("zzzz:0001").is_err());
        assert!(parse_vid_pid("0a12:").is_err());
    }

    #[test]
    fn test_in_transfer_len_rounds_up_to_max_packet_size_multiple() {
        assert_eq!(in_transfer_len(16, 257), 272);
        assert_eq!(in_transfer_len(64, 4096), 4096);
        assert_eq!(in_transfer_len(512, 4096), 4096);
        assert_eq!(in_transfer_len(64, 1), 64);
        assert_eq!(in_transfer_len(64, 0), 64);
    }

    #[test]
    fn test_acl_reassembler_handles_byte_at_a_time_delivery() {
        let acl = [0x40, 0x00, 0x02, 0x00, 0xAA, 0xBB];
        let mut reassembler = AclInReassembler::default();
        for &byte in &acl[..acl.len() - 1] {
            reassembler.feed(&[byte]);
            assert_eq!(reassembler.next_packet(), None);
        }
        reassembler.feed(&acl[acl.len() - 1..]);
        let mut expected = vec![h4_type::HCI_ACL_DATA];
        expected.extend_from_slice(&acl);
        assert_eq!(reassembler.next_packet().unwrap(), expected);
    }

    #[test]
    fn test_acl_reassembler_handles_zero_length_payload() {
        let acl = [0x40, 0x00, 0x00, 0x00];
        let mut reassembler = AclInReassembler::default();
        reassembler.feed(&acl);
        assert_eq!(
            reassembler.next_packet().unwrap(),
            vec![h4_type::HCI_ACL_DATA, 0x40, 0x00, 0x00, 0x00]
        );
        assert_eq!(reassembler.next_packet(), None);
    }
}
