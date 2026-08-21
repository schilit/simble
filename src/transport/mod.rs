// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! HCI Transports and in-memory bridges to the Rootcanal Controller.

pub mod hci_adapter;
pub mod netsim;
pub mod rootcanal;

pub use hci_adapter::{HciChannel, h4_type};
pub use netsim::NetsimTransport;
pub use rootcanal::{H4FrameReader, RootcanalTransport, read_h4_packet, write_h4_packet};
