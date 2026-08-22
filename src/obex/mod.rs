// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! OBEX (IrOBEX 1.3) — the object-exchange protocol three Bluetooth
//! profiles are built on: OPP (object push / "Bluetooth share"), PBAP
//! (phonebook access) and MAP (message access).
//!
//! The layering here is deliberately transport-free: [`server::ObexServer`]
//! and [`client::ObexClient`] take a received packet and hand back the bytes
//! to send. OBEX rides on RFCOMM for these profiles (and on L2CAP for
//! OBEX-over-L2CAP), but neither transport is visible from this module —
//! wiring one in is a matter of relaying two byte buffers.
//!
//! Two details account for most OBEX interoperability bugs, and both are
//! handled in typed code here rather than by hand at each call site:
//!
//! * **Big-endian lengths.** OBEX predates Bluetooth and uses network byte
//!   order, unlike nearly everything else in this crate.
//! * **Lengths include their own prefix.** A header's 2-byte length counts
//!   the 3-byte header prefix; a packet's length counts the 3-byte packet
//!   prefix.
//!
//! The third is [continuation](server): an object too large for one packet
//! arrives as a run of non-final PUTs answered `0x90 Continue`, ending with
//! a PUT-Final answered `0xA0 Success`. A server that answers Success early
//! silently truncates every large object it receives.

pub mod client;
pub mod header;
pub mod opp;
pub mod packet;
pub mod server;

pub use client::{ClientState, ObexClient};
pub use header::{Header, HeaderEncoding, HeaderError, HeaderValue, header_id};
pub use opp::{object_push_server, object_push_service_record};
pub use packet::{PacketError, Request, Response, opcode, response};
pub use server::{ObexServer, ReceivedObject, ServerEvent, ServerLimits, SessionPolicy};
