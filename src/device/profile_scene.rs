// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Two BR/EDR devices on one simulated link, for any pair of profiles.
//!
//! [`CarKit`](crate::device::CarKit) built its own scene because RFCOMM was
//! the only profile a [`ClassicHost`] could host. Now that
//! [`ProtocolHandler`] takes several channels, three profiles want the same
//! two devices and the same four steps between them — inquire, resolve the
//! name, page, hand over — so those live here once.
//!
//! Everything crosses [`crate::controller::sim`]. Nothing is wired directly
//! together: [`DeviceSpec::scan_enable`] on the acceptor is the switch that
//! proves it, because turning off inquiry scan makes the initiator's plan
//! fail in [`LinkPhase::Inquiring`] rather than connecting anyway.
//!
//! ## Where the plan stops
//!
//! At [`LinkPhase::Connected`], and deliberately. `CarKit`'s plan goes
//! further because RFCOMM's server channel is *dynamic* — the number can
//! only be learned from SDP, so the transport plan has to do the search. The
//! profiles here (AVDTP on 0x0019, HID on 0x0011/0x0013) have PSMs fixed by
//! their specification, so a handler can ask for its own channels through
//! [`ProtocolHandler::poll_channel_requests`] and drive itself from there.
//! The scene stops where the profile can take over.

use crate::controller::sim::Link;
use crate::device::classic_host::scan_enable;
use crate::device::{ClassicHost, ProtocolHandler};
use crate::transport::hci_adapter::HciChannel;
use crate::types::Address;
use std::sync::Arc;

/// How far the initiator has got in bringing the link up.
///
/// Each phase is entered only when the *controller event* for the previous
/// one arrived, never on a tick count. That is what makes a stall visible as
/// a phase that stops moving rather than as a scene that silently drifts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkPhase {
    /// Bring-up commands have not been queued yet.
    Starting,
    /// HCI Inquiry is running.
    Inquiring,
    /// Resolving the acceptor's name.
    ResolvingName,
    /// Paging; waiting for Connection Complete.
    Paging,
    /// The ACL is up and the profiles own the link.
    Connected,
    /// The plan could not continue; see [`ProfileScene::error`].
    Failed,
}

impl LinkPhase {
    /// Stable identifier for a status document.
    pub fn name(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Inquiring => "inquiring",
            Self::ResolvingName => "resolving-name",
            Self::Paging => "paging",
            Self::Connected => "connected",
            Self::Failed => "failed",
        }
    }
}

/// One device to put in a [`ProfileScene`]: who it is, how visible it is,
/// and which profiles it serves.
pub struct DeviceSpec {
    /// The name a Remote Name Request answers with.
    pub name: String,
    /// Class of Device, little-endian on the wire.
    pub class_of_device: [u8; 3],
    /// Its BD_ADDR.
    pub address: Address,
    /// Its Scan Enable (`scan_enable::*`). An acceptor needs at least
    /// inquiry scan to be found and page scan to be connected to.
    pub scan_enable: u8,
    /// The profiles it serves.
    pub handlers: Vec<Box<dyn ProtocolHandler>>,
}

impl DeviceSpec {
    /// A device that is neither discoverable nor connectable — what an
    /// initiator should be, since it is the one doing the finding.
    pub fn initiator(
        name: &str,
        class_of_device: [u8; 3],
        address: Address,
        handlers: Vec<Box<dyn ProtocolHandler>>,
    ) -> Self {
        Self {
            name: name.to_string(),
            class_of_device,
            address,
            scan_enable: scan_enable::NONE,
            handlers,
        }
    }

    /// A device that is both discoverable and connectable.
    pub fn acceptor(
        name: &str,
        class_of_device: [u8; 3],
        address: Address,
        handlers: Vec<Box<dyn ProtocolHandler>>,
    ) -> Self {
        Self {
            name: name.to_string(),
            class_of_device,
            address,
            scan_enable: scan_enable::INQUIRY_AND_PAGE,
            handlers,
        }
    }

    /// Overrides the Scan Enable — the way to build a device that is
    /// deliberately not findable.
    pub fn with_scan_enable(mut self, value: u8) -> Self {
        self.scan_enable = value;
        self
    }
}

/// One device in a scene: its host, its channel to the medium, and whether
/// bring-up has been queued.
struct SceneHost {
    host: ClassicHost,
    channel: Arc<HciChannel>,
    started: bool,
    scan_enable: u8,
}

impl SceneHost {
    fn build(link: &mut Link, spec: DeviceSpec) -> Self {
        let mut host = ClassicHost::new(spec.name, spec.class_of_device);
        for handler in spec.handlers {
            let _ = host.register_handler(handler);
        }
        let channel = link.add_device(spec.address);
        Self {
            host,
            channel,
            started: false,
            scan_enable: spec.scan_enable,
        }
    }

    fn queue_start(&mut self) {
        if self.started {
            return;
        }
        for packet in self.host.start_commands() {
            let _ = self.channel.inject_host_packet(packet);
        }
        for packet in self.host.set_scan_enable(self.scan_enable) {
            let _ = self.channel.inject_host_packet(packet);
        }
        self.started = true;
    }

    fn pump_in(&mut self) -> Result<(), String> {
        while let Some(packet) = self.channel.poll_controller_packet() {
            let out = self
                .host
                .handle_packet(&packet)
                .map_err(|e| e.to_string())?;
            for reply in out {
                let _ = self.channel.inject_host_packet(reply);
            }
        }
        Ok(())
    }

    fn pump_out(&mut self) {
        for packet in self.host.poll() {
            let _ = self.channel.inject_host_packet(packet);
        }
    }
}

/// Two BR/EDR devices on one simulated link: one that finds and pages, one
/// that is found and paged.
///
/// Transport-free, like [`CarKit`](crate::device::CarKit): no sockets and no
/// clock. The caller pumps it with [`Self::tick`] and reads state out
/// through [`Self::initiator`] / [`Self::acceptor`].
pub struct ProfileScene {
    link: Link,
    initiator: SceneHost,
    acceptor: SceneHost,
    target: Address,
    phase: LinkPhase,
    error: Option<String>,
}

impl ProfileScene {
    /// Builds the scene. The initiator will inquire for the acceptor's
    /// address, resolve its name, page it, and then stop.
    pub fn new(initiator: DeviceSpec, acceptor: DeviceSpec) -> Self {
        let mut link = Link::new();
        let target = acceptor.address;
        let initiator = SceneHost::build(&mut link, initiator);
        let acceptor = SceneHost::build(&mut link, acceptor);
        Self {
            link,
            initiator,
            acceptor,
            target,
            phase: LinkPhase::Starting,
            error: None,
        }
    }

    /// How far the transport has got.
    pub fn phase(&self) -> LinkPhase {
        self.phase
    }

    /// Why the plan stopped, if it did.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The initiator's profile handler of type `T`.
    pub fn initiator<T: ProtocolHandler>(&self) -> &T {
        self.initiator
            .host
            .handler::<T>()
            .expect("the initiator was built with this handler")
    }

    /// The initiator's profile handler of type `T`, mutably.
    pub fn initiator_mut<T: ProtocolHandler>(&mut self) -> &mut T {
        self.initiator
            .host
            .handler_mut::<T>()
            .expect("the initiator was built with this handler")
    }

    /// The acceptor's profile handler of type `T`.
    pub fn acceptor<T: ProtocolHandler>(&self) -> &T {
        self.acceptor
            .host
            .handler::<T>()
            .expect("the acceptor was built with this handler")
    }

    /// The acceptor's profile handler of type `T`, mutably.
    pub fn acceptor_mut<T: ProtocolHandler>(&mut self) -> &mut T {
        self.acceptor
            .host
            .handler_mut::<T>()
            .expect("the acceptor was built with this handler")
    }

    /// The initiator's host, for assertions the plan does not cover.
    pub fn initiator_host(&self) -> &ClassicHost {
        &self.initiator.host
    }

    /// The acceptor's host.
    pub fn acceptor_host(&self) -> &ClassicHost {
        &self.acceptor.host
    }

    /// Advances the scene one step: both devices produce, the medium routes,
    /// both devices consume.
    pub fn tick(&mut self) {
        self.initiator.queue_start();
        self.acceptor.queue_start();

        self.advance();
        self.acceptor.pump_out();

        self.link.tick();

        if let Err(e) = self.initiator.pump_in() {
            self.fail(e);
        }
        if let Err(e) = self.acceptor.pump_in() {
            self.fail(e);
        }
    }

    /// Runs until `done` is true or `steps` have passed; returns whether it
    /// finished.
    pub fn run_until(&mut self, steps: usize, mut done: impl FnMut(&Self) -> bool) -> bool {
        for _ in 0..steps {
            if done(self) {
                return true;
            }
            self.tick();
        }
        done(self)
    }

    fn fail(&mut self, reason: impl Into<String>) {
        if self.error.is_none() {
            self.error = Some(reason.into());
            self.phase = LinkPhase::Failed;
        }
    }

    /// The transport plan. Past `Connected` the initiator's own `poll` is
    /// the engine: it drains each handler's channel requests and its queued
    /// PDUs.
    fn advance(&mut self) {
        let target = self.target;
        let packets = match self.phase {
            LinkPhase::Starting => {
                self.phase = LinkPhase::Inquiring;
                self.initiator.host.start_inquiry(1)
            }
            LinkPhase::Inquiring => {
                if !self.initiator.host.inquiry_finished() {
                    Vec::new()
                } else if self
                    .initiator
                    .host
                    .discovered()
                    .iter()
                    .any(|d| d.address == target)
                {
                    self.phase = LinkPhase::ResolvingName;
                    self.initiator.host.request_remote_name(target)
                } else {
                    self.fail(format!("inquiry did not find {target}"));
                    Vec::new()
                }
            }
            LinkPhase::ResolvingName => {
                if self.initiator.host.name_of(target).is_none() {
                    Vec::new()
                } else {
                    self.phase = LinkPhase::Paging;
                    self.initiator.host.create_connection(target)
                }
            }
            LinkPhase::Paging => {
                if self.initiator.host.connection().is_none() {
                    Vec::new()
                } else {
                    self.phase = LinkPhase::Connected;
                    Vec::new()
                }
            }
            LinkPhase::Connected | LinkPhase::Failed => Vec::new(),
        };
        for packet in packets {
            let _ = self.initiator.channel.inject_host_packet(packet);
        }
        self.initiator.pump_out();
    }
}
