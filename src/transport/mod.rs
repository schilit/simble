// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! HCI Transports and in-memory bridges to the Rootcanal Controller.

pub(crate) mod hci_adapter;
// The socket/USB transports need `std::net`/`nusb`, neither of which exists
// on wasm32-unknown-unknown; the browser build talks to netsim through
// `wasm_ws` instead, whose JS-binding half is gated inside the module so its
// pure-Rust demo engines stay natively compiled and natively tested.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod netsim;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod rootcanal;
#[cfg(not(target_arch = "wasm32"))]
pub mod usb;
pub mod wasm_ws;

pub use hci_adapter::{HciChannel, h4_type};
#[cfg(not(target_arch = "wasm32"))]
pub use netsim::NetsimTransport;
#[cfg(not(target_arch = "wasm32"))]
pub use rootcanal::{H4FrameReader, RootcanalTransport, read_h4_packet, write_h4_packet};
#[cfg(not(target_arch = "wasm32"))]
pub use usb::UsbTransport;
