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
use crate::transport::hci_adapter::CommandCredits;
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

// --- Choosing one dongle out of several -------------------------------------

/// One USB device presenting a Bluetooth HCI controller, as
/// [`list_bluetooth_dongles`] found it. Everything here comes from the device
/// descriptor and the platform's enumeration — nothing is opened, so listing
/// works even for a dongle another process holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbDongle {
    /// Position in [`list_bluetooth_dongles`]'s order, which is sorted by
    /// bus then address so it is the same for every caller in a session.
    pub index: usize,
    /// Platform bus identifier (`"02"` on macOS, `"001"` on Linux).
    pub bus_id: String,
    /// Address assigned to the device on that bus at enumeration time. Fresh
    /// on every re-plug — see [`UsbSelector`] on what that costs.
    pub device_address: u8,
    /// Hub port numbers from the root hub down to this device. Together with
    /// `bus_id` this names a *physical socket*, and unlike `device_address`
    /// nusb documents it as stable across re-plugs and reboots.
    pub port_chain: Vec<u8>,
    /// `idVendor`.
    pub vendor_id: u16,
    /// `idProduct`.
    pub product_id: u16,
    /// `iManufacturer`, when the dongle publishes one.
    pub manufacturer: Option<String>,
    /// `iProduct`, when the dongle publishes one.
    pub product: Option<String>,
    /// `iSerialNumber`, when the dongle publishes one. Absent on the cheap
    /// CSR clones, which is why serial numbers cannot be the selector.
    pub serial_number: Option<String>,
}

impl UsbDongle {
    /// The `bus/address` selector naming exactly this device right now.
    pub fn address_selector(&self) -> String {
        format!("{}/{}", self.bus_id, self.device_address)
    }

    /// The `bus.port.port` selector naming the socket this device is in.
    pub fn port_selector(&self) -> String {
        let ports: Vec<String> = self.port_chain.iter().map(u8::to_string).collect();
        format!("{}.{}", self.bus_id, ports.join("."))
    }

    /// One line for a chooser: every way to name it, and what it says it is.
    pub fn describe(&self) -> String {
        let name = match (&self.manufacturer, &self.product) {
            (Some(m), Some(p)) => format!("{m} {p}"),
            (None, Some(p)) => p.clone(),
            (Some(m), None) => m.clone(),
            (None, None) => "unnamed".to_string(),
        };
        format!(
            "#{} {:04x}:{:04x} at {} (port {}) — {}",
            self.index,
            self.vendor_id,
            self.product_id,
            self.address_selector(),
            self.port_selector(),
            name
        )
    }
}

/// How a caller names *which* dongle to open.
///
/// Two dongles of the same model are the case this exists for: they share a
/// vendor and product ID, and the CSR8510 clones publish no serial number, so
/// nothing in the descriptors tells them apart. Only the platform's
/// enumeration does. The four forms trade precision against how long the name
/// stays true:
///
/// | form | example | precise | survives a re-plug |
/// |---|---|---|---|
/// | [`First`](Self::First) | *(no argument)* | no | n/a |
/// | [`VidPid`](Self::VidPid) | `0a12:0001` | only when unique | yes |
/// | [`Index`](Self::Index) | `#1` | yes, within a session | no — order can change |
/// | [`BusAddress`](Self::BusAddress) | `02/4` | yes | no — a new address is assigned |
/// | [`BusPort`](Self::BusPort) | `02.1` | yes | **yes, in the same socket** |
///
/// `VidPid` is deliberately *not* "the first one that matches": that is the
/// bug this type exists to fix. With two dongles plugged in, `0a12:0001` is an
/// error that lists the candidates by their `bus/addr` and `bus.port` names,
/// so the next attempt can be exact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsbSelector {
    /// The first Bluetooth-class device found. Fine with one dongle, a coin
    /// flip with two.
    First,
    /// A vendor/product ID pair, which must match exactly one device. Also
    /// the only form that reaches a dongle hiding behind a vendor-specific
    /// class code, since such a device is in no Bluetooth-class listing.
    VidPid(u16, u16),
    /// A position in [`list_bluetooth_dongles`]'s order.
    Index(usize),
    /// A bus and the address the device currently holds on it — what `lsusb`
    /// prints as `Bus 002 Device 004`.
    BusAddress {
        /// Platform bus identifier, compared numerically when both sides are
        /// numbers so `2` finds `"002"`.
        bus_id: String,
        /// Address on that bus.
        device_address: u8,
    },
    /// A bus and a hub port path: the physical socket, whatever is in it.
    BusPort {
        /// Platform bus identifier, compared the same way as in
        /// [`BusAddress`](Self::BusAddress).
        bus_id: String,
        /// Port numbers from the root hub down.
        port_chain: Vec<u8>,
    },
}

/// Bus identifiers are strings because Windows' are not numbers, but Linux
/// zero-pads (`"001"`) where a human types `1`. Compare as numbers when both
/// sides are numbers, and as text otherwise.
fn bus_id_matches(spec: &str, actual: &str) -> bool {
    match (spec.parse::<u32>(), actual.parse::<u32>()) {
        (Ok(a), Ok(b)) => a == b,
        _ => spec.eq_ignore_ascii_case(actual),
    }
}

impl UsbSelector {
    /// Parses the string form. The separator decides which form it is, so no
    /// prefix keyword is needed and the old `vid:pid` spelling still means
    /// what it did:
    ///
    /// - `#N` — index into [`list_bluetooth_dongles`] (`#` required, so a
    ///   bare number is never silently an index).
    /// - `vid:pid` — hex pair, e.g. `0a12:0001`.
    /// - `bus/address` — decimal address, e.g. `02/4`.
    /// - `bus.port[.port…]` — hub port path, e.g. `02.1` or `2.4.1`.
    pub fn parse(spec: &str) -> Result<Self, SimbleError> {
        let spec = spec.trim();
        let invalid = |why: &str| {
            SimbleError::Transport(format!(
                "invalid dongle selector {spec:?}: {why} (expected \"#0\" for an index, \
                 \"0a12:0001\" for vid:pid, \"02/4\" for bus/address, or \"02.1\" for a \
                 bus.port path)"
            ))
        };
        if spec.is_empty() {
            return Err(invalid("empty"));
        }
        if let Some(n) = spec.strip_prefix('#') {
            return n
                .parse::<usize>()
                .map(UsbSelector::Index)
                .map_err(|_| invalid("index is not a number"));
        }
        if spec.contains(':') {
            let (vid, pid) = parse_vid_pid(spec)?;
            return Ok(UsbSelector::VidPid(vid, pid));
        }
        if let Some((bus_id, address)) = spec.split_once('/') {
            let device_address = address
                .parse::<u8>()
                .map_err(|_| invalid("device address is not a number in 0..=255"))?;
            return Ok(UsbSelector::BusAddress {
                bus_id: bus_id.to_string(),
                device_address,
            });
        }
        if let Some((bus_id, ports)) = spec.split_once('.') {
            let port_chain = ports
                .split('.')
                .map(|p| p.parse::<u8>())
                .collect::<Result<Vec<u8>, _>>()
                .map_err(|_| invalid("a port number is not a number in 0..=255"))?;
            return Ok(UsbSelector::BusPort {
                bus_id: bus_id.to_string(),
                port_chain,
            });
        }
        Err(invalid("no separator"))
    }

    /// How this selector reads back to whoever supplied it.
    pub fn describe(&self) -> String {
        match self {
            UsbSelector::First => "the first Bluetooth-class dongle found".to_string(),
            UsbSelector::VidPid(vid, pid) => format!("{vid:04x}:{pid:04x}"),
            UsbSelector::Index(n) => format!("#{n}"),
            UsbSelector::BusAddress {
                bus_id,
                device_address,
            } => format!("{bus_id}/{device_address}"),
            UsbSelector::BusPort { bus_id, port_chain } => {
                let ports: Vec<String> = port_chain.iter().map(u8::to_string).collect();
                format!("{bus_id}.{}", ports.join("."))
            }
        }
    }
}

/// The Bluetooth-class devices currently enumerated, in a stable order
/// (sorted by bus, then address) so `#0` and `#1` mean the same thing to
/// every caller in a session.
///
/// A caller cannot choose without a list — `run_on("usb")` and every
/// hardware test start here. Nothing is opened, so this succeeds for dongles
/// already claimed by another process or by the OS Bluetooth stack; whether
/// one can actually be *used* is only learned by opening it.
pub fn list_bluetooth_dongles() -> Result<Vec<UsbDongle>, SimbleError> {
    let infos = list_usb_devices()?;
    let all: Vec<UsbDongle> = infos.iter().map(dongle_from_info).collect();
    Ok(bluetooth_order(&infos, &all)
        .into_iter()
        .enumerate()
        .map(|(index, position)| UsbDongle {
            index,
            ..all[position].clone()
        })
        .collect())
}

/// Positions in `infos` of the Bluetooth-class devices, in the stable order
/// [`list_bluetooth_dongles`] publishes: bus, then address.
fn bluetooth_order(infos: &[nusb::DeviceInfo], all: &[UsbDongle]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..infos.len())
        .filter(|&i| is_bluetooth_device(&infos[i]))
        .collect();
    order.sort_by_key(|&i| sort_key(&all[i].bus_id, all[i].device_address));
    order
}

/// Everything the descriptors say about one device. `index` is filled in by
/// [`list_bluetooth_dongles`], which is what defines the numbering.
fn dongle_from_info(d: &nusb::DeviceInfo) -> UsbDongle {
    UsbDongle {
        index: 0,
        // `bus_id`, `device_address`, and `port_chain` are the sysfs-derived
        // location fields. nusb exposes their accessors only on desktop
        // targets (linux/macos/windows); on Android it offers no device
        // enumeration at all, so these fall back to empty. This branch is
        // never reached on Android — `list_usb_devices` errors out first —
        // but it must still compile there.
        #[cfg(not(target_os = "android"))]
        bus_id: d.bus_id().to_string(),
        #[cfg(target_os = "android")]
        bus_id: String::new(),
        #[cfg(not(target_os = "android"))]
        device_address: d.device_address(),
        #[cfg(target_os = "android")]
        device_address: 0,
        #[cfg(not(target_os = "android"))]
        port_chain: d.port_chain().to_vec(),
        #[cfg(target_os = "android")]
        port_chain: Vec::new(),
        vendor_id: d.vendor_id(),
        product_id: d.product_id(),
        manufacturer: d.manufacturer_string().map(str::to_string),
        product: d.product_string().map(str::to_string),
        serial_number: d.serial_number().map(str::to_string),
    }
}

/// Picks the one device `selector` names out of `all` — every enumerated
/// device, in enumeration order — with `bluetooth` giving the positions of
/// the Bluetooth-class ones in [`list_bluetooth_dongles`] order. Returns a
/// position in `all`.
///
/// Split from [`UsbTransport::open_selected`] so every rule below is testable
/// on a machine with no dongle in it. The rule that matters: a selector
/// matching several devices is an **error naming the candidates**, never the
/// first of them. Two dongles of one model share a vid:pid, so "first match
/// wins" silently opens whichever the OS happened to enumerate first — the
/// two runs of a device-to-device test then land on the same radio and the
/// failure looks like RF.
fn resolve_selection(
    selector: &UsbSelector,
    all: &[UsbDongle],
    bluetooth: &[usize],
) -> Result<usize, SimbleError> {
    // How a candidate reads in an error message: its index when it is a
    // Bluetooth-class device, and always both location forms.
    let name = |position: usize| {
        let d = &all[position];
        let index = bluetooth
            .iter()
            .position(|&p| p == position)
            .map(|i| format!("#{i} "))
            .unwrap_or_default();
        format!(
            "{index}{:04x}:{:04x} at {} (port {})",
            d.vendor_id,
            d.product_id,
            d.address_selector(),
            d.port_selector()
        )
    };
    let listing = |positions: &[usize]| {
        positions
            .iter()
            .map(|&p| name(p))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let unique = |positions: Vec<usize>, what: String| match positions.len() {
        1 => Ok(positions[0]),
        0 => Err(SimbleError::Transport(format!(
            "no USB device matches {what}"
        ))),
        n => Err(SimbleError::Transport(format!(
            "{what} matches {n} USB devices — name one exactly: {}",
            listing(&positions)
        ))),
    };

    match selector {
        UsbSelector::First => bluetooth.first().copied().ok_or_else(|| {
            SimbleError::Transport(
                "no Bluetooth-class USB device found (class E0/01/01 at device or interface level)"
                    .to_string(),
            )
        }),
        UsbSelector::Index(n) => bluetooth.get(*n).copied().ok_or_else(|| {
            SimbleError::Transport(if bluetooth.is_empty() {
                format!("no Bluetooth-class USB device found, so there is no #{n}")
            } else {
                format!(
                    "no dongle #{n}: {} Bluetooth-class dongle(s) are plugged in — {}",
                    bluetooth.len(),
                    listing(bluetooth)
                )
            })
        }),
        UsbSelector::VidPid(vid, pid) => unique(
            (0..all.len())
                .filter(|&i| all[i].vendor_id == *vid && all[i].product_id == *pid)
                .collect(),
            format!("{vid:04x}:{pid:04x}"),
        ),
        UsbSelector::BusAddress {
            bus_id,
            device_address,
        } => unique(
            (0..all.len())
                .filter(|&i| {
                    bus_id_matches(bus_id, &all[i].bus_id)
                        && all[i].device_address == *device_address
                })
                .collect(),
            format!("bus {bus_id} address {device_address}"),
        ),
        UsbSelector::BusPort { bus_id, port_chain } => unique(
            (0..all.len())
                .filter(|&i| {
                    bus_id_matches(bus_id, &all[i].bus_id) && all[i].port_chain == *port_chain
                })
                .collect(),
            format!("bus {bus_id} port {}", selector.describe()),
        ),
    }
}

/// Orders a bus id numerically when it is a number (so `"2"` sorts before
/// `"10"`, which text order gets wrong), falling back to text.
fn sort_key(bus_id: &str, device_address: u8) -> (u32, String, u8) {
    (
        bus_id.parse::<u32>().unwrap_or(u32::MAX),
        bus_id.to_string(),
        device_address,
    )
}

/// Whether a `DeviceInfo` is a Bluetooth HCI controller, at either
/// descriptor level (see [`is_bluetooth_hci`]).
fn is_bluetooth_device(d: &nusb::DeviceInfo) -> bool {
    let interfaces: Vec<_> = d
        .interfaces()
        .map(|i| (i.class(), i.subclass(), i.protocol()))
        .collect();
    is_bluetooth_hci((d.class(), d.subclass(), d.protocol()), &interfaces)
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
    /// H4-typed bulk framing: each packet on the wire begins with its H4
    /// type byte (ACL or ISO), so the length field's offset moves by one
    /// and the emitted packet keeps the type it arrived with.
    typed: bool,
}

/// ACL data packet header length: 2-byte handle/flags + 2-byte data length
/// (Vol 4, Part E, Section 5.4.2).
const ACL_HEADER_LEN: usize = 4;

impl AclInReassembler {
    fn feed(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    fn next_packet(&mut self) -> Option<Vec<u8>> {
        if self.typed {
            // [type | handle(2) | len(2) | payload]: ACL carries a plain
            // 16-bit length; ISO keeps two flag bits in the top of its
            // length field (Vol 4, Part E, Section 5.4.5).
            if self.buffer.len() < 1 + ACL_HEADER_LEN {
                return None;
            }
            let raw = u16::from_le_bytes([self.buffer[3], self.buffer[4]]) as usize;
            let payload_len = if self.buffer[0] == h4_type::HCI_ISO_DATA {
                raw & 0x3FFF
            } else {
                raw
            };
            let total_len = 1 + ACL_HEADER_LEN + payload_len;
            if self.buffer.len() < total_len {
                return None;
            }
            let packet = self.buffer[..total_len].to_vec();
            self.buffer.drain(..total_len);
            return Some(packet);
        }
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

/// How long a freshly opened dongle is drained of traffic that predates this
/// session, and how long of silence ends the drain early. A dongle that was
/// never opened before is silent from the first poll, so this costs
/// [`STALE_QUIET`] and not [`STALE_BUDGET`] in the normal case.
const STALE_BUDGET: Duration = Duration::from_millis(100);
const STALE_QUIET: Duration = Duration::from_millis(20);

/// Throws away whatever the controller had queued before this session began.
///
/// A dongle does not forget when the host lets go of it. Events its previous
/// owner asked for and never collected sit in the controller's buffer and are
/// delivered to the *next* program to claim the interface, ahead of anything
/// that program asked for. The result is a host that reads someone else's
/// answers as its own: a `Read_BD_ADDR` completion from the last session
/// arrives before this session's `Reset` has even been answered, so the host
/// believes bring-up finished while the controller is still in reset.
///
/// This is invisible on a first open and invisible in simulation, where no
/// controller outlives its host. It shows up the moment one program opens the
/// same dongle twice — which is exactly what a suite of hardware tests does.
///
/// Called before any command goes out, so nothing legitimate can be discarded
/// by definition: the host has asked for nothing yet.
fn discard_stale_traffic(endpoints: &mut impl UsbEndpoints) -> Result<(), SimbleError> {
    let start = std::time::Instant::now();
    let mut last_seen = start;
    while start.elapsed() < STALE_BUDGET {
        let mut saw_any = false;
        while endpoints.try_recv_event()?.is_some() {
            saw_any = true;
        }
        while endpoints.try_recv_acl()?.is_some() {
            saw_any = true;
        }
        if saw_any {
            last_seen = std::time::Instant::now();
        } else if last_seen.elapsed() >= STALE_QUIET {
            break;
        }
    }
    Ok(())
}

/// Bidirectional HCI transport to a physical USB Bluetooth dongle, exposing
/// the same [`pump`](Self::pump) contract as `RootcanalTransport` and
/// `NetsimTransport` so callers can swap a simulated controller for real
/// hardware without touching the rest of the stack.
pub struct UsbTransport {
    endpoints: Box<dyn UsbEndpoints>,
    acl_reassembler: AclInReassembler,
    /// H4-typed bulk framing: every bulk packet begins with its H4 type
    /// byte, which is how ISO data gets a USB path at all — the standard
    /// class has none. Opted into by firmware that names itself for it
    /// (our patched Zephyr `hci_usb` reads "H4-bulk" in its product
    /// string); a standard dongle never sees a prefixed byte.
    typed_bulk: bool,
    /// The controller's command budget. Real dongles drop commands sent past
    /// it without complaining — see [`CommandCredits`].
    credits: CommandCredits,
    /// Whether `credits` gates outbound commands. See
    /// [`set_command_flow_control`](Self::set_command_flow_control).
    flow_control: bool,
}

impl UsbTransport {
    /// Opens the first USB device whose descriptors identify it as a
    /// Bluetooth HCI controller (Wireless Controller / RF / Bluetooth class).
    ///
    /// "First" is [`list_bluetooth_dongles`] order, so it is at least
    /// repeatable — but with two dongles plugged in it is still a guess.
    /// [`open_selected`](Self::open_selected) is how you say which one.
    pub fn open_first() -> Result<Self, SimbleError> {
        Self::open_selected(&UsbSelector::First)
    }

    /// Opens the USB device with the given vendor/product ID, for dongles
    /// that hide behind vendor-specific class codes (see also
    /// [`parse_vid_pid`] for the `"vid:pid"` string form).
    ///
    /// **Errors if more than one device carries that pair**, listing them —
    /// two dongles of the same model are indistinguishable by ID, and
    /// quietly taking the first is how a two-radio test ends up talking to
    /// one radio twice.
    pub fn open(vid: u16, pid: u16) -> Result<Self, SimbleError> {
        Self::open_selected(&UsbSelector::VidPid(vid, pid))
    }

    /// Asks the controller for its own `BD_ADDR`, blocking until it answers.
    ///
    /// A dongle's public address lives in ROM, so it is the address a peer
    /// actually sees — and therefore the one SMP must compute with. Reading it
    /// is the only way to know it: nothing the host configures can change it
    /// while `own_address_type` is public.
    pub fn read_bd_addr(&mut self) -> Result<crate::types::Address, SimbleError> {
        let channel = super::HciChannel::new();
        channel.send_command(&[0x09, 0x10, 0x00])?;
        for _ in 0..2000 {
            self.pump(&channel)?;
            while let Some(p) = channel.poll_controller_packet() {
                // Command Complete for Read_BD_ADDR: 04 0E 0A 01 09 10 status addr[6]
                if p.len() >= 13 && p[1] == 0x0E && p[4] == 0x09 && p[5] == 0x10 && p[6] == 0x00 {
                    let mut bytes = [0u8; 6];
                    bytes.copy_from_slice(&p[7..13]);
                    return Ok(crate::types::Address::new(bytes));
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        Err(SimbleError::Transport(
            "the controller never answered Read_BD_ADDR".to_string(),
        ))
    }

    /// Opens the one dongle `selector` names, or fails saying what else it
    /// could have meant. See [`UsbSelector`] for the forms and their
    /// trade-offs.
    pub fn open_selected(selector: &UsbSelector) -> Result<Self, SimbleError> {
        let infos = list_usb_devices()?;
        let all: Vec<UsbDongle> = infos.iter().map(dongle_from_info).collect();
        let bluetooth = bluetooth_order(&infos, &all);
        let chosen = resolve_selection(selector, &all, &bluetooth)?;
        Self::open_device(&infos[chosen])
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
        let mut event_in = interface
            .endpoint::<Interrupt, In>(addresses.event_in)
            .map_err(|e| claim("interrupt IN", e))?;
        let mut acl_in = interface
            .endpoint::<Bulk, In>(addresses.acl_in)
            .map_err(|e| claim("bulk IN", e))?;
        let mut acl_out = interface
            .endpoint::<Bulk, Out>(addresses.acl_out)
            .map_err(|e| claim("bulk OUT", e))?;

        // Clear each endpoint's halt before the first transfer, which also
        // resets the data toggle on both ends.
        //
        // This is not defensive tidying — without it the *second* open of a
        // dongle in one session is deaf. A USB endpoint's DATA0/DATA1 toggle
        // is state the device keeps; the host resets its own to DATA0 on
        // claiming, and the dongle, never having been told anything happened,
        // carries on from wherever the previous session left it. Half the
        // time the two disagree and every event the controller sends is
        // dropped by the host controller as a retransmission. The symptom is
        // a dongle that enumerates, claims, and accepts commands, and answers
        // exactly nothing — with no error anywhere, because at the USB level
        // nothing went wrong. Two dongles being opened and closed by
        // successive tests is what makes this reachable at all; a demo that
        // opens one dongle once never sees it.
        //
        // Safe on a healthy endpoint: CLEAR_FEATURE(ENDPOINT_HALT) on an
        // unhalted endpoint is defined as a no-op (USB 2.0, Section 9.4.1).
        // Must happen before any transfer is submitted, which is why it is
        // here and not in `NusbEndpoints`.
        let unhalt = |what: &str, e: nusb::Error| {
            SimbleError::Transport(format!(
                "clearing the halt on the {what} endpoint of {vid:04x}:{pid:04x} failed: {e}"
            ))
        };
        event_in
            .clear_halt()
            .wait()
            .map_err(|e| unhalt("interrupt IN", e))?;
        acl_in
            .clear_halt()
            .wait()
            .map_err(|e| unhalt("bulk IN", e))?;
        acl_out
            .clear_halt()
            .wait()
            .map_err(|e| unhalt("bulk OUT", e))?;

        let mut endpoints = NusbEndpoints {
            interface,
            event_in,
            acl_in,
            acl_out,
            event_transfer_len,
            acl_transfer_len,
        };
        discard_stale_traffic(&mut endpoints)?;
        let typed_bulk = info.product_string().is_some_and(|p| p.contains("H4-bulk"));
        let mut transport = Self::with_endpoints(Box::new(endpoints));
        transport.typed_bulk = typed_bulk;
        transport.acl_reassembler.typed = typed_bulk;
        Ok(transport)
    }

    fn with_endpoints(endpoints: Box<dyn UsbEndpoints>) -> Self {
        Self {
            endpoints,
            acl_reassembler: AclInReassembler::default(),
            credits: CommandCredits::new(),
            flow_control: true,
            typed_bulk: false,
        }
    }

    /// The controller's remaining command budget and how many commands are
    /// waiting on it — for a caller that wants to report "the dongle stopped
    /// answering" rather than hang. See [`CommandCredits`].
    pub fn command_backlog(&self) -> (u8, usize) {
        (self.credits.credits(), self.credits.queued())
    }

    /// Turns HCI command flow control off, restoring the pre-[`CommandCredits`]
    /// behaviour: commands go out the instant they are queued, budget or no
    /// budget.
    ///
    /// **This is a bug switch, and it exists to be demonstrated.** A dongle
    /// does not reject commands past its budget, it discards them, so nothing
    /// in a log says what went wrong. The only way to show the difference is
    /// to run the same burst both ways against the same silicon, which is
    /// what `tests/usb_hardware_test.rs` does. No production caller should
    /// touch this.
    pub fn set_command_flow_control(&mut self, enabled: bool) {
        self.flow_control = enabled;
    }

    /// Moves packets in both directions between the dongle and `channel`,
    /// mirroring the `rootcanal`/`netsim` pump contract: drains every
    /// H4-framed packet `channel` has queued for the controller, strips the
    /// type byte, and routes commands to the control endpoint and ACL data
    /// to bulk OUT; then polls the interrupt and bulk IN endpoints without
    /// blocking, restores the H4 type byte (reassembling fragmented ACL
    /// packets), and hands complete packets to `channel`.
    ///
    /// **Commands leave under the controller's own flow control**
    /// ([`CommandCredits`]), which is the one place this differs from the
    /// simulated transports. A dongle grants one credit at a time and
    /// silently drops anything sent past its budget, so a command drained
    /// from `channel` may sit here for several pumps before it goes out. ACL
    /// data is not affected — it has its own buffer accounting.
    pub fn pump(&mut self, channel: &super::HciChannel) -> Result<(), SimbleError> {
        // `SIMBLE_HCI_LOG=1` traces every packet in both directions. A real
        // controller's misbehaviour is usually silent — a command discarded
        // for want of credit, a parameter refused, an event never raised —
        // and without a trace the symptom is only ever "nothing happened".
        let trace = std::env::var_os("SIMBLE_HCI_LOG").is_some();
        while let Some(packet) = channel.poll_host_packet() {
            if trace {
                eprintln!("host -> ctlr  {:02X?}", packet);
            }
            let Some((&packet_type, payload)) = packet.split_first() else {
                return Err(SimbleError::Transport("empty H4 packet".to_string()));
            };
            match packet_type {
                h4_type::HCI_COMMAND if self.flow_control => self.credits.queue(payload.to_vec()),
                h4_type::HCI_COMMAND => self.endpoints.send_command(payload)?,
                h4_type::HCI_ACL_DATA => {
                    if trace && payload.len() > 8 {
                        // L2CAP CID 0x0006 is the SMP fixed channel, 0x0004 ATT.
                        let cid = u16::from_le_bytes([payload[6], payload[7]]);
                        eprintln!("host -> ctlr  ACL cid={cid:#06x} {:02X?}", &payload[8..]);
                    }
                    if self.typed_bulk {
                        self.endpoints.send_acl(&packet)? // type byte and all
                    } else {
                        self.endpoints.send_acl(payload)?
                    }
                }
                // ISO data has a USB path only in the typed-bulk dialect;
                // SCO would need interface 1's isochronous endpoints, which
                // this transport does not claim.
                h4_type::HCI_ISO_DATA if self.typed_bulk => self.endpoints.send_acl(&packet)?,
                other => {
                    return Err(SimbleError::Transport(format!(
                        "H4 packet type {other:#04x} is not supported over the USB transport"
                    )));
                }
            }
        }
        self.send_permitted_commands()?;

        while let Some(event) = self.endpoints.try_recv_event()? {
            // Before the event is handed on, not after: the credit it carries
            // is what releases the next command in the same pump.
            self.credits.observe_event(&event);
            if trace {
                eprintln!("ctlr -> host  [04] {:02X?}", event);
            }
            let mut packet = Vec::with_capacity(1 + event.len());
            packet.push(h4_type::HCI_EVENT);
            packet.extend_from_slice(&event);
            channel.receive_from_controller(packet)?;
        }
        self.send_permitted_commands()?;

        while let Some(chunk) = self.endpoints.try_recv_acl()? {
            if trace && chunk.len() > 8 {
                let cid = u16::from_le_bytes([chunk[6], chunk[7]]);
                eprintln!("ctlr -> host  ACL cid={cid:#06x} {:02X?}", &chunk[8..]);
            }
            self.acl_reassembler.feed(&chunk);
            while let Some(packet) = self.acl_reassembler.next_packet() {
                channel.receive_from_controller(packet)?;
            }
        }
        Ok(())
    }

    /// Sends as many queued commands as the controller's current budget
    /// allows.
    fn send_permitted_commands(&mut self) -> Result<(), SimbleError> {
        while let Some(command) = self.credits.next_to_send() {
            self.endpoints.send_command(&command)?;
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
    /// Which dongle to open, in any of [`UsbSelector`]'s forms.
    /// The dongle the caller named, and the pool it is the first of.
    ///
    /// A dongle is one controller and hosts one device, so a scene with two
    /// devices needs two dongles. `device` is the one the caller asked for;
    /// `pool` is every Bluetooth-class dongle on the machine, consulted only
    /// when a *second* device is added, so a single-dongle machine behaves
    /// exactly as before.
    device: UsbSelector,
    pool: Vec<UsbSelector>,
    /// The dongles already hosting a device, in the order they were taken.
    taken: Vec<UsbSelector>,
    scene: crate::transport::live_scene::LiveScene<UsbTransport>,
}

impl UsbScene {
    /// Creates an empty dongle-backed scene. `device` says which dongle;
    /// [`UsbSelector::First`] auto-detects, which is only unambiguous when
    /// exactly one is plugged in.
    pub fn new(device: UsbSelector) -> Self {
        Self {
            device,
            pool: Vec::new(),
            taken: Vec::new(),
            scene: crate::transport::live_scene::LiveScene::new(),
        }
    }

    /// How the dongle will be chosen, for reporting back to whoever selected
    /// this backend before anything has been opened.
    pub fn selector(&self) -> String {
        self.device.describe()
    }

    /// The dongle for the next device: the caller's choice first, then any
    /// other Bluetooth-class dongle not already hosting one.
    ///
    /// A radio can host exactly one device, so "two devices" and "two
    /// dongles" are the same requirement. The refusal says which is missing
    /// rather than asserting the old flat rule that one dongle is the limit.
    fn next_dongle(&mut self) -> Result<UsbSelector, String> {
        if self.scene.device_count() == 0 {
            return Ok(self.device.clone());
        }
        if self.pool.is_empty() {
            self.pool = list_bluetooth_dongles()
                .map_err(|e| format!("cannot list dongles: {e}"))?
                .iter()
                .filter_map(|d| UsbSelector::parse(&d.port_selector()).ok())
                .collect();
        }
        let taken = self.taken.clone();
        self.pool
            .iter()
            .find(|candidate| !taken.iter().any(|t| t.describe() == candidate.describe()))
            .cloned()
            .ok_or_else(|| {
                format!(
                    "every dongle is already hosting a device ({} plugged in, {} in use) — \
                     a radio hosts one device, so another needs another dongle; \
                     run_on(\"self\") for a scene with as many devices as you like",
                    self.pool.len(),
                    self.scene.device_count()
                )
            })
    }

    /// Runs `script` and puts the resulting peripheral on a dongle, opening
    /// it on this call. The first device takes the dongle the caller named;
    /// each later one takes the next free dongle on the machine, and the
    /// scene refuses only when the radios run out.
    pub fn add_peripheral(
        &mut self,
        address: crate::types::Address,
        script: &str,
    ) -> Result<usize, String> {
        let device = self.next_dongle()?;
        self.taken.push(device.clone());
        self.scene.add_peripheral(address, script, |peripheral| {
            // A dongle's vintage is unknown and usually old — the CSR8510
            // clones everyone has are 4.0 parts. The default LE_Event_Mask
            // sets bits such a controller does not define, and it rejects the
            // whole command with 0x12 rather than masking off what it does not
            // know. The failure is silent from the host's side: no LE Meta
            // Event ever arrives, so the peripheral advertises into the void
            // and never learns anyone connected.
            peripheral.set_le_event_mask(crate::device::host::LE_EVENT_MASK_CORE_4_0);
            let mut transport = UsbTransport::open_selected(&device).map_err(|e| {
                format!(
                    "{e} — is a Bluetooth dongle plugged in and free? \
                     (the OS Bluetooth stack usually claims the built-in adapter)"
                )
            })?;
            // Re-stamp the identity with the address the *controller* will
            // actually advertise. A public address lives in ROM, so the
            // caller's choice never reaches the air — and SMP computes its
            // confirm value over the responder address, so a peer that saw the
            // dongle's address and a host that used the caller's disagree and
            // pairing dies with 0x04, Confirm Value Failed. That is not a
            // hypothetical: it is what a phone does to this scene.
            match transport.read_bd_addr() {
                Ok(real) => peripheral.set_identity(real),
                Err(e) => eprintln!(
                    "simble: could not read the controller's BD_ADDR ({e}); \
                     SMP will compute with an address the peer never saw"
                ),
            }
            Ok(transport)
        })
    }

    /// Opens a dongle as a scanner and joins it to the scene, so `scan` hears
    /// what is actually on the air — real devices around the machine, not the
    /// agent's own peripherals. A radio hosts one role, so the scanner claims
    /// its own dongle: the named one when the scene is otherwise empty, else the
    /// next free one. Idempotent — a scene keeps one scanner.
    pub fn add_scanner(&mut self) -> Result<(), String> {
        if self.scene.has_scanner() {
            return Ok(());
        }
        let device = self.next_dongle()?;
        self.taken.push(device.clone());
        let transport = UsbTransport::open_selected(&device).map_err(|e| {
            format!(
                "{e} — is a Bluetooth dongle plugged in and free? \
                 (the OS Bluetooth stack usually claims the built-in adapter)"
            )
        })?;
        self.scene.add_scanner(transport);
        Ok(())
    }

    /// Whether a scanner is already listening on this scene.
    pub fn has_scanner(&self) -> bool {
        self.scene.has_scanner()
    }

    /// The scanner's advertising reports as a JSON array, or `None` if none was
    /// added — the real-RF answer to `scan`.
    pub fn scanner_reports_json(&self) -> Option<String> {
        self.scene.scanner_reports_json()
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

/// The shared [`Scene`](super::Scene) shape, forwarding to the inherent methods
/// above. A dongle is real radio, so it implements scanning too.
impl super::Scene for UsbScene {
    fn name(&self) -> &'static str {
        "usb"
    }
    fn add_peripheral(
        &mut self,
        address: crate::types::Address,
        script: &str,
    ) -> Result<usize, String> {
        UsbScene::add_peripheral(self, address, script)
    }
    fn pump(&mut self) {
        self.pump()
    }
    fn tick(&mut self, millis: f64) -> Option<f64> {
        // The trait speaks milliseconds; the inherent clock is seconds.
        UsbScene::tick(self, millis / 1000.0);
        self.next_timeout_ms()
    }
    fn now_ms(&self) -> f64 {
        UsbScene::now(self) * 1000.0
    }
    fn device_count(&self) -> usize {
        UsbScene::device_count(self)
    }
    fn peripheral_status_json(&self, index: usize) -> Option<String> {
        UsbScene::peripheral_status_json(self, index)
    }
    fn add_scanner(&mut self) -> Result<(), String> {
        UsbScene::add_scanner(self)
    }
    fn has_scanner(&self) -> bool {
        UsbScene::has_scanner(self)
    }
    fn scanner_reports_json(&self) -> Option<String> {
        UsbScene::scanner_reports_json(self)
    }
}

#[cfg(not(target_os = "android"))]
fn list_usb_devices() -> Result<Vec<nusb::DeviceInfo>, SimbleError> {
    Ok(nusb::list_devices()
        .wait()
        .map_err(|e| SimbleError::Transport(format!("USB device enumeration failed: {e}")))?
        .collect())
}

// nusb has no `list_devices()` on Android: USB devices arrive as a file
// descriptor handed over by the Java framework, not by enumeration. Real-radio
// USB scanning is therefore unavailable on-device, and this reports that rather
// than silently returning an empty list.
#[cfg(target_os = "android")]
fn list_usb_devices() -> Result<Vec<nusb::DeviceInfo>, SimbleError> {
    Err(SimbleError::Transport(
        "USB device enumeration is not supported on Android".to_string(),
    ))
}

#[cfg(test)]
#[path = "usb_tests.rs"]
mod tests;
