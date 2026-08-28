// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! BR/EDR (classic) devices in a scene: the SPP client/server engine.
//!
//! [`ClassicDevice`] drives a classic connection — SDP then RFCOMM — as either
//! initiator or acceptor, the natively-testable engine behind the browser
//! bindings. Split out of the wasm transport so the scene engine and the car
//! kit can drive it directly.

use crate::transport::hci_adapter::HciChannel;
use crate::types::Address;

// ---------------------------------------------------------------------------
// BR/EDR devices in a scene
// ---------------------------------------------------------------------------

use crate::classic::rfcomm::RFCOMM_PSM;
use crate::classic::sdp::{SDP_PSM, SdpServer, SdpUuid, Service};
use crate::device::classic_host::{self, spp_service_record};
use crate::device::{
    ClassicHost, DiscoveredDevice, RfcommHandler, SdpHandler, SdpQueryHandler, SharedRfcommPort,
    SharedSdpQueryResults,
};

/// The Serial Port Profile service class (Assigned Numbers) — what an SPP
/// record advertises itself as, and what a client searches for.
pub(crate) const SERIAL_PORT_SERVICE_CLASS: SdpUuid = SdpUuid::Uuid16(0x1101);

/// How far a classic client has got through its plan.
///
/// The phases are the real BR/EDR connection sequence, in order, and each
/// one is entered only when the previous one's *event* arrived. That is the
/// point of naming them: a client stuck in `Paging` has not been refused, it
/// has been left without a Connection Complete — which is the failure this
/// whole layer exists to make visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassicPhase {
    /// Bring-up commands have not been queued yet.
    Starting,
    /// HCI Inquiry is running; waiting for Inquiry Complete.
    Inquiring,
    /// Resolving the names of what the inquiry found.
    ResolvingNames,
    /// Paging the target; waiting for Connection Complete.
    Paging,
    /// Opening the SDP channel and asking what the peer offers.
    QueryingSdp,
    /// Opening RFCOMM on the server channel SDP advertised.
    OpeningRfcomm,
    /// The data link is open and bytes are moving.
    Exchanging,
    /// Everything asked for was done and the link was torn down.
    Done,
    /// The plan could not continue; see `ClassicDevice::error`.
    Failed,
    /// This device answers rather than initiates, so it has no plan.
    Accepting,
}

impl ClassicPhase {
    /// Stable identifier for a UI or a status document.
    pub fn name(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Inquiring => "inquiring",
            Self::ResolvingNames => "resolving-names",
            Self::Paging => "paging",
            Self::QueryingSdp => "querying-sdp",
            Self::OpeningRfcomm => "opening-rfcomm",
            Self::Exchanging => "exchanging",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Accepting => "accepting",
        }
    }
}

/// One BR/EDR device in a [`SceneEngine`]: a [`ClassicHost`], the plan it is
/// following, and the handles a test or a page reads its progress from.
///
/// A device with no `target` is the *acceptor*: it makes itself discoverable
/// and connectable, serves SDP and RFCOMM, and waits. A device with a target
/// is the *initiator*, and runs the sequence a phone runs — inquire, resolve
/// the name, page, query SDP, open the advertised RFCOMM channel, exchange
/// data, disconnect.
pub struct ClassicDevice {
    host: ClassicHost,
    /// Who to connect to. `None` makes this an acceptor.
    target: Option<Address>,
    /// The Scan Enable this device brings up with. An acceptor that is not
    /// discoverable is a legitimate thing to want to test.
    scan_enable: u8,
    phase: ClassicPhase,
    /// The RFCOMM service class the client looks for in the peer's SDP.
    wanted_service: SdpUuid,
    sdp_results: Option<SharedSdpQueryResults>,
    port: Option<SharedRfcommPort>,
    /// What the client writes once the DLC opens.
    to_send: Vec<u8>,
    /// What came back over the serial port.
    received: Vec<Vec<u8>>,
    /// When set, the plan stops at [`ClassicPhase::Exchanging`] and stays
    /// there: the link is a *seam* for someone else — a profile holding the
    /// port — rather than a errand to run and finish. Without it the plan
    /// disconnects as soon as one payload comes back, which is right for the
    /// send-one-thing demo it was written for and fatal for a conversation.
    hold_open: bool,
    /// Set by [`Self::request_sco`]: open the audio connection as soon as
    /// there is an ACL to hang it off. Held as a *request* rather than acted
    /// on immediately because a profile decides there is audio long before
    /// this device's plan reaches a point where it can send HCI.
    sco_requested: bool,
    /// Whether *this* device opened the audio connection.
    ///
    /// Only the end that opened it may hang it up. Without this, a device
    /// that merely *accepted* an inbound synchronous request sees a link it
    /// never asked for and disconnects it — inside the same tick it came up,
    /// so from the outside the audio never appears at all and no layer
    /// reports an error.
    sco_opened_here: bool,
    /// Call audio queued for the synchronous link, waiting for it to exist.
    sco_to_send: Vec<Vec<u8>>,
    error: Option<String>,
}

impl ClassicDevice {
    /// An acceptor: discoverable, connectable, serving SDP plus an echoing
    /// RFCOMM port on `rfcomm_channel`, advertised in its SDP record under
    /// the Serial Port service class.
    pub fn acceptor(name: &str, class_of_device: [u8; 3], rfcomm_channel: u8) -> Self {
        let (rfcomm, port) = RfcommHandler::echoing(rfcomm_channel);
        Self::accepting(
            name,
            class_of_device,
            vec![(
                0x00010001,
                spp_service_record(0x00010001, rfcomm_channel, name),
            )],
            rfcomm,
            port,
        )
    }

    /// An acceptor that serves the SDP `records` it is given and an RFCOMM
    /// responder on `rfcomm_channel`, handing back the port so the *caller's*
    /// profile drives the serial connection.
    ///
    /// [`Self::acceptor`] is this with an SPP record and a port that echoes.
    /// A profile whose answer is not "the bytes you just sent" — HFP's Audio
    /// Gateway, for one — needs its own record and its own hand on the port,
    /// and that is the whole difference.
    pub fn serving(
        name: &str,
        class_of_device: [u8; 3],
        rfcomm_channel: u8,
        records: Vec<(u32, Service)>,
    ) -> (Self, SharedRfcommPort) {
        let port: SharedRfcommPort =
            std::sync::Arc::new(std::sync::Mutex::new(crate::device::RfcommPort::default()));
        let rfcomm = RfcommHandler::new(rfcomm_channel, port.clone());
        let mut device = Self::accepting(name, class_of_device, records, rfcomm, port.clone());
        device.hold_open = true;
        (device, port)
    }

    /// The shared body of [`Self::acceptor`] and [`Self::serving`].
    fn accepting(
        name: &str,
        class_of_device: [u8; 3],
        records: Vec<(u32, Service)>,
        rfcomm: RfcommHandler,
        port: SharedRfcommPort,
    ) -> Self {
        let mut host = ClassicHost::new(name, class_of_device);
        let mut sdp = SdpHandler::new(SdpServer::new());
        for (handle, record) in records {
            sdp.server_mut().service_records.insert(handle, record);
        }
        let _ = host.register_handler(Box::new(sdp));
        let _ = host.register_handler(Box::new(rfcomm));
        Self {
            host,
            target: None,
            scan_enable: classic_host::scan_enable::INQUIRY_AND_PAGE,
            phase: ClassicPhase::Accepting,
            wanted_service: SERIAL_PORT_SERVICE_CLASS,
            sdp_results: None,
            port: Some(port),
            to_send: Vec::new(),
            received: Vec::new(),
            hold_open: false,
            sco_requested: false,
            sco_opened_here: false,
            sco_to_send: Vec::new(),
            error: None,
        }
    }

    /// An initiator that discovers `target`, opens its Serial Port service
    /// and sends `payload` over it.
    pub fn initiator(
        name: &str,
        class_of_device: [u8; 3],
        target: Address,
        payload: impl Into<Vec<u8>>,
    ) -> Self {
        let mut device = Self::seeking(name, class_of_device, target, SERIAL_PORT_SERVICE_CLASS);
        device.to_send = payload.into();
        device
    }

    /// An initiator that discovers `target`, searches its SDP server for
    /// `service`, opens the RFCOMM channel that record advertises — and then
    /// **stops**, holding the data link open for the caller to drive through
    /// the port it returns.
    ///
    /// [`Self::initiator`] is a whole errand: find a serial port, say one
    /// thing on it, hang up. This is the same machinery with the errand
    /// removed, which is what a profile above RFCOMM needs — the conversation
    /// belongs to the profile, and the link must outlive the first payload
    /// rather than being torn down by it.
    pub fn client(
        name: &str,
        class_of_device: [u8; 3],
        target: Address,
        service: SdpUuid,
    ) -> (Self, SharedRfcommPort) {
        let port: SharedRfcommPort =
            std::sync::Arc::new(std::sync::Mutex::new(crate::device::RfcommPort::default()));
        let mut device = Self::seeking(name, class_of_device, target, service);
        device.hold_open = true;
        // The port is created here rather than in `advance_sdp`, so the
        // profile above can start writing into it before SDP has answered —
        // there is nowhere else to put bytes it produces while connecting.
        device.port = Some(port.clone());
        (device, port)
    }

    /// The shared body of [`Self::initiator`] and [`Self::client`]: a device
    /// that inquires for `target` and searches its SDP for `service`.
    fn seeking(name: &str, class_of_device: [u8; 3], target: Address, service: SdpUuid) -> Self {
        let mut host = ClassicHost::new(name, class_of_device);
        let (sdp, results) = SdpQueryHandler::searching_with_profile_version(service);
        let _ = host.register_handler(Box::new(sdp));
        Self {
            host,
            target: Some(target),
            // A client need be neither discoverable nor connectable: it is
            // the one doing the finding.
            scan_enable: classic_host::scan_enable::NONE,
            phase: ClassicPhase::Starting,
            wanted_service: service,
            sdp_results: Some(results),
            port: None,
            to_send: Vec::new(),
            received: Vec::new(),
            hold_open: false,
            sco_requested: false,
            sco_opened_here: false,
            sco_to_send: Vec::new(),
            error: None,
        }
    }

    /// This device's SDP query results, for a caller that wants to report
    /// what the search actually cost and found.
    pub fn sdp_results(&self) -> Option<&SharedSdpQueryResults> {
        self.sdp_results.as_ref()
    }

    /// The serial port this device's RFCOMM handler serves, once there is one.
    pub fn port(&self) -> Option<&SharedRfcommPort> {
        self.port.as_ref()
    }

    // --- the audio connection (SCO / eSCO) ---------------------------------

    /// Asks for the audio connection to be opened over this device's ACL.
    ///
    /// Only one end may do this — HFP gives the job to the Audio Gateway —
    /// and it is a request, not a command: the setup goes out on the next
    /// step at which there is an ACL to hang it off.
    pub fn request_sco(&mut self) {
        self.sco_requested = true;
        self.sco_opened_here = true;
    }

    /// Hangs up the audio, leaving the ACL and everything on it alone.
    pub fn release_sco(&mut self) {
        self.sco_requested = false;
        self.sco_to_send.clear();
    }

    /// The Voice Setting and packet types the next setup asks for — the
    /// codec seam (`AudioCodec::voice_setting`/`esco_packet_type`).
    pub fn set_sco_parameters(&mut self, voice_setting: u16, packet_type: u16) {
        self.host.set_sco_parameters(voice_setting, packet_type);
    }

    /// What this device answers an inbound synchronous Connection Request
    /// with.
    pub fn set_sco_policy(&mut self, policy: crate::device::ScoPolicy) {
        self.host.set_sco_policy(policy);
    }

    /// The audio connection, if one is up.
    pub fn sco(&self) -> Option<crate::device::ScoConnection> {
        self.host.sco()
    }

    /// Why the last audio setup failed, if the far end refused it.
    pub fn sco_failure(&self) -> Option<u8> {
        self.host.sco_failure()
    }

    /// Queues one payload for the synchronous link. It waits if the link is
    /// not up yet, rather than being dropped: a profile that starts talking
    /// the instant it decides there is audio is right to, and the frames it
    /// produced while the setup was in flight are still the call.
    pub fn send_sco(&mut self, payload: impl Into<Vec<u8>>) {
        self.sco_to_send.push(payload.into());
    }

    /// Takes the call audio that has arrived on the synchronous link.
    pub fn take_sco_received(&mut self) -> Vec<Vec<u8>> {
        self.host.take_sco_received()
    }

    /// Makes this device's Scan Enable `value` — used to build a device that
    /// is deliberately not discoverable.
    pub fn with_scan_enable(mut self, value: u8) -> Self {
        self.scan_enable = value;
        self
    }

    /// How far the plan has got.
    pub fn phase(&self) -> ClassicPhase {
        self.phase
    }

    /// Why the plan stopped, if it did.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The devices this device's inquiry turned up.
    pub fn discovered(&self) -> &[DiscoveredDevice] {
        self.host.discovered()
    }

    /// The underlying host, for assertions a plan does not cover.
    pub fn host(&self) -> &ClassicHost {
        &self.host
    }

    /// What arrived over the serial port.
    pub fn received(&self) -> &[Vec<u8>] {
        &self.received
    }

    /// Queues this device's bring-up on its channel.
    pub(crate) fn queue_start(&mut self, channel: &HciChannel) {
        for packet in self.host.start_commands() {
            let _ = channel.inject_host_packet(packet);
        }
        for packet in self.host.set_scan_enable(self.scan_enable) {
            let _ = channel.inject_host_packet(packet);
        }
        self.phase = match self.target {
            Some(_) => ClassicPhase::Starting,
            None => ClassicPhase::Accepting,
        };
    }

    fn fail(&mut self, reason: impl Into<String>) {
        self.error = Some(reason.into());
        self.phase = ClassicPhase::Failed;
    }

    /// Advance the plan one step, emitting whatever HCI it asks for.
    ///
    /// Every transition is gated on something the *controller* said, never
    /// on a tick count: that is what makes a stalled step visible as a phase
    /// that stops moving rather than as a scene that silently drifts on.
    pub(crate) fn produce(&mut self, channel: &HciChannel) {
        // The audio connection is orthogonal to the plan below: it hangs off
        // whatever ACL exists, and either role may be the one holding it, so
        // it runs before the acceptor's early return rather than inside the
        // initiator's state machine.
        self.produce_sco(channel);

        let Some(target) = self.target else {
            // An acceptor still has to drain what its profiles want to send.
            for packet in self.host.poll() {
                let _ = channel.inject_host_packet(packet);
            }
            return;
        };

        let packets = match self.phase {
            ClassicPhase::Starting => {
                self.phase = ClassicPhase::Inquiring;
                self.host.start_inquiry(1)
            }
            ClassicPhase::Inquiring => {
                if !self.host.inquiry_finished() {
                    Vec::new()
                } else if self.host.discovered().iter().any(|d| d.address == target) {
                    self.phase = ClassicPhase::ResolvingNames;
                    self.host.request_remote_name(target)
                } else {
                    self.fail(format!("inquiry did not find {target}"));
                    Vec::new()
                }
            }
            ClassicPhase::ResolvingNames => {
                if self.host.name_of(target).is_none() {
                    Vec::new()
                } else {
                    self.phase = ClassicPhase::Paging;
                    self.host.create_connection(target)
                }
            }
            ClassicPhase::Paging => {
                if self.host.connection().is_none() {
                    Vec::new()
                } else {
                    self.phase = ClassicPhase::QueryingSdp;
                    // The SDP query itself leaves on its own, from the
                    // handler's `poll_output`, once this channel opens.
                    self.host.open_channel(SDP_PSM).unwrap_or_default()
                }
            }
            ClassicPhase::QueryingSdp => self.advance_sdp(),
            ClassicPhase::OpeningRfcomm => {
                let open = self
                    .port
                    .as_ref()
                    .and_then(|port| port.lock().ok().map(|port| port.is_open()))
                    .unwrap_or(false);
                if !open {
                    Vec::new()
                } else {
                    self.phase = ClassicPhase::Exchanging;
                    let payload = std::mem::take(&mut self.to_send);
                    // An empty write is a zero-length UIH frame the peer has
                    // to make sense of; a client with nothing to say of its
                    // own should say nothing.
                    if !payload.is_empty()
                        && let Some(port) = self.port.as_ref()
                        && let Ok(mut port) = port.lock()
                    {
                        port.write(payload);
                    }
                    Vec::new()
                }
            }
            ClassicPhase::Exchanging => {
                self.drain_port();
                if self.hold_open || self.received.is_empty() {
                    Vec::new()
                } else {
                    self.phase = ClassicPhase::Done;
                    self.host.disconnect()
                }
            }
            ClassicPhase::Done | ClassicPhase::Failed | ClassicPhase::Accepting => Vec::new(),
        };

        for packet in packets {
            let _ = channel.inject_host_packet(packet);
        }
        // Profiles speak unprompted too — the RFCOMM initiator's SABM leaves
        // this way, not from the plan above.
        for packet in self.host.poll() {
            let _ = channel.inject_host_packet(packet);
        }
    }

    /// The audio connection's step: open it when it has been asked for and
    /// there is an ACL under it, hang it up when the request is withdrawn,
    /// and drain whatever audio is queued for it.
    ///
    /// Every transition here is gated on what the *controller* said, like
    /// every other stage: `host.sco()` is `Some` only once a Synchronous
    /// Connection Complete has arrived, so a setup that is refused leaves
    /// this loop trying again rather than reporting audio nobody agreed to.
    fn produce_sco(&mut self, channel: &HciChannel) {
        let up = self.host.sco().is_some();
        if self.sco_requested && !up && self.host.sco_failure().is_some() {
            // The far end refused. Asking again every step would busy the
            // link and hide the refusal behind a setup that is always "in
            // flight"; the request is withdrawn and the reason stays
            // readable on the host.
            self.sco_requested = false;
            self.sco_opened_here = false;
            self.sco_to_send.clear();
        }
        let packets = if self.sco_requested && !up {
            self.host.setup_sco()
        } else if !self.sco_requested && up && self.sco_opened_here {
            self.sco_opened_here = false;
            self.host.disconnect_sco()
        } else {
            Vec::new()
        };
        for packet in packets {
            let _ = channel.inject_host_packet(packet);
        }
        if self.host.sco().is_some() {
            for payload in std::mem::take(&mut self.sco_to_send) {
                for packet in self.host.send_sco(&payload) {
                    let _ = channel.inject_host_packet(packet);
                }
            }
        }
    }

    /// The SDP stage: wait for the answer, then register an RFCOMM initiator
    /// on the server channel the peer advertised and open its L2CAP channel.
    ///
    /// The handler cannot be registered any earlier: which channel to open a
    /// DLC on is precisely what the SDP query is for, and guessing it is how
    /// a client ends up with a DLC refused by DM.
    fn advance_sdp(&mut self) -> Vec<Vec<u8>> {
        let Some(results) = self.sdp_results.as_ref() else {
            self.fail("no SDP results handle");
            return Vec::new();
        };
        let Ok(results) = results.lock() else {
            return Vec::new();
        };
        if !results.answered {
            return Vec::new();
        }
        if let Some(code) = results.error {
            drop(results);
            self.fail(format!("peer's SDP server returned error {code:#06x}"));
            return Vec::new();
        }
        let Some(rfcomm_channel) = results.channel_for(self.wanted_service) else {
            drop(results);
            self.fail("peer advertises no Serial Port service".to_string());
            return Vec::new();
        };
        drop(results);

        // Reuse the port the caller was already given, if there is one: a
        // profile holding a clone of it must not be handed a *different*
        // port once SDP answers, or everything it queued goes nowhere.
        let port = self.port.clone().unwrap_or_else(|| {
            std::sync::Arc::new(std::sync::Mutex::new(crate::device::RfcommPort::default()))
        });
        let rfcomm = RfcommHandler::initiating(rfcomm_channel, port.clone());
        if let Err(e) = self.host.register_handler(Box::new(rfcomm)) {
            self.fail(e.to_string());
            return Vec::new();
        }
        self.port = Some(port);
        self.phase = ClassicPhase::OpeningRfcomm;
        match self.host.open_channel(RFCOMM_PSM) {
            Ok(packets) => packets,
            Err(e) => {
                self.fail(e.to_string());
                Vec::new()
            }
        }
    }

    /// Move anything the serial port received into this device's record of it.
    ///
    /// A device holding the link open for a profile above it must *not* do
    /// this: the port is the seam, and taking from it here would swallow the
    /// bytes the profile is waiting for. That is not hypothetical — it is
    /// what a plan written for "send one payload and read the echo" does to
    /// its second consumer, silently, with the link up and every phase green.
    fn drain_port(&mut self) {
        if self.hold_open {
            return;
        }
        if let Some(port) = self.port.as_ref()
            && let Ok(mut port) = port.lock()
        {
            self.received.extend(port.take_received());
        }
    }

    /// Everything a page renders about this device's BR/EDR link, as JSON:
    /// the phase, what the inquiry turned up, the ACL connection, and the
    /// state of the serial port on top of it.
    ///
    /// A BR/EDR link has no equivalent of an advertising report to look at,
    /// so a page that shows nothing but "connected" cannot say *how* it
    /// connected — which stage it is stuck in, or whether the peer was even
    /// found. That is what this exists to make visible.
    pub fn status_json(&self) -> String {
        #[derive(serde::Serialize)]
        struct FoundJson {
            address: String,
            class_of_device: String,
            name: Option<String>,
        }
        #[derive(serde::Serialize)]
        struct DlcJson {
            dlci: u8,
            tx_max_frame_size: u16,
            rx_max_frame_size: u16,
            rx_initial_credits: u8,
            credits_out: u8,
            credits_in: u8,
        }
        #[derive(serde::Serialize)]
        struct ClassicJson {
            phase: &'static str,
            error: Option<String>,
            name: String,
            discovered: Vec<FoundJson>,
            acl_handle: Option<u16>,
            peer: Option<String>,
            sdp_channel: Option<u8>,
            sdp_profile_version: Option<u16>,
            sdp_request_bytes: usize,
            sdp_response_bytes: usize,
            dlc: Option<DlcJson>,
            received: usize,
        }

        let connection = self.host.connection();
        let results = self.sdp_results.as_ref().and_then(|r| r.lock().ok());
        let port = self.port.as_ref().and_then(|p| p.lock().ok());
        let status = ClassicJson {
            phase: self.phase.name(),
            error: self.error.clone(),
            name: self.host.name().to_string(),
            discovered: self
                .host
                .discovered()
                .iter()
                .map(|d| FoundJson {
                    address: d.address.to_string(),
                    class_of_device: format!(
                        "{:02X}{:02X}{:02X}",
                        d.class_of_device[2], d.class_of_device[1], d.class_of_device[0]
                    ),
                    name: d.name.clone(),
                })
                .collect(),
            acl_handle: connection.map(|(handle, _)| handle),
            peer: connection.map(|(_, address)| address.to_string()),
            sdp_channel: results
                .as_ref()
                .and_then(|r| r.channel_for(self.wanted_service)),
            sdp_profile_version: results.as_ref().and_then(|r| r.profile_version),
            sdp_request_bytes: results.as_ref().map(|r| r.request_bytes).unwrap_or(0),
            sdp_response_bytes: results.as_ref().map(|r| r.response_bytes).unwrap_or(0),
            dlc: port.as_ref().and_then(|p| p.window()).map(|w| DlcJson {
                dlci: w.dlci,
                tx_max_frame_size: w.tx_max_frame_size,
                rx_max_frame_size: w.rx_max_frame_size,
                rx_initial_credits: w.rx_initial_credits,
                credits_out: w.tx_credits,
                credits_in: w.rx_credits,
            }),
            received: port.as_ref().map(|p| p.received_count()).unwrap_or(0),
        };
        serde_json::to_string(&status).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
    }

    /// Feed one controller packet to the host and send back what it answers.
    pub(crate) fn consume(&mut self, channel: &HciChannel, packet: &[u8]) {
        match self.host.handle_packet(packet) {
            Ok(out) => {
                for reply in out {
                    let _ = channel.inject_host_packet(reply);
                }
            }
            Err(e) => self.fail(e.to_string()),
        }
        self.drain_port();
    }
}
