// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Picking a live controller at run time: netsim, any H4-over-TCP
//! controller, or a real USB dongle.
//!
//! The interop examples all did the same thing — build a netsim WebSocket URL
//! out of a name and an address, connect, optionally start a btsnoop trace.
//! That hardcoded *which* controller they could ever meet, and netsim means
//! `netsimd` from the Android SDK, which is why the foreign-oracle scripts
//! could never run in CI.
//!
//! There is a second controller already on the machine whenever the scripts
//! run: **Bumble's own**. `bumble.controller.Controller` plus
//! `bumble.link.LocalLink` is the same architecture as simble's `sim.rs` plus
//! `Link` — a virtual controller and a virtual ether — and Bumble can expose
//! one over `tcp-server:`, which is plain H4-over-TCP, exactly what
//! [`RootcanalTransport`] already speaks. So a script can host both ends
//! itself with no Android SDK anywhere.
//!
//! The two are **not** interchangeable, and the difference is the point:
//!
//! - rootcanal (what netsim runs) is the controller a real Android emulator
//!   uses. It *dies* on malformed HCI rather than answering with an error
//!   status, which is how this project learned its bytes were wrong twice,
//!   and it honours `Write Inquiry Mode`, which is how the two unhandled
//!   inquiry-result forms were found.
//! - Bumble's controller models no inquiry at all and no BIG, so anything
//!   built on those cannot run against it.
//!
//! This enum is therefore additive: the same example binary reaches either
//! controller, netsim stays the default, and a script that needs something
//! only rootcanal models says so and skips rather than pretending.
//!
//! The third backend is the one neither of those can be: a **USB dongle**,
//! whose radio is physical. Both simulated controllers model only the peers
//! a test also owns, so nothing built on them can meet consumer kit — a
//! Bluetooth speaker answers an inquiry from `usb:` and from nowhere else
//! here. That makes it the only backend whose failures are the peer's rather
//! than a model's, and every hardware session so far has found bugs in this
//! stack that no simulator could have.

use super::serial::SerialTransport;
use super::usb::UsbSelector;
use super::{
    HciChannel, HciTransport, NETSIM_WS_URL, NetsimTransport, RootcanalTransport, UsbTransport,
};
use crate::types::{Address, SimbleError};
use std::net::TcpStream;

/// Environment variable naming the controller to join. Unset means netsim,
/// so every existing invocation keeps its behaviour.
pub const HCI_SPEC_ENV: &str = "SIMBLE_HCI";

/// A live controller connection, chosen at run time. Implements
/// [`HciTransport`], so callers pump it exactly as before.
pub enum LiveTransport {
    /// netsim's WebSocket frontend, reached over `ws://`.
    Netsim(NetsimTransport<TcpStream>),
    /// Any controller speaking bare H4 over TCP — rootcanal's own test port,
    /// or a Bumble `Controller` published with `tcp-server:`.
    Tcp(RootcanalTransport<TcpStream>),
    /// H4 over a serial tty (Zephyr `hci_uart` over CDC-ACM): the physical
    /// backend that also carries ISO data.
    Serial(crate::transport::serial::SerialTransport),
    /// A real USB dongle, opened directly. The only backend whose radio is
    /// physical, and so the only one that can reach consumer kit — a
    /// Bluetooth speaker answers a BR/EDR inquiry from here and from nowhere
    /// else in this enum.
    Usb(UsbTransport),
}

/// Where a spec resolved to, before anything is dialed. Separated from
/// [`LiveTransport::open`] so the parsing is testable without a live
/// controller of either kind on the machine.
#[derive(Debug, PartialEq, Eq)]
pub enum Backend {
    /// netsim, at this fully-built WebSocket URL.
    Netsim(String),
    /// Bare H4 over TCP, at this `host:port`.
    Tcp(String),
    /// A USB dongle, named by any of [`UsbSelector`]'s forms.
    Usb(UsbSelector),
    /// H4 over a serial device — a Zephyr `hci_uart` build behind CDC-ACM.
    /// The transport that carries ISO data, which the USB Bluetooth class
    /// cannot.
    Serial(String),
}

/// Resolves `spec` to the backend it names. See [`LiveTransport::open`] for
/// the accepted forms.
pub fn resolve(spec: &str, name: &str, address: Address) -> Result<Backend, SimbleError> {
    match spec {
        "" | "netsim" => {
            // netsim reads `address=` least-significant byte first, so the
            // display form has to be converted or the device lands on the
            // air reversed.
            Ok(Backend::Netsim(format!(
                "{NETSIM_WS_URL}/v1/websocket/bt?name={name}&address={}",
                address.to_netsim_wire_string()
            )))
        }
        url if url.starts_with("ws://") || url.starts_with("wss://") => {
            Ok(Backend::Netsim(url.to_string()))
        }
        // A bare "usb" takes the only dongle on the machine, which is
        // unambiguous exactly when there is one. Anything after the colon is
        // a `UsbSelector`, so every form that names a *particular* dongle —
        // `#0`, `0a12:0001`, `02/4`, `02.3.4` — works here unchanged, and
        // its parse errors already list the candidates.
        "usb" => Ok(Backend::Usb(UsbSelector::First)),
        spec => {
            if let Some(selector) = spec.strip_prefix("usb:") {
                return Ok(Backend::Usb(UsbSelector::parse(selector)?));
            }
            if let Some(path) = spec.strip_prefix("serial:") {
                return Ok(Backend::Serial(path.to_string()));
            }
            match spec.strip_prefix("tcp:") {
                Some(addr) => Ok(Backend::Tcp(addr.to_string())),
                None => Err(SimbleError::Transport(format!(
                    "unrecognized {HCI_SPEC_ENV} spec {spec:?} — expected \"netsim\", \
                     a ws:// URL, \"tcp:HOST:PORT\", \"serial:/dev/tty…\", or \
                     \"usb\" / \"usb:SELECTOR\""
                ))),
            }
        }
    }
}

impl LiveTransport {
    /// Connects to the controller `spec` names, joining as `name` at
    /// `address` where the backend lets a host say so.
    ///
    /// Accepted specs:
    ///
    /// - `""` or `"netsim"` — netsim on its default WebSocket port. `name`
    ///   and `address` become the URL's query parameters, which is how a
    ///   device gets its identity on netsim's ether.
    /// - `"ws://…"` / `"wss://…"` — that netsim URL verbatim, for a
    ///   non-default port or a fully hand-built query.
    /// - `"tcp:HOST:PORT"` — bare H4 over TCP. `name` and `address` are
    ///   *unused*: on this transport the controller owns the identity, and
    ///   the host sets its address with HCI like it would on real silicon.
    /// - `"usb"` — the only Bluetooth-class dongle on the machine.
    /// - `"usb:SELECTOR"` — a particular dongle, in any form
    ///   [`UsbSelector::parse`] accepts: `usb:#0`, `usb:0a12:0001`,
    ///   `usb:02/4`, `usb:02.3.4`. `name` and `address` are unused here for
    ///   the same reason as `tcp:` — the silicon owns its identity.
    pub fn open(spec: &str, name: &str, address: Address) -> Result<Self, SimbleError> {
        match resolve(spec, name, address)? {
            Backend::Netsim(url) => Ok(Self::Netsim(NetsimTransport::connect(&url)?)),
            Backend::Tcp(addr) => Ok(Self::Tcp(RootcanalTransport::connect(&addr)?)),
            Backend::Usb(selector) => Ok(Self::Usb(UsbTransport::open_selected(&selector)?)),
            Backend::Serial(path) => Ok(Self::Serial(SerialTransport::open(&path)?)),
        }
    }

    /// [`open`](Self::open) with the spec taken from `$SIMBLE_HCI`, which is
    /// how the interop scripts select a controller for the example they
    /// drive. Unset means netsim.
    pub fn open_from_env(name: &str, address: Address) -> Result<Self, SimbleError> {
        let spec = std::env::var(HCI_SPEC_ENV).unwrap_or_default();
        Self::open(&spec, name, address)
    }

    /// Starts a btsnoop capture of every H4 packet both ways, where the
    /// backend supports one. Returns `false` for a backend that does not, so
    /// a caller can say "no capture" rather than claim a file that will stay
    /// empty — only the netsim transport implements tracing today.
    pub fn set_trace(&mut self, file: std::fs::File) -> bool {
        match self {
            Self::Netsim(transport) => transport.set_trace(file).is_ok(),
            Self::Tcp(_) | Self::Usb(_) | Self::Serial(_) => false,
        }
    }

    /// How this connection describes itself in a log line.
    pub fn describe(&self) -> &'static str {
        match self {
            Self::Netsim(_) => "netsim",
            Self::Tcp(_) => "H4/TCP",
            Self::Usb(_) => "USB",
            Self::Serial(_) => "H4/serial",
        }
    }
}

// Hand-written because neither wrapped transport is `Debug` (each owns a
// `TcpStream` and a framer), and the only fact worth printing is which
// backend this is.
impl std::fmt::Debug for LiveTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LiveTransport({})", self.describe())
    }
}

impl HciTransport for LiveTransport {
    fn pump(&mut self, channel: &HciChannel) -> Result<(), SimbleError> {
        match self {
            Self::Netsim(transport) => transport.pump(channel),
            Self::Tcp(transport) => transport.pump(channel),
            Self::Usb(transport) => transport.pump(channel),
            Self::Serial(transport) => transport.pump(channel),
        }
    }
}

#[cfg(test)]
#[path = "live_tests.rs"]
mod tests;
