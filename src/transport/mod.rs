// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! HCI transports and in-memory bridges to the Rootcanal controller, plus the
//! transport-neutral [`scan_report`] parsing/bring-up helpers. The `wasm_ws`
//! module is the browser (wasm32) WebSocket bindings only — the engines it once
//! held now live in `scan_report`, `device`, `scene`, and `scripting`.

pub(crate) mod hci_adapter;
pub mod scan_report;
// The socket/USB transports need `std::net`/`nusb`, neither of which exists
// on wasm32-unknown-unknown; the browser build talks to netsim through
// `wasm_ws` instead, whose JS-binding half is gated inside the module so its
// pure-Rust demo engines stay natively compiled and natively tested.
#[cfg(not(target_arch = "wasm32"))]
pub mod live;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod live_scene;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod netsim;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod rootcanal;
#[cfg(not(target_arch = "wasm32"))]
pub mod serial;
// nusb has no wasm32 backend, so this module is as native-only as its
// neighbours above. It lost the gate at some point and the browser build has
// been failing on seventeen `unresolved module nusb` errors ever since; the
// re-exports below and every caller (`mcp`, `live`, the CLI) were already
// gated, so only the declaration was missing.
#[cfg(not(target_arch = "wasm32"))]
pub mod usb;
pub mod wasm_ws;
// Shared hand-rolled RFC 6455 codec + the WebSocket server end (`usb-ble-ws`).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod ws;

pub use hci_adapter::{CommandCredits, HciChannel, h4_type};

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

/// A live scene on one controller: run scripted devices on it, drive them, and
/// read their state. `NetsimScene`, `UsbScene`, and any future backend meet the
/// same shape — each is a thin wrapper over `live_scene::LiveScene<T>` — so a
/// caller (today [`crate::mcp`]'s live server) holds a `Box<dyn Scene>` instead
/// of matching a hand-written enum, and a new controller is one `impl` away
/// rather than another arm in ten methods. This is the "formalise the backend
/// interface" step of `docs/controller-routing.md`.
#[cfg(not(target_arch = "wasm32"))]
pub trait Scene {
    /// What `status` calls this controller ("netsim", "usb", …).
    fn name(&self) -> &'static str;
    /// Runs `script` and registers the device at `address`; returns its index.
    fn add_peripheral(
        &mut self,
        address: crate::types::Address,
        script: &str,
    ) -> Result<usize, String>;
    /// Moves packets both ways for every device on this controller.
    fn pump(&mut self);
    /// Advances the scene's simulated clock by `seconds`.
    fn tick(&mut self, seconds: f64);
    /// The scene's current simulated time, in seconds.
    fn now(&self) -> f64;
    /// How many devices are on this controller.
    fn device_count(&self) -> usize;
    /// A device's render-ready status JSON, if the index is in range.
    fn peripheral_status_json(&self, index: usize) -> Option<String>;
    /// Adds a scanner on this controller's medium. Default: unsupported — only a
    /// controller that can hear real advertisers overrides this.
    fn add_scanner(&mut self) -> Result<(), String> {
        Err("scanning is not supported on this controller".to_string())
    }
    /// Whether a scanner has been added (default: none).
    fn has_scanner(&self) -> bool {
        false
    }
    /// The scanner's reports as JSON, if one is running (default: none).
    fn scanner_reports_json(&self) -> Option<String> {
        None
    }
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
pub use live::LiveTransport;
#[cfg(not(target_arch = "wasm32"))]
// Renamed on the way out: at this level "the default WebSocket URL" is
// ambiguous — there is more than one live backend.
pub use netsim::{DEFAULT_WS_URL as NETSIM_WS_URL, NetsimTransport};
#[cfg(not(target_arch = "wasm32"))]
pub use rootcanal::{H4FrameReader, RootcanalTransport, read_h4_packet, write_h4_packet};
#[cfg(not(target_arch = "wasm32"))]
pub use usb::UsbTransport;
#[cfg(not(target_arch = "wasm32"))]
pub use ws::{Inbound, WsServerConn, accept_inbound};
