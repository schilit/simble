// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! HCI Transports and in-memory bridges to the Rootcanal Controller.

pub(crate) mod hci_adapter;
// The socket/USB transports need `std::net`/`nusb`, neither of which exists
// on wasm32-unknown-unknown; the browser build talks to netsim through
// `wasm_ws` instead, whose JS-binding half is gated inside the module so its
// pure-Rust demo engines stay natively compiled and natively tested.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod live_scene;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod netsim;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod rootcanal;
#[cfg(not(target_arch = "wasm32"))]
pub mod usb;
pub mod wasm_ws;
// Shared hand-rolled RFC 6455 codec + the WebSocket server end (`usb-ble-ws`).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod ws;

pub use hci_adapter::{HciChannel, h4_type};

/// The contract every live HCI transport already meets by convention
/// (rootcanal, netsim, usb): move packets both ways between the wire and
/// `channel` without blocking. Formalized so scenes can be generic over
/// *where* their controller lives — `LiveScene<T>` runs scripted devices
/// over any of them.
#[cfg(not(target_arch = "wasm32"))]
pub trait HciTransport {
    /// Drains host→controller packets from `channel` onto the wire, and
    /// feeds any controller→host packets currently available back into it.
    fn pump(&mut self, channel: &HciChannel) -> Result<(), crate::types::SimbleError>;
}

#[cfg(not(target_arch = "wasm32"))]
impl<S: std::io::Read + std::io::Write> HciTransport for netsim::NetsimTransport<S> {
    fn pump(&mut self, channel: &HciChannel) -> Result<(), crate::types::SimbleError> {
        NetsimTransport::pump(self, channel)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl<S: std::io::Read + std::io::Write> HciTransport for rootcanal::RootcanalTransport<S> {
    fn pump(&mut self, channel: &HciChannel) -> Result<(), crate::types::SimbleError> {
        RootcanalTransport::pump(self, channel)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl HciTransport for usb::UsbTransport {
    fn pump(&mut self, channel: &HciChannel) -> Result<(), crate::types::SimbleError> {
        UsbTransport::pump(self, channel)
    }
}
#[cfg(not(target_arch = "wasm32"))]
pub use netsim::NetsimTransport;
#[cfg(not(target_arch = "wasm32"))]
pub use rootcanal::{H4FrameReader, RootcanalTransport, read_h4_packet, write_h4_packet};
#[cfg(not(target_arch = "wasm32"))]
pub use usb::UsbTransport;
#[cfg(not(target_arch = "wasm32"))]
pub use ws::WsServerConn;
