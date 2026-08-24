// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Live BR/EDR **initiator** against a running netsimd: the sequence a phone
//! runs when you tap an unpaired device — inquire, resolve the name, page,
//! open SDP, read the RFCOMM channel out of the peer's service record, open
//! the data link, exchange bytes, disconnect.
//!
//! [`examples/classic_discoverable`](classic_discoverable) is the other half
//! of this: it *answers*. Everything here is the half that has only ever been
//! validated against simble's own simulated controller, which is why this
//! example exists — pointed at a foreign peer (Bumble, an Android emulator)
//! it is the first thing that proves the Inquiry / Remote Name Request /
//! Create Connection parameter layouts and the SDP client are right.
//!
//! Its exit status is the verdict: 0 only if every stage the caller asked
//! for was reached *and* the foreign facts matched.
//!
//! ```text
//! netsimd --logtostderr --no-shutdown --ws-port 7681
//! cargo run --example classic_initiator -- AA:BB:CC:00:00:02
//! ```
//!
//! Expectations come from the environment so the caller can assert on facts
//! only the *peer* knows:
//!
//! | Variable | Meaning |
//! |---|---|
//! | `SIMBLE_EXPECT_NAME` | the name the Remote Name Request must return |
//! | `SIMBLE_EXPECT_COD` | the Class of Device the inquiry result must carry, `0x240404` style |
//! | `SIMBLE_EXPECT_CHANNEL` | the RFCOMM server channel the peer's SDP must advertise |
//! | `SIMBLE_SPP_PAYLOAD` | what to write on the DLC (default `hello serial`) |
//! | `SIMBLE_EXPECT_ECHO` | what must come back (default: the payload) |
//! | `SIMBLE_INQUIRY_MODE` | `standard`, `rssi` or `eir` (default `standard`) |
//! | `SIMBLE_EXPECT_INQUIRY_EVENT` | the event code the results must arrive as, e.g. `0x2F` |
//! | `SIMBLE_TIMEOUT` | seconds before a stalled stage is a failure (default 30) |
//! | `SIMBLE_BTSNOOP` | write a btsnoop capture of the whole run here |
//! | `SIMBLE_PAIR` | `1` to pair and encrypt after paging, before SDP |
//! | `SIMBLE_IO_CAPABILITY` | `displayyesno` (default), `displayonly`, `keyboardonly`, `none` |
//! | `SIMBLE_MITM` | `1` to set the MITM-protection-required bit |
//! | `SIMBLE_EXPECT_AUTHENTICATED_KEY` | `1` to require the resulting key type be an authenticated one |
//!
//! With `SIMBLE_PAIR=1` the run inserts two stages between the page and the
//! SDP query: Authentication Requested and Set Connection Encryption. That is
//! the sequence a phone runs the first time you tap an unpaired device, and
//! it is the one that a *foreign* peer has to agree with — every event in it
//! (IO Capability Request, IO Capability Response, User Confirmation Request,
//! Link Key Notification, Simple Pairing Complete, Authentication Complete,
//! Encryption Change) is produced by the peer's controller and answered by
//! this host, so a wrong parameter layout on either side fails here and
//! passes against simble's own simulated controller.

use simble::classic::rfcomm::RFCOMM_PSM;
use simble::classic::sdp::{SDP_PSM, SdpUuid};
use simble::device::classic_host::{
    RfcommHandler, SdpQueryHandler, SharedRfcommPort, SharedSdpQueryResults,
    authentication_requirements, inquiry_mode, io_capability, scan_enable,
};
use simble::device::{ClassicHost, DiscoveredDevice};
use simble::transport::{HciChannel, HciTransport, LiveTransport};
use simble::types::Address;
use std::str::FromStr;
use std::time::{Duration, Instant};

/// Serial Port Profile service class (Assigned Numbers) — what an SPP record
/// advertises itself as, and what the SDP query searches for.
const SERIAL_PORT_SERVICE_CLASS: SdpUuid = SdpUuid::Uuid16(0x1101);

/// How far the plan has got. Named stages, because the useful failure report
/// is "it stopped at X", not "it timed out".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Starting,
    Inquiring,
    ResolvingName,
    Paging,
    Authenticating,
    Encrypting,
    QueryingSdp,
    OpeningRfcomm,
    Exchanging,
    Disconnecting,
    Done,
}

impl Phase {
    fn label(self) -> &'static str {
        match self {
            Phase::Starting => "bring-up",
            Phase::Inquiring => "inquiry",
            Phase::ResolvingName => "remote name request",
            Phase::Paging => "create connection",
            Phase::Authenticating => "pairing (authentication requested)",
            Phase::Encrypting => "set connection encryption",
            Phase::QueryingSdp => "SDP query",
            Phase::OpeningRfcomm => "RFCOMM DLC open",
            Phase::Exchanging => "data exchange",
            Phase::Disconnecting => "disconnect",
            Phase::Done => "done",
        }
    }
}

/// Parses `SIMBLE_IO_CAPABILITY`. The value decides which association model
/// the two controllers pick, which is the one knob that changes whether the
/// bond resists a man in the middle — so it is spelled out rather than
/// hardcoded.
fn parse_io_capability(text: &str) -> Result<u8, String> {
    match text.trim().to_ascii_lowercase().as_str() {
        "displayonly" => Ok(io_capability::DISPLAY_ONLY),
        "displayyesno" => Ok(io_capability::DISPLAY_YES_NO),
        "keyboardonly" => Ok(io_capability::KEYBOARD_ONLY),
        "none" | "noinputnooutput" => Ok(io_capability::NO_INPUT_NO_OUTPUT),
        other => Err(format!("unknown SIMBLE_IO_CAPABILITY: {other}")),
    }
}

/// Whether an environment variable is set to something that means yes.
fn env_flag(name: &str) -> bool {
    matches!(
        std::env::var(name).as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

/// What the caller asked us to prove about the *peer*.
struct Expectations {
    name: Option<String>,
    class_of_device: Option<[u8; 3]>,
    rfcomm_channel: Option<u8>,
    payload: Vec<u8>,
    echo: Vec<u8>,
    timeout: Duration,
    /// The Inquiry Mode to ask the controller for, if any.
    inquiry_mode: Option<u8>,
    /// The event code the inquiry results must arrive as.
    inquiry_event: Option<u8>,
    /// Whether to pair and encrypt between the page and the SDP query.
    pair: bool,
    /// What to claim in IO Capability Request Reply.
    io_capability: u8,
    /// The Authentication_Requirements to ask for.
    authentication_requirements: u8,
    /// Whether the resulting link key must be an authenticated one — which is
    /// only true if the association model put a person in the loop.
    expect_authenticated_key: bool,
}

impl Expectations {
    fn from_env() -> Result<Self, String> {
        let payload = std::env::var("SIMBLE_SPP_PAYLOAD").unwrap_or_else(|_| "hello serial".into());
        let echo = std::env::var("SIMBLE_EXPECT_ECHO").unwrap_or_else(|_| payload.clone());
        Ok(Self {
            name: std::env::var("SIMBLE_EXPECT_NAME").ok(),
            class_of_device: match std::env::var("SIMBLE_EXPECT_COD") {
                Ok(text) => Some(parse_class_of_device(&text)?),
                Err(_) => None,
            },
            rfcomm_channel: match std::env::var("SIMBLE_EXPECT_CHANNEL") {
                Ok(text) => Some(
                    text.trim()
                        .parse()
                        .map_err(|_| format!("SIMBLE_EXPECT_CHANNEL is not a number: {text}"))?,
                ),
                Err(_) => None,
            },
            payload: payload.into_bytes(),
            echo: echo.into_bytes(),
            timeout: Duration::from_secs(
                std::env::var("SIMBLE_TIMEOUT")
                    .ok()
                    .and_then(|t| t.parse().ok())
                    .unwrap_or(30),
            ),
            inquiry_mode: match std::env::var("SIMBLE_INQUIRY_MODE").as_deref() {
                Ok("standard") => Some(inquiry_mode::STANDARD),
                Ok("rssi") => Some(inquiry_mode::WITH_RSSI),
                Ok("eir") => Some(inquiry_mode::WITH_EXTENDED),
                Ok(other) => return Err(format!("unknown SIMBLE_INQUIRY_MODE: {other}")),
                Err(_) => None,
            },
            inquiry_event: match std::env::var("SIMBLE_EXPECT_INQUIRY_EVENT") {
                Ok(text) => {
                    let cleaned = text
                        .trim()
                        .trim_start_matches("0x")
                        .trim_start_matches("0X");
                    Some(u8::from_str_radix(cleaned, 16).map_err(|_| {
                        format!("SIMBLE_EXPECT_INQUIRY_EVENT is not a hex byte: {text}")
                    })?)
                }
                Err(_) => None,
            },
            pair: env_flag("SIMBLE_PAIR"),
            io_capability: match std::env::var("SIMBLE_IO_CAPABILITY") {
                Ok(text) => parse_io_capability(&text)?,
                Err(_) => io_capability::DISPLAY_YES_NO,
            },
            authentication_requirements: if env_flag("SIMBLE_MITM") {
                authentication_requirements::GENERAL_BONDING_MITM
            } else {
                authentication_requirements::GENERAL_BONDING
            },
            expect_authenticated_key: env_flag("SIMBLE_EXPECT_AUTHENTICATED_KEY"),
        })
    }
}

/// Parses `0x240404` (or `240404`) into the three octets a Class of Device
/// occupies on the wire, least-significant first — the order an Inquiry
/// Result reports it in.
fn parse_class_of_device(text: &str) -> Result<[u8; 3], String> {
    let cleaned = text
        .trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X");
    let value = u32::from_str_radix(cleaned, 16)
        .map_err(|_| format!("SIMBLE_EXPECT_COD is not hex: {text}"))?;
    if value > 0xFF_FFFF {
        return Err(format!("Class of Device does not fit in 24 bits: {text}"));
    }
    Ok([value as u8, (value >> 8) as u8, (value >> 16) as u8])
}

/// Renders a Class of Device the way it is written down, most-significant
/// nibble first, from the wire order.
fn class_of_device_string(cod: [u8; 3]) -> String {
    format!("{:#08x}", u32::from_le_bytes([cod[0], cod[1], cod[2], 0]))
}

/// One check, reported the way the interop scripts report them: a line per
/// fact, and the first failure ends the run.
fn check(ok: bool, message: impl std::fmt::Display) -> bool {
    if ok {
        println!("ok    {message}");
    } else {
        println!("FAIL  {message}");
    }
    ok
}

/// The foreign bytes worth keeping. Everything printed here is meant to be
/// pasted into `tests/classic_foreign_bytes_test.rs` as a const, so a
/// regression in the parsers is caught by `cargo test` with no netsim in
/// sight.
#[derive(Default)]
struct Captured {
    /// Which of the three inquiry-result event codes the controller used.
    inquiry_event: Option<u8>,
    inquiry_result: Option<Vec<u8>>,
    remote_name: Option<Vec<u8>>,
    connection_complete: Option<Vec<u8>>,
    sdp_response: Vec<Vec<u8>>,
    /// The security events the *peer's* controller produced. These are the
    /// bytes no simble-to-simble test can vouch for: an IO Capability
    /// Response whose fields sit where we think they do, a Link Key
    /// Notification with the key type a foreign stack chose.
    io_capability_response: Option<Vec<u8>>,
    user_confirmation_request: Option<Vec<u8>>,
    link_key_notification: Option<Vec<u8>>,
    simple_pairing_complete: Option<Vec<u8>>,
    encryption_change: Option<Vec<u8>>,
}

impl Captured {
    /// Records the first instance of each interesting controller packet.
    /// Only the first: a repeat is the same layout, and a wall of hex is
    /// nobody's idea of a capture.
    fn observe(&mut self, packet: &[u8]) {
        match packet {
            // HCI Event: code, then a length and its parameters.
            [0x04, code, ..] => match code {
                // Inquiry Result / with RSSI / Extended Inquiry Result.
                0x02 | 0x22 | 0x2F if self.inquiry_result.is_none() => {
                    self.inquiry_event = Some(*code);
                    self.inquiry_result = Some(packet.to_vec());
                }
                0x07 if self.remote_name.is_none() => self.remote_name = Some(packet.to_vec()),
                0x03 if self.connection_complete.is_none() => {
                    self.connection_complete = Some(packet.to_vec());
                }
                0x32 if self.io_capability_response.is_none() => {
                    self.io_capability_response = Some(packet.to_vec());
                }
                0x33 if self.user_confirmation_request.is_none() => {
                    self.user_confirmation_request = Some(packet.to_vec());
                }
                0x18 if self.link_key_notification.is_none() => {
                    self.link_key_notification = Some(packet.to_vec());
                }
                0x36 if self.simple_pairing_complete.is_none() => {
                    self.simple_pairing_complete = Some(packet.to_vec());
                }
                0x08 if self.encryption_change.is_none() => {
                    self.encryption_change = Some(packet.to_vec());
                }
                _ => {}
            },
            // ACL data (H4 type 0x02): ACL header(4) + L2CAP header(4), then
            // the SDU. SDP PDU id 0x07 is a ServiceSearchAttributeResponse
            // and 0x01 an error response — both are answers worth keeping.
            // Only complete first fragments are looked at; a continuation
            // fragment has no L2CAP header to skip.
            [0x02, rest @ ..] if rest.len() > 8 && (rest[1] >> 4) & 0x03 != 0x01 => {
                let sdu = &rest[8..];
                // An SDP PDU is id(1) + transaction(2) + length(2) + body,
                // and the length has to account for the rest exactly — an
                // RFCOMM UIH frame also starts 0x01 often enough to matter.
                let declared = sdu
                    .get(3..5)
                    .map(|len| usize::from(u16::from_be_bytes([len[0], len[1]])));
                if matches!(sdu.first(), Some(0x07 | 0x01))
                    && declared == Some(sdu.len().saturating_sub(5))
                {
                    self.sdp_response.push(sdu.to_vec());
                }
            }
            _ => {}
        }
    }

    fn report(&self) {
        println!("\n--- captured foreign bytes ---");
        for (label, bytes) in [
            ("inquiry result event", self.inquiry_result.as_deref()),
            ("remote name complete event", self.remote_name.as_deref()),
            (
                "connection complete event",
                self.connection_complete.as_deref(),
            ),
            (
                "io capability response event",
                self.io_capability_response.as_deref(),
            ),
            (
                "user confirmation request event",
                self.user_confirmation_request.as_deref(),
            ),
            (
                "link key notification event",
                self.link_key_notification.as_deref(),
            ),
            (
                "simple pairing complete event",
                self.simple_pairing_complete.as_deref(),
            ),
            ("encryption change event", self.encryption_change.as_deref()),
        ] {
            match bytes {
                Some(bytes) => println!("capture {label}: {}", hex(bytes)),
                None => println!("capture {label}: (none seen)"),
            }
        }
        for (index, sdu) in self.sdp_response.iter().enumerate() {
            println!("capture sdp response #{index}: {}", hex(sdu));
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

/// The whole run, as a state machine driven by what the controller said.
/// Nothing advances on a tick count — a stage that stops moving is a stalled
/// stage, and the timeout names it.
struct Initiator {
    host: ClassicHost,
    target: Address,
    phase: Phase,
    phase_since: Instant,
    sdp_results: SharedSdpQueryResults,
    port: Option<SharedRfcommPort>,
    rfcomm_channel: Option<u8>,
    received: Vec<Vec<u8>>,
    sent: bool,
    /// Whether to pair and encrypt before the SDP query.
    pair: bool,
    /// The link's security as it stood once pairing and encryption were
    /// done, snapshotted there because [`ClassicHost::security`] describes
    /// the *live* link and this run ends by tearing that link down. Reading
    /// it in the verdict would report the security of a connection that no
    /// longer exists, which is to say none at all.
    security_after_pairing: Option<simble::device::LinkSecurity>,
    /// The peer's name as the *inquiry alone* knew it, snapshotted the
    /// moment the target was found and before any Remote Name Request went
    /// out. Only an Extended Inquiry Result can fill this in.
    name_from_inquiry: Option<String>,
}

impl Initiator {
    fn new(name: &str, class_of_device: [u8; 3], target: Address, expect: &Expectations) -> Self {
        let mut host = ClassicHost::new(name, class_of_device);
        let (sdp, sdp_results) = SdpQueryHandler::searching(SERIAL_PORT_SERVICE_CLASS);
        host.register_handler(Box::new(sdp))
            .expect("the SDP client registers");
        host.set_io_capability(expect.io_capability, expect.authentication_requirements);
        Self {
            host,
            pair: expect.pair,
            target,
            phase: Phase::Starting,
            phase_since: Instant::now(),
            sdp_results,
            port: None,
            rfcomm_channel: None,
            received: Vec::new(),
            sent: false,
            security_after_pairing: None,
            name_from_inquiry: None,
        }
    }

    fn enter(&mut self, phase: Phase) {
        println!("  -> {}", phase.label());
        self.phase = phase;
        self.phase_since = Instant::now();
    }

    /// Advance one step, returning the HCI to send. `Err` is a stage that
    /// failed outright rather than one still waiting.
    fn step(&mut self, payload: &[u8]) -> Result<Vec<Vec<u8>>, String> {
        match self.phase {
            Phase::Starting => {
                self.enter(Phase::Inquiring);
                // 1 × 1.28 s is the shortest inquiry the spec allows and is
                // plenty on a simulated ether; a real one wants longer.
                Ok(self.host.start_inquiry(4))
            }
            Phase::Inquiring => {
                // Snapshot before asking: an EIR name and a Remote Name
                // Request answer land in the same field, and after the
                // request there is no telling which one is being read.
                if let Some(name) = self.found().map(|d| d.name.clone()) {
                    self.name_from_inquiry = name;
                    if self.name_from_inquiry.is_some() {
                        // The inquiry already told us. A phone does not page
                        // a device to ask a question it has the answer to,
                        // and neither does this.
                        self.enter(Phase::Paging);
                        return Ok(self.host.create_connection(self.target));
                    }
                    self.enter(Phase::ResolvingName);
                    return Ok(self.host.request_remote_name(self.target));
                }
                if self.host.inquiry_finished() {
                    return Err(format!(
                        "inquiry completed without finding {}; it found {:?}",
                        self.target,
                        self.host
                            .discovered()
                            .iter()
                            .map(|d| d.address.to_string())
                            .collect::<Vec<_>>()
                    ));
                }
                Ok(Vec::new())
            }
            Phase::ResolvingName => {
                if self.host.name_of(self.target).is_none() {
                    return Ok(Vec::new());
                }
                self.enter(Phase::Paging);
                Ok(self.host.create_connection(self.target))
            }
            Phase::Paging => {
                if self.host.connection().is_none() {
                    return Ok(Vec::new());
                }
                if self.pair {
                    self.enter(Phase::Authenticating);
                    // Everything after this is the peer's controller asking
                    // this host questions; `ClassicHost::handle_packet`
                    // answers them, and nothing here has to drive it.
                    return Ok(self.host.authenticate());
                }
                self.enter(Phase::QueryingSdp);
                // The query itself leaves from the handler's `poll_output`
                // once this channel opens; all this does is open it.
                self.host.open_channel(SDP_PSM).map_err(|e| e.to_string())
            }
            Phase::Authenticating => {
                let security = self.host.security();
                if let Some(status) = security.pairing_status.filter(|s| *s != 0x00) {
                    return Err(format!(
                        "the peer refused pairing: Simple Pairing Complete \
                         status {status:#04x}"
                    ));
                }
                if !security.authenticated {
                    return Ok(Vec::new());
                }
                if let Some(value) = security.numeric_value {
                    println!("  the peer's controller showed {value:06} for comparison");
                }
                if let Some(key) = self.host.link_key(self.target) {
                    println!(
                        "  bonded: key type {:#04x} ({})",
                        key.key_type,
                        if key.is_authenticated() {
                            "authenticated"
                        } else {
                            "unauthenticated"
                        }
                    );
                }
                self.enter(Phase::Encrypting);
                Ok(self.host.encrypt(true))
            }
            Phase::Encrypting => {
                if !self.host.security().encrypted {
                    return Ok(Vec::new());
                }
                // Snapshot before anything can tear the link down: this is
                // the only moment the whole security picture is true at once.
                self.security_after_pairing = Some(self.host.security());
                self.enter(Phase::QueryingSdp);
                self.host.open_channel(SDP_PSM).map_err(|e| e.to_string())
            }
            Phase::QueryingSdp => self.advance_sdp(),
            Phase::OpeningRfcomm => {
                let open = self
                    .port
                    .as_ref()
                    .and_then(|port| port.lock().ok().map(|port| port.is_open()))
                    .unwrap_or(false);
                if !open {
                    return Ok(Vec::new());
                }
                self.enter(Phase::Exchanging);
                if let Some(port) = self.port.as_ref()
                    && let Ok(mut port) = port.lock()
                {
                    port.write(payload.to_vec());
                    self.sent = true;
                }
                Ok(Vec::new())
            }
            Phase::Exchanging => {
                self.drain_port();
                if self.received.is_empty() {
                    return Ok(Vec::new());
                }
                self.enter(Phase::Disconnecting);
                Ok(self.host.disconnect())
            }
            Phase::Disconnecting => {
                if self.host.connection().is_some() {
                    return Ok(Vec::new());
                }
                self.enter(Phase::Done);
                Ok(Vec::new())
            }
            Phase::Done => Ok(Vec::new()),
        }
    }

    /// The SDP stage: wait for the peer's answer, then open RFCOMM on the
    /// channel it named. Which channel is precisely what the query is for —
    /// guessing it is how a client ends up with a DLC refused by DM.
    fn advance_sdp(&mut self) -> Result<Vec<Vec<u8>>, String> {
        let channel = {
            let Ok(results) = self.sdp_results.lock() else {
                return Ok(Vec::new());
            };
            if !results.answered {
                return Ok(Vec::new());
            }
            if let Some(code) = results.error {
                return Err(format!("the peer's SDP server answered error {code:#06x}"));
            }
            match results.channel_for(SERIAL_PORT_SERVICE_CLASS) {
                Some(channel) => channel,
                None => {
                    return Err(format!(
                        "the peer's SDP answer advertises no Serial Port service; it \
                         offered {:?}",
                        results.rfcomm_channels
                    ));
                }
            }
        };
        println!("  peer's SDP advertises RFCOMM server channel {channel}");
        self.rfcomm_channel = Some(channel);

        let (rfcomm, port) = RfcommHandler::initiator(channel);
        self.host
            .register_handler(Box::new(rfcomm))
            .map_err(|e| e.to_string())?;
        self.port = Some(port);
        self.enter(Phase::OpeningRfcomm);
        self.host
            .open_channel(RFCOMM_PSM)
            .map_err(|e| e.to_string())
    }

    fn drain_port(&mut self) {
        if let Some(port) = self.port.as_ref()
            && let Ok(mut port) = port.lock()
        {
            self.received.extend(port.take_received());
        }
    }

    fn found(&self) -> Option<&DiscoveredDevice> {
        self.host
            .discovered()
            .iter()
            .find(|d| d.address == self.target)
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(target) = args.next() else {
        eprintln!(
            "usage: classic_initiator <peer-address>\n\
             (the peer's BD_ADDR in display order, e.g. AA:BB:CC:00:00:02)"
        );
        std::process::exit(2);
    };
    let target = match Address::from_str(&target) {
        Ok(address) => address,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let expect = match Expectations::from_env() {
        Ok(expect) => expect,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };

    let name = std::env::var("SIMBLE_NAME").unwrap_or_else(|_| "simble-initiator".to_string());
    let address = std::env::var("SIMBLE_ADDR").unwrap_or_else(|_| "F0:DE:C0:00:0C:02".to_string());
    let local = Address::from_str(&address).unwrap_or(Address::ANY);
    // netsim unless `$SIMBLE_HCI` says otherwise, so every existing
    // invocation is unchanged; `tcp:HOST:PORT` reaches a standalone
    // rootcanal instead, which is how the inquiry path runs with no Android
    // SDK. Bumble's controller is *not* an option here — it has no
    // `HCI_Inquiry` handler at all — so this example is netsim-or-rootcanal.
    let mut transport = match LiveTransport::open_from_env(&name, local) {
        Ok(transport) => transport,
        Err(e) => {
            eprintln!("cannot reach a controller: {e}");
            std::process::exit(1);
        }
    };
    if let Ok(path) = std::env::var("SIMBLE_BTSNOOP")
        && let Ok(file) = std::fs::File::create(&path)
        && transport.set_trace(file)
    {
        println!("btsnoop capture: {path}");
    }
    println!(
        "connected to {} as {name} ({local}); target {target}",
        transport.describe()
    );

    // Class of Device 0x5A020C: phone major class with networking/capturing
    // /object-transfer services — what a peer renders as a phone, and what
    // makes this device recognisably the *initiating* side.
    let mut initiator = Initiator::new(&name, [0x0C, 0x02, 0x5A], target, &expect);
    let channel = HciChannel::new();
    for packet in initiator.host.start_commands() {
        channel.inject_host_packet(packet).expect("queue bring-up");
    }
    // A pure client is neither discoverable nor connectable: it is the one
    // doing the finding. `start_commands` ends by enabling both scans, so
    // this has to come after it.
    for packet in initiator.host.set_scan_enable(scan_enable::NONE) {
        channel
            .inject_host_packet(packet)
            .expect("queue scan enable");
    }
    if let Some(mode) = expect.inquiry_mode {
        println!("asking the controller for inquiry mode {mode:#04x}");
        for packet in initiator.host.set_inquiry_mode(mode) {
            channel
                .inject_host_packet(packet)
                .expect("queue inquiry mode");
        }
    }

    let mut captured = Captured::default();
    let mut failure: Option<String> = None;
    let deadline = Instant::now() + expect.timeout;

    while initiator.phase != Phase::Done && failure.is_none() {
        if let Err(e) = transport.pump(&channel) {
            failure = Some(format!("transport: {e}"));
            break;
        }
        while let Some(packet) = channel.poll_controller_packet() {
            captured.observe(&packet);
            match initiator.host.handle_packet(&packet) {
                Ok(outgoing) => {
                    for out in outgoing {
                        let _ = channel.inject_host_packet(out);
                    }
                }
                Err(e) => eprintln!("host: {e}"),
            }
        }
        initiator.drain_port();
        match initiator.step(&expect.payload) {
            Ok(packets) => {
                for packet in packets {
                    let _ = channel.inject_host_packet(packet);
                }
            }
            Err(e) => failure = Some(e),
        }
        // Profiles speak unprompted too — the SDP query and the RFCOMM
        // initiator's SABM both leave this way, not from the plan above.
        for packet in initiator.host.poll() {
            let _ = channel.inject_host_packet(packet);
        }
        if Instant::now() > deadline && failure.is_none() {
            failure = Some(format!(
                "timed out after {:?} at stage: {}",
                expect.timeout,
                initiator.phase.label()
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    // --- the verdict ------------------------------------------------------
    println!();
    let mut passed = true;
    let found = initiator.found().cloned();
    passed &= check(
        found.is_some(),
        format!("inquiry found the peer at {target}"),
    );
    if let (Some(found), Some(expected)) = (found.as_ref(), expect.class_of_device) {
        passed &= check(
            found.class_of_device == expected,
            format!(
                "the inquiry result carried the peer's Class of Device {} (got {})",
                class_of_device_string(expected),
                class_of_device_string(found.class_of_device)
            ),
        );
    }
    if let Some(expected) = expect.inquiry_event {
        passed &= check(
            captured.inquiry_event == Some(expected),
            format!(
                "the controller reported the inquiry as event {expected:#04x} (got {:?})",
                captured.inquiry_event.map(|c| format!("{c:#04x}"))
            ),
        );
        // Only an Extended Inquiry Result carries the peer's EIR, and the
        // whole point of asking for one is not having to page for a name.
        if expected == 0x2F
            && let Some(expected_name) = expect.name.as_deref()
        {
            passed &= check(
                initiator.name_from_inquiry.as_deref() == Some(expected_name),
                format!(
                    "the Extended Inquiry Result carried {expected_name:?} with no Remote \
                     Name Request (got {:?})",
                    initiator.name_from_inquiry
                ),
            );
        }
    }
    let read_name = found.as_ref().and_then(|d| d.name.clone());
    // Say which question the answer came from. With an Extended Inquiry
    // Result the name is already known and no Remote Name Request is sent,
    // and reporting one that never happened is the sort of claim this whole
    // script exists to stop.
    let how = if initiator.name_from_inquiry.is_some() {
        "the inquiry"
    } else {
        "the Remote Name Request"
    };
    if let Some(expected) = expect.name.as_deref() {
        passed &= check(
            read_name.as_deref() == Some(expected),
            format!("{how} returned the peer's name {expected:?} (got {read_name:?})"),
        );
    } else {
        passed &= check(
            read_name.is_some(),
            format!("{how} produced a name (got {read_name:?})"),
        );
    }
    if expect.pair {
        // The snapshot, not the live link: by now the run has disconnected,
        // and a link's security dies with the link.
        let security = initiator.security_after_pairing.unwrap_or_default();
        passed &= check(
            security.pairing_status == Some(0x00),
            format!(
                "the peer's controller reported Simple Pairing Complete with \
                 success (got {:?})",
                security.pairing_status.map(|s| format!("{s:#04x}"))
            ),
        );
        passed &= check(
            security.authenticated,
            "the link was authenticated (Authentication Complete, status 0)",
        );
        passed &= check(
            security.encrypted,
            "the link was encrypted (Encryption Change, enabled)",
        );
        // The key is the one fact that could only have come from the peer:
        // sixteen octets this process never chose.
        let key = initiator.host.link_key(target);
        passed &= check(
            key.is_some(),
            format!(
                "a Link Key Notification arrived and the bond was stored \
                 (got {:?})",
                key.map(|k| format!("key type {:#04x}", k.key_type))
            ),
        );
        // The peer's own IO capability, read out of its IO Capability
        // Response — a field a simble-only test can never disagree about.
        passed &= check(
            security.peer_io_capability.is_some(),
            format!(
                "the peer's IO Capability Response named its capability (got \
                 {:?})",
                security.peer_io_capability
            ),
        );
        if expect.expect_authenticated_key {
            passed &= check(
                key.is_some_and(|k| k.is_authenticated()),
                format!(
                    "and the association model produced an *authenticated* \
                     key (got key type {:?})",
                    key.map(|k| format!("{:#04x}", k.key_type))
                ),
            );
        }
    }
    passed &= check(
        initiator.rfcomm_channel.is_some(),
        format!(
            "the peer's SDP answer named an RFCOMM channel (got {:?})",
            initiator.rfcomm_channel
        ),
    );
    if let Some(expected) = expect.rfcomm_channel {
        passed &= check(
            initiator.rfcomm_channel == Some(expected),
            format!(
                "and it is channel {expected}, the one the peer's server actually listens on \
                 (got {:?})",
                initiator.rfcomm_channel
            ),
        );
    }
    passed &= check(initiator.sent, "the RFCOMM DLC opened and took the payload");
    passed &= check(
        initiator.received.iter().any(|r| r == &expect.echo),
        format!(
            "the peer echoed {:?} back over the DLC (got {:?})",
            String::from_utf8_lossy(&expect.echo),
            initiator
                .received
                .iter()
                .map(|r| String::from_utf8_lossy(r).to_string())
                .collect::<Vec<_>>()
        ),
    );
    passed &= check(
        initiator.phase == Phase::Done,
        format!(
            "the link was torn down (stage reached: {})",
            initiator.phase.label()
        ),
    );

    captured.report();

    if let Some(reason) = failure {
        println!("\nFAIL — {reason}");
        std::process::exit(1);
    }
    if !passed {
        println!("\nFAIL — see the checks above");
        std::process::exit(1);
    }
    println!("\nPASS — simble's BR/EDR initiator drove a foreign peer end to end");
}
