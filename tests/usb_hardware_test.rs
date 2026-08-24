// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Two USB dongles, two real radios, no simulator anywhere in the path.
//!
//! Every other test in this tree has simble on both ends, or simble against a
//! software controller that simble's authors can read. These have a CSR8510
//! answering a CSR8510 over the air, and they are the only tests that can
//! catch the class of bug real silicon has: commands silently dropped for
//! exceeding a credit budget, event masks refused for setting reserved bits,
//! a scan that hears nothing because the antenna is the thing that is wrong.
//!
//! # Running them
//!
//! ```sh
//! cargo run --example usb_list          # what is plugged in, and its names
//! cargo test --test usb_hardware_test -- --nocapture
//! ```
//!
//! `SIMBLE_USB_A` and `SIMBLE_USB_B` override which dongle plays which role,
//! in any form [`UsbSelector::parse`] takes (`#0`, `0a12:0001`, `02/4`,
//! `02.1`). The default is `#0` and `#1`.
//!
//! # Skipping
//!
//! These **cannot** run in CI: there is no radio there. With fewer than two
//! usable dongles each test prints `SKIP:` and what is missing, then returns
//! — the spirit of `tests/interop/`'s exit 77, which is the house pattern for
//! "cannot honestly run here". It is never a failure, and it is never a
//! silent pass: the skip line names the reason, and `--nocapture` (as above)
//! is what makes cargo show it.
//!
//! Skipping covers *absence*, not malfunction. Two dongles that enumerate but
//! will not open is a skip (the OS Bluetooth stack has claimed one, or usbfs
//! permissions are missing on Linux — neither is a simble bug). Two dongles
//! that open and then misbehave is a failure.

use simble::device::host::{EVENT_MASK_ALL, LE_EVENT_MASK_CORE_4_0};
use simble::device::{CentralEvent, LeCentral};
use simble::transport::usb::{UsbScene, UsbSelector, list_bluetooth_dongles};
use simble::transport::{HciChannel, UsbTransport, h4_type};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// The dongles are a process-wide resource: one radio each, and a test that
/// leaves one advertising poisons the next. Cargo runs the tests in a binary
/// concurrently, so they take turns here.
static RADIO: Mutex<()> = Mutex::new(());

fn radio_lock() -> MutexGuard<'static, ()> {
    RADIO.lock().unwrap_or_else(|e| e.into_inner())
}

// --- opcodes, in the little-endian byte order they go on the wire ------------

const RESET: [u8; 2] = [0x03, 0x0C];
const SET_EVENT_MASK: [u8; 2] = [0x01, 0x0C];
const READ_LOCAL_VERSION: [u8; 2] = [0x01, 0x10];
const READ_BD_ADDR: [u8; 2] = [0x09, 0x10];
const LE_SET_EVENT_MASK: [u8; 2] = [0x01, 0x20];
const LE_SET_ADVERTISING_PARAMETERS: [u8; 2] = [0x06, 0x20];
const LE_SET_ADVERTISING_DATA: [u8; 2] = [0x08, 0x20];
const LE_SET_ADVERTISING_ENABLE: [u8; 2] = [0x0A, 0x20];
const LE_SET_SCAN_PARAMETERS: [u8; 2] = [0x0B, 0x20];
const LE_SET_SCAN_ENABLE: [u8; 2] = [0x0C, 0x20];
const LE_CREATE_CONNECTION: [u8; 2] = [0x0D, 0x20];
const LE_CREATE_CONNECTION_CANCEL: [u8; 2] = [0x0E, 0x20];
const DISCONNECT: [u8; 2] = [0x06, 0x04];

const EVT_DISCONNECTION_COMPLETE: u8 = 0x05;
const EVT_COMMAND_COMPLETE: u8 = 0x0E;
const EVT_COMMAND_STATUS: u8 = 0x0F;
const EVT_LE_META: u8 = 0x3E;
const SUB_LE_CONNECTION_COMPLETE: u8 = 0x01;
const SUB_LE_ADVERTISING_REPORT: u8 = 0x02;

/// The name the advertiser puts in its AD, and the scanner looks for. Long
/// enough that a stray advertiser in the room cannot collide with it.
const ADV_NAME: &str = "SimbleWire";

/// The bring-up every controller gets, in order.
const BRING_UP: [[u8; 2]; 5] = [
    RESET,
    SET_EVENT_MASK,
    LE_SET_EVENT_MASK,
    READ_LOCAL_VERSION,
    READ_BD_ADDR,
];

/// One HCI command packet, no H4 type byte.
fn cmd(opcode: [u8; 2], params: &[u8]) -> Vec<u8> {
    let mut c = vec![opcode[0], opcode[1], params.len() as u8];
    c.extend_from_slice(params);
    c
}

/// Most-significant octet first, the way a BD_ADDR is written; the wire
/// carries it reversed.
fn fmt_addr(le_bytes: &[u8]) -> String {
    le_bytes
        .iter()
        .rev()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

// --- how a wait ended --------------------------------------------------------

/// Why a wait for a radio event stopped.
///
/// `docs/decisions-2026-08-23.md` §2 settles `run_until` on a bare `bool` and
/// records the cost: `false` conflates "the budget ran out" with "the
/// condition was never true". Over a real radio that ambiguity is the whole
/// diagnosis. "No advertising report arrived at all" is a dead antenna, a
/// dongle that never left reset, or an event mask the controller refused;
/// "reports arrived, none of them was ours" is an address or an AD-encoding
/// bug. A `false` that cannot tell those apart sends the reader to the wrong
/// half of the stack, so this carries the counts instead.
#[must_use]
#[derive(Debug)]
enum Waited {
    /// The condition held, this long in.
    Held(Duration),
    /// The budget ran out first, with this much traffic seen meanwhile.
    Expired {
        budget: Duration,
        events_a: usize,
        events_b: usize,
        matching_kind: usize,
    },
}

impl Waited {
    /// Unwraps a hold, or panics with what the radio was doing instead.
    /// `what` names the condition; `kind` names the family of events that
    /// were being counted, so the message can say "20 reports, none of them
    /// ours" rather than just "timed out".
    fn expect(self, what: &str, kind: &str) -> Duration {
        match self {
            Waited::Held(elapsed) => elapsed,
            Waited::Expired {
                budget,
                events_a,
                events_b,
                matching_kind,
            } => panic!(
                "never saw {what} within {budget:?}: {} HCI event(s) arrived \
                 (A {events_a}, B {events_b}), {matching_kind} of them {kind}. {}",
                events_a + events_b,
                if matching_kind == 0 {
                    "None at all of that kind — suspect the radio, the event mask, \
                     or a controller still in reset, not the matching."
                } else {
                    "Some arrived and none matched — suspect the addresses or the \
                     payload, not the radio."
                }
            ),
        }
    }
}

// --- one dongle --------------------------------------------------------------

/// A dongle plus the host-side channel to it, and every event it has ever
/// sent. Hardware tests are short and the traffic is small, so keeping the
/// whole log is cheaper than deciding in advance what to keep — and a failure
/// message that can print the log is worth more than one that cannot.
struct Radio {
    name: &'static str,
    transport: UsbTransport,
    channel: HciChannel,
    /// Every controller→host event, H4 type byte stripped.
    events: Vec<Vec<u8>>,
    /// This controller's own BD_ADDR once `Read_BD_ADDR` has answered.
    bd_addr: Option<[u8; 6]>,
}

impl Radio {
    fn open(name: &'static str, selector: &UsbSelector) -> Result<Self, String> {
        let transport = UsbTransport::open_selected(selector)
            .map_err(|e| format!("dongle {name} ({}): {e}", selector.describe()))?;
        Ok(Self {
            name,
            transport,
            channel: HciChannel::new(),
            events: Vec::new(),
            bd_addr: None,
        })
    }

    /// Queues one command. It leaves under the controller's credit budget, so
    /// "queued" is not "sent" — see `simble::transport::CommandCredits`.
    fn send(&mut self, opcode: [u8; 2], params: &[u8]) {
        self.channel
            .send_command(&cmd(opcode, params))
            .expect("queue command");
    }

    /// One turn of the pump: commands out as credits allow, events in.
    fn pump(&mut self) {
        self.transport
            .pump(&self.channel)
            .unwrap_or_else(|e| panic!("dongle {}: pump failed: {e}", self.name));
        while let Some(packet) = self.channel.poll_controller_packet() {
            if packet.first() == Some(&h4_type::HCI_EVENT) {
                let event = packet[1..].to_vec();
                if let Some(addr) = bd_addr_from_command_complete(&event) {
                    self.bd_addr = Some(addr);
                }
                self.events.push(event);
            }
        }
    }

    /// Events of one code, oldest first.
    fn events_with_code(&self, code: u8) -> impl Iterator<Item = &Vec<u8>> {
        self.events.iter().filter(move |e| e.first() == Some(&code))
    }

    /// LE Meta Events of one subevent code.
    fn le_events(&self, subevent: u8) -> impl Iterator<Item = &Vec<u8>> {
        self.events_with_code(EVT_LE_META)
            .filter(move |e| e.get(2) == Some(&subevent))
    }

    /// Command Complete for `opcode`, if it has arrived: the return
    /// parameters, status byte first.
    fn completion(&self, opcode: [u8; 2]) -> Option<&[u8]> {
        self.events_with_code(EVT_COMMAND_COMPLETE)
            .find(|e| e.get(3..5) == Some(&opcode[..]))
            .map(|e| &e[5..])
    }

    /// The status a command was answered with — from a Command Complete or a
    /// Command Status, whichever the command produces.
    fn status_of(&self, opcode: [u8; 2]) -> Option<u8> {
        if let Some(params) = self.completion(opcode) {
            return params.first().copied();
        }
        self.events_with_code(EVT_COMMAND_STATUS)
            .find(|e| e.get(4..6) == Some(&opcode[..]))
            .and_then(|e| e.get(2).copied())
    }

    /// Brings the controller up: reset, both event masks, and its address.
    /// The masks are the library's, which is the point — `EVENT_MASK_ALL`
    /// stops at bit 61 and `LE_EVENT_MASK_CORE_4_0` at bit 4 precisely
    /// because this silicon refuses anything wider.
    fn bring_up(&mut self) {
        for opcode in BRING_UP {
            match opcode {
                SET_EVENT_MASK => self.send(opcode, &EVENT_MASK_ALL),
                LE_SET_EVENT_MASK => self.send(opcode, &LE_EVENT_MASK_CORE_4_0),
                _ => self.send(opcode, &[]),
            }
        }
    }

    /// Whether bring-up finished — every command answered, not just the last
    /// one. "The address arrived" is not the same thing and was not safe to
    /// assume: see the note on `discard_stale_traffic` in the transport, where
    /// a *previous* session's `Read_BD_ADDR` answer used to arrive first.
    fn is_up(&self) -> bool {
        BRING_UP.iter().all(|op| self.status_of(*op).is_some()) && self.bd_addr.is_some()
    }

    fn addr(&self) -> [u8; 6] {
        self.bd_addr
            .unwrap_or_else(|| panic!("dongle {} never answered Read_BD_ADDR", self.name))
    }
}

/// The address out of a `Read_BD_ADDR` Command Complete, if that is what this
/// event is. Bare event bytes: code, len, credits, opcode lo/hi, status, then
/// six address octets.
fn bd_addr_from_command_complete(event: &[u8]) -> Option<[u8; 6]> {
    if event.first() != Some(&EVT_COMMAND_COMPLETE) || event.get(3..5) != Some(&READ_BD_ADDR[..]) {
        return None;
    }
    if event.get(5) != Some(&0x00) {
        return None;
    }
    event.get(6..12)?.try_into().ok()
}

// --- both dongles ------------------------------------------------------------

/// The pair under test. Both are pumped from one thread: an advertiser that
/// stops being pumped stops advertising, so the scanner's wait has to drive
/// the advertiser too.
struct Pair {
    a: Radio,
    b: Radio,
}

impl Pair {
    fn pump(&mut self) {
        self.a.pump();
        self.b.pump();
    }

    /// Pumps both radios until `done` holds or `budget` runs out. `count`
    /// says how many events of the relevant kind have been seen, so an
    /// expiry can report whether the radio was silent or merely unhelpful.
    fn wait(
        &mut self,
        budget: Duration,
        mut done: impl FnMut(&Pair) -> bool,
        mut count: impl FnMut(&Pair) -> usize,
    ) -> Waited {
        let start = Instant::now();
        loop {
            self.pump();
            if done(self) {
                return Waited::Held(start.elapsed());
            }
            if start.elapsed() >= budget {
                return Waited::Expired {
                    budget,
                    events_a: self.a.events.len(),
                    events_b: self.b.events.len(),
                    matching_kind: count(self),
                };
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    /// Leaves both controllers idle: scanning off, advertising off, any link
    /// dropped. A dongle left advertising is still advertising when the next
    /// test opens it, and a link left up makes the next `LE_Create_Connection`
    /// fail with 0x0C, Command Disallowed.
    fn quiesce(&mut self) {
        for handle in connection_handles(&self.b) {
            self.b.send(DISCONNECT, &[handle[0], handle[1], 0x13]);
        }
        self.b.send(LE_SET_SCAN_ENABLE, &[0x00, 0x00]);
        self.a.send(LE_SET_ADVERTISING_ENABLE, &[0x00]);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            self.pump();
            if self.a.transport.command_backlog().1 == 0
                && self.b.transport.command_backlog().1 == 0
            {
                break;
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }
}

/// Connection handles this radio has been told about and not seen closed.
fn connection_handles(radio: &Radio) -> Vec<[u8; 2]> {
    let mut open: Vec<[u8; 2]> = radio
        .le_events(SUB_LE_CONNECTION_COMPLETE)
        .filter(|e| e.get(3) == Some(&0x00))
        .filter_map(|e| e.get(4..6).and_then(|h| h.try_into().ok()))
        .collect();
    // Disconnection Complete (7.7.5): Status, Connection_Handle, Reason —
    // so the handle is at 3..5. It is 4..6 in LE Connection Complete, whose
    // subevent code shifts everything along by one.
    for closed in radio.events_with_code(EVT_DISCONNECTION_COMPLETE) {
        if let Some(handle) = closed.get(3..5).and_then(|h| <[u8; 2]>::try_from(h).ok()) {
            open.retain(|h| *h != handle);
        }
    }
    open
}

/// How often the pump runs while waiting. Long enough not to spin a core,
/// short enough that a 100 ms advertising interval is never missed.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

// --- acquiring the hardware, or saying why not -------------------------------

/// The two dongles to use, or the reason there are none — which is a skip,
/// not a failure. Absence and inaccessibility only: a dongle that opens and
/// then misbehaves is a bug, and fails.
///
/// Both are opened and released here as a probe, so "the OS Bluetooth stack
/// has this one" is reported once, as a skip, rather than as a failure in
/// whichever test happened to run first.
fn usable_selectors() -> Result<(UsbSelector, UsbSelector), String> {
    let dongles = list_bluetooth_dongles().map_err(|e| format!("USB enumeration failed: {e}"))?;
    if dongles.len() < 2 {
        return Err(format!(
            "needs 2 Bluetooth-class USB dongles, found {}{}. \
             `cargo run --example usb_list` shows what is plugged in.",
            dongles.len(),
            if dongles.is_empty() {
                " (a Mac's built-in controller is PCIe-attached and never appears)"
            } else {
                ""
            }
        ));
    }
    let a = selector_from_env("SIMBLE_USB_A", UsbSelector::Index(0))?;
    let b = selector_from_env("SIMBLE_USB_B", UsbSelector::Index(1))?;
    // Hold A open while opening B, which is the state every test below runs
    // in. Probing them one at a time would pass for two selectors naming the
    // *same* dongle — `#0` and `02/4` are different strings and one device —
    // and the tests would then fail on "could not open interface for
    // exclusive access" rather than skipping. Selector equality cannot catch
    // that: the four forms are four names for the same hardware.
    let opened_a = UsbTransport::open_selected(&a).map_err(|e| open_failure("A", &a, e))?;
    let _opened_b = UsbTransport::open_selected(&b).map_err(|e| open_failure("B", &b, e))?;
    drop(opened_a);
    Ok((a, b))
}

/// Why a dongle could not be opened, in the terms that fix it.
fn open_failure(role: &str, selector: &UsbSelector, e: simble::types::SimbleError) -> String {
    format!(
        "dongle {role} ({}) will not open: {e} — it may be the *same* dongle as the \
         other role (the four selector forms are four names for one device; \
         `cargo run --example usb_list` shows which is which), or held by the OS \
         Bluetooth stack on macOS, or short of usbfs permissions on Linux (udev \
         rule, or run as root)",
        selector.describe()
    )
}

/// Two open raw-HCI dongles.
fn open_pair() -> Result<Pair, String> {
    let (a, b) = usable_selectors()?;
    Ok(Pair {
        a: Radio::open("A", &a)?,
        b: Radio::open("B", &b)?,
    })
}

fn selector_from_env(var: &str, default: UsbSelector) -> Result<UsbSelector, String> {
    match std::env::var(var) {
        Err(_) => Ok(default),
        Ok(spec) => UsbSelector::parse(&spec).map_err(|e| format!("{var}={spec:?}: {e}")),
    }
}

/// Runs `body` with the hardware, or prints why it was skipped and returns.
///
/// The `SKIP:` line is the house "cannot honestly run here" signal (see the
/// module docs); cargo captures it unless the run passes `--nocapture`.
fn with_hardware(test: &str, body: impl FnOnce(Pair)) {
    let _guard = radio_lock();
    match open_pair() {
        Err(why) => eprintln!("SKIP: {test} — {why}"),
        Ok(pair) => body(pair),
    }
}

/// As [`with_hardware`], but hands over the *selectors* rather than open
/// transports, for a test that needs something other than a raw `Radio` on
/// one of them — a `UsbScene`, which opens the dongle itself.
fn with_selectors(test: &str, body: impl FnOnce(UsbSelector, UsbSelector)) {
    let _guard = radio_lock();
    match usable_selectors() {
        Err(why) => eprintln!("SKIP: {test} — {why}"),
        Ok((a, b)) => body(a, b),
    }
}

/// Brings both controllers up and asserts they answered, since every test
/// below starts here.
fn bring_up_both(pair: &mut Pair) {
    pair.a.bring_up();
    pair.b.bring_up();
    pair.wait(
        Duration::from_secs(5),
        |p| p.a.is_up() && p.b.is_up(),
        |p| p.a.events.len() + p.b.events.len(),
    )
    .expect("both controllers to finish bring-up", "HCI events");
    for radio in [&pair.a, &pair.b] {
        assert_eq!(
            radio.status_of(SET_EVENT_MASK),
            Some(0x00),
            "dongle {} rejected Set_Event_Mask — the mask has a reserved bit set",
            radio.name
        );
        assert_eq!(
            radio.status_of(LE_SET_EVENT_MASK),
            Some(0x00),
            "dongle {} rejected LE_Set_Event_Mask — the mask asks for subevents \
             this controller does not define",
            radio.name
        );
    }
}

// --- the tests ---------------------------------------------------------------

/// The credit budget, shown rather than asserted from a comment.
///
/// The same seven-command burst goes to the same dongle twice: once with
/// flow control off, which is what every simble caller did before
/// `CommandCredits` existed, and once with it on. Only the second run is
/// asserted — a controller generous enough to answer all seven unthrottled
/// would be a nice surprise, not a test failure — but the printed counts are
/// the evidence, and on a CSR8510 they are 1 and 7.
#[test]
fn hardware_command_credits_stop_the_controller_dropping_commands() {
    with_hardware(
        "hardware_command_credits_stop_the_controller_dropping_commands",
        |mut pair| {
            let burst = [
                (RESET, vec![]),
                (SET_EVENT_MASK, EVENT_MASK_ALL.to_vec()),
                (LE_SET_EVENT_MASK, LE_EVENT_MASK_CORE_4_0.to_vec()),
                (READ_LOCAL_VERSION, vec![]),
                (READ_BD_ADDR, vec![]),
                (LE_SET_SCAN_ENABLE, vec![0x00, 0x00]),
                (LE_SET_ADVERTISING_ENABLE, vec![0x00]),
            ];

            // Run one: no flow control. Every command is written the instant
            // it is queued, and the controller keeps whatever it has room for.
            pair.a.transport.set_command_flow_control(false);
            for (opcode, params) in &burst {
                pair.a.send(*opcode, params);
            }
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                pair.a.pump();
                std::thread::sleep(POLL_INTERVAL);
            }
            let unthrottled: Vec<[u8; 2]> = answered_opcodes(&pair.a, &burst);

            // Run two: same burst, same dongle, credits respected.
            pair.a.events.clear();
            pair.a.bd_addr = None;
            pair.a.transport.set_command_flow_control(true);
            for (opcode, params) in &burst {
                pair.a.send(*opcode, params);
            }
            let waited = pair.wait(
                Duration::from_secs(5),
                |p| answered_opcodes(&p.a, &burst).len() == burst.len(),
                |p| answered_opcodes(&p.a, &burst).len(),
            );
            let throttled: Vec<[u8; 2]> = answered_opcodes(&pair.a, &burst);

            eprintln!(
                "credits: {}/{} commands answered with flow control OFF, {}/{} with it ON",
                unthrottled.len(),
                burst.len(),
                throttled.len(),
                burst.len()
            );
            if unthrottled.len() < burst.len() {
                let dropped: Vec<String> = burst
                    .iter()
                    .map(|(op, _)| *op)
                    .filter(|op| !unthrottled.contains(op))
                    .map(|op| format!("{:#06x}", u16::from_le_bytes(op)))
                    .collect();
                eprintln!(
                    "  without credits the controller silently discarded: {} \
                     — no error, no event, nothing to log",
                    dropped.join(", ")
                );
            }

            waited.expect(
                "every command in the burst answered with flow control on",
                "answers to burst commands",
            );
            assert_eq!(
                throttled.len(),
                burst.len(),
                "with credits respected every command must be answered; \
                 {throttled:02X?} of {burst:?} were"
            );
            pair.quiesce();
        },
    );
}

/// Which of `burst`'s opcodes the controller has answered.
fn answered_opcodes(radio: &Radio, burst: &[([u8; 2], Vec<u8>)]) -> Vec<[u8; 2]> {
    burst
        .iter()
        .map(|(opcode, _)| *opcode)
        .filter(|opcode| radio.status_of(*opcode).is_some())
        .collect()
}

/// A advertises, B scans, and the assertion is on what **B's** controller
/// reports — a different piece of silicon than the one that built the packet,
/// which is the only way an over-the-air encoding bug can surface.
///
/// The advertiser's address comes from its own `Read_BD_ADDR`, never a
/// constant: these dongles are interchangeable and a hardcoded address turns
/// "the radio worked" into "the address matched".
#[test]
fn hardware_one_dongle_hears_the_other_advertise() {
    with_hardware(
        "hardware_one_dongle_hears_the_other_advertise",
        |mut pair| {
            bring_up_both(&mut pair);
            let advertiser = pair.a.addr();
            eprintln!(
                "A {} advertising as {ADV_NAME:?}, B {} scanning",
                fmt_addr(&advertiser),
                fmt_addr(&pair.b.addr())
            );

            start_advertising(&mut pair.a);
            start_scanning(&mut pair.b);

            let waited = pair.wait(
                Duration::from_secs(15),
                |p| matching_reports(&p.b, &advertiser) > 0,
                |p| p.b.le_events(SUB_LE_ADVERTISING_REPORT).count(),
            );
            let elapsed = waited.expect(
                &format!("an advertising report from {}", fmt_addr(&advertiser)),
                "LE Advertising Reports",
            );
            let total = pair.b.le_events(SUB_LE_ADVERTISING_REPORT).count();
            eprintln!(
                "B heard A after {elapsed:?} ({} report(s) total, {} from A)",
                total,
                matching_reports(&pair.b, &advertiser)
            );

            // The report is only evidence if it carries both the address and the
            // payload: an address alone could be any advertiser at that address,
            // and a name alone could be anyone's.
            let report = pair
                .b
                .le_events(SUB_LE_ADVERTISING_REPORT)
                .find(|e| report_address(e) == Some(advertiser))
                .expect("a report from A");
            let data = report_data(report).expect("advertising data in the report");
            assert!(
                contains_complete_local_name(data, ADV_NAME),
                "B's report of A carried no Complete Local Name {ADV_NAME:?}: {data:02X?}"
            );

            pair.quiesce();
        },
    );
}

/// B connects to A, and **both** controllers must say so. One end reporting a
/// connection is one end's opinion; a link exists when the peripheral has
/// also been told, with the initiator's address, in the opposite role.
#[test]
fn hardware_the_two_dongles_form_a_real_le_link() {
    with_hardware(
        "hardware_the_two_dongles_form_a_real_le_link",
        |mut pair| {
            bring_up_both(&mut pair);
            let advertiser = pair.a.addr();
            let initiator = pair.b.addr();

            start_advertising(&mut pair.a);
            start_scanning(&mut pair.b);
            pair.wait(
                Duration::from_secs(15),
                |p| matching_reports(&p.b, &advertiser) > 0,
                |p| p.b.le_events(SUB_LE_ADVERTISING_REPORT).count(),
            )
            .expect(
                &format!("an advertising report from {}", fmt_addr(&advertiser)),
                "LE Advertising Reports",
            );

            // Scanning and initiating at once is more than a 4.0 controller
            // promises, so stop scanning first.
            pair.b.send(LE_SET_SCAN_ENABLE, &[0x00, 0x00]);
            pair.b
                .send(LE_CREATE_CONNECTION, &create_connection_params(&advertiser));

            let waited = pair.wait(
                Duration::from_secs(20),
                |p| {
                    connection_to(&p.b, &advertiser).is_some()
                        && connection_to(&p.a, &initiator).is_some()
                },
                |p| {
                    p.a.le_events(SUB_LE_CONNECTION_COMPLETE).count()
                        + p.b.le_events(SUB_LE_CONNECTION_COMPLETE).count()
                },
            );
            if matches!(waited, Waited::Expired { .. }) {
                // Leave the initiator in a sane state before the panic, or the
                // dongle is still trying to connect when the next test opens it.
                pair.b.send(LE_CREATE_CONNECTION_CANCEL, &[]);
                for _ in 0..100 {
                    pair.pump();
                    std::thread::sleep(POLL_INTERVAL);
                }
            }
            let elapsed = waited.expect(
                "LE Connection Complete at both ends",
                "LE Connection Completes",
            );

            let central = connection_to(&pair.b, &advertiser).expect("B's connection to A");
            let peripheral = connection_to(&pair.a, &initiator).expect("A's connection to B");
            eprintln!(
                "linked in {elapsed:?}: B handle {:#06x} as central, A handle {:#06x} as peripheral",
                central.handle, peripheral.handle
            );
            assert_eq!(central.role, 0x00, "B initiated, so B must be the central");
            assert_eq!(
                peripheral.role, 0x01,
                "A advertised, so A must be the peripheral"
            );

            pair.quiesce();
            // The link must also come down, or the next run starts with the
            // dongles already busy and every LE_Create_Connection is refused.
            pair.wait(
                Duration::from_secs(5),
                |p| connection_handles(&p.b).is_empty(),
                |p| p.b.events_with_code(EVT_DISCONNECTION_COMPLETE).count(),
            )
            .expect("the link to come down again", "Disconnection Completes");
        },
    );
}

// --- the HCI each role sends -------------------------------------------------

fn start_advertising(radio: &mut Radio) {
    // 100 ms both bounds, ADV_IND, public own address, all three channels,
    // no filtering.
    radio.send(
        LE_SET_ADVERTISING_PARAMETERS,
        &[
            0xA0, 0x00, 0xA0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x07,
            0x00,
        ],
    );
    let mut ad = vec![0x02, 0x01, 0x06]; // Flags: LE General Discoverable, no BR/EDR
    ad.push(1 + ADV_NAME.len() as u8);
    ad.push(0x09); // Complete Local Name
    ad.extend_from_slice(ADV_NAME.as_bytes());
    let mut params = vec![ad.len() as u8];
    params.extend_from_slice(&ad);
    params.resize(32, 0x00); // the parameter is fixed-length, always 31 + 1
    radio.send(LE_SET_ADVERTISING_DATA, &params);
    radio.send(LE_SET_ADVERTISING_ENABLE, &[0x01]);
}

fn start_scanning(radio: &mut Radio) {
    // Active scanning (0x01) so scan responses are solicited too; 10 ms
    // interval and window, public own address, accept everything.
    radio.send(
        LE_SET_SCAN_PARAMETERS,
        &[0x01, 0x10, 0x00, 0x10, 0x00, 0x00, 0x00],
    );
    radio.send(LE_SET_SCAN_ENABLE, &[0x01, 0x00]); // enable, duplicates on
}

/// `LE_Create_Connection` at `peer`: 60 ms scan interval / 30 ms window, no
/// filter list, public addresses both ends, a 30-50 ms connection interval,
/// no latency, and a 2.56 s supervision timeout — comfortably more than the
/// `(1 + latency) * interval * 2` the spec requires.
fn create_connection_params(peer: &[u8; 6]) -> Vec<u8> {
    let mut params = vec![0x60, 0x00, 0x30, 0x00, 0x00, 0x00];
    params.extend_from_slice(peer);
    params.extend_from_slice(&[
        0x00, // own address type: public
        0x18, 0x00, // min interval 30 ms
        0x28, 0x00, // max interval 50 ms
        0x00, 0x00, // max latency
        0x00, 0x01, // supervision timeout 2.56 s
        0x00, 0x00, 0x00, 0x00, // CE length min/max
    ]);
    params
}

// --- reading what came back --------------------------------------------------

/// One end's view of a link.
struct Link {
    handle: u16,
    /// 0x00 central, 0x01 peripheral.
    role: u8,
}

/// This radio's successful connection to `peer`, if it has one.
/// `LE_Connection_Complete` params: subevent, status, handle, role, peer
/// address type, peer address.
fn connection_to(radio: &Radio, peer: &[u8; 6]) -> Option<Link> {
    radio
        .le_events(SUB_LE_CONNECTION_COMPLETE)
        .find(|e| e.get(3) == Some(&0x00) && e.get(8..14) == Some(&peer[..]))
        .map(|e| Link {
            handle: u16::from_le_bytes([e[4], e[5]]),
            role: e[6],
        })
}

/// The advertiser's address out of an `LE_Advertising_Report`, for the
/// single-report case these dongles emit. Params: subevent, num reports,
/// event type, address type, 6-byte address, data length, data, RSSI.
fn report_address(event: &[u8]) -> Option<[u8; 6]> {
    if event.get(3) != Some(&0x01) {
        return None; // batched reports are not what a CSR8510 sends
    }
    event.get(6..12)?.try_into().ok()
}

/// The AD payload out of a single-report `LE_Advertising_Report`.
fn report_data(event: &[u8]) -> Option<&[u8]> {
    if event.get(3) != Some(&0x01) {
        return None;
    }
    let len = *event.get(12)? as usize;
    event.get(13..13 + len)
}

fn matching_reports(radio: &Radio, peer: &[u8; 6]) -> usize {
    radio
        .le_events(SUB_LE_ADVERTISING_REPORT)
        .filter(|e| report_address(e) == Some(*peer))
        .count()
}

/// Whether the AD carries `name` as a Complete Local Name (type 0x09). AD is
/// length-prefixed structures: one length byte covering the type byte and the
/// value, then the next.
fn contains_complete_local_name(ad: &[u8], name: &str) -> bool {
    let mut rest = ad;
    while let Some((&len, tail)) = rest.split_first() {
        let len = len as usize;
        if len == 0 || tail.len() < len {
            return false;
        }
        let (structure, next) = tail.split_at(len);
        if structure[0] == 0x09 && &structure[1..] == name.as_bytes() {
            return true;
        }
        rest = next;
    }
    false
}

// --- one layer up: GATT over the air -----------------------------------------

/// The peripheral dongle A hosts: the catalog's Heart Rate device, unchanged.
const PERIPHERAL_SCRIPT: &str = r#"
let server = android::BluetoothGattServer("SimbleWire");
let hrs = android::BluetoothGattService(uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY);
let hr = android::BluetoothGattCharacteristic(uuid::HEART_RATE_MEASUREMENT,
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
hr.set_value([0x00, 72]);
hr.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
hrs.add_characteristic(hr);
server.add_service(hrs);
"#;

/// The public BD_ADDR burned into the dongle `selector` names, read from the
/// controller and then released so the scene can claim it.
///
/// **A scripted address cannot reach the air here.** `UsbScene` stamps the
/// address the caller passes onto the peripheral's identity, but the
/// advertising parameters say `own_address_type = public`, and a controller's
/// public address is in its ROM — so what the peer actually sees is the
/// dongle's, and a central told to look for the scripted one waits forever.
/// (A real stack that wants to choose its address uses
/// `LE_Set_Random_Address` with `own_address_type = random`; simble's host
/// does not offer that yet.) So the test asks the hardware what it will
/// advertise as, rather than telling it.
fn public_address(selector: &UsbSelector) -> simble::types::Address {
    let mut radio = Radio::open("A", selector).expect("dongle A");
    radio.send(READ_BD_ADDR, &[]);
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        radio.pump();
        if let Some(wire) = radio.bd_addr {
            // HCI carries a BD_ADDR least-significant octet first; `Address`
            // is built most-significant first.
            let mut be = wire;
            be.reverse();
            return simble::types::Address::from_be_bytes(be);
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    panic!("dongle {} never answered Read_BD_ADDR", selector.describe());
}

/// `LeCentral` driven over a dongle: the host state machine on one side, real
/// HCI on the other.
struct CentralRadio {
    transport: UsbTransport,
    channel: HciChannel,
    central: LeCentral,
}

impl CentralRadio {
    fn open(selector: &UsbSelector) -> Result<Self, String> {
        let mut central = LeCentral::new();
        // The same narrowing `UsbScene` applies to its peripheral, for the
        // same reason: a 4.0 dongle refuses the default mask outright, and a
        // central that never receives LE Advertising Report never gets as far
        // as connecting.
        central.set_le_event_mask(LE_EVENT_MASK_CORE_4_0);
        Ok(Self {
            transport: UsbTransport::open_selected(selector)
                .map_err(|e| format!("central dongle ({}): {e}", selector.describe()))?,
            channel: HciChannel::new(),
            central,
        })
    }

    /// Hands H4-framed packets from the host state machine to the controller.
    fn queue(&self, packets: Vec<Vec<u8>>) {
        for packet in packets {
            self.channel
                .inject_host_packet(packet)
                .expect("queue host packet");
        }
    }

    fn pump(&mut self) {
        self.transport
            .pump(&self.channel)
            .expect("central: USB pump");
        while let Some(packet) = self.channel.poll_controller_packet() {
            let replies = self.central.on_packet(&packet);
            self.queue(replies);
        }
        let queued = self.central.pump();
        self.queue(queued);
    }
}

/// The whole stack over real RF: a scripted `VirtualDevice` peripheral on
/// dongle A, `LeCentral`'s GATT client on dongle B, and service discovery
/// between them with nothing simulated anywhere.
///
/// The raw-HCI tests above prove the radios reach each other. This proves the
/// layers on top of them do: advertising built by simble's host code, heard by
/// simble's central code, an ATT MTU exchange and a full discovery over a link
/// that exists only because two antennas found each other.
#[test]
fn hardware_gatt_discovery_over_two_real_radios() {
    with_selectors(
        "hardware_gatt_discovery_over_two_real_radios",
        |sel_a, sel_b| {
            // Read A's address before the scene takes the dongle, and use it
            // for both ends: it is what A will advertise as whatever the
            // scene is told, so making the scripted identity agree with it
            // keeps the peripheral honest about who it is.
            let address = public_address(&sel_a);
            eprintln!("peripheral on A is {address}, central on B connecting to it");
            let mut peripheral = UsbScene::new(sel_a);
            peripheral
                .add_peripheral(address, PERIPHERAL_SCRIPT)
                .expect("scripted peripheral on dongle A");
            let mut central = CentralRadio::open(&sel_b).expect("central on dongle B");

            // Let the peripheral finish bring-up and start advertising before
            // the central starts looking, so the first scan window can hear it.
            for _ in 0..40 {
                peripheral.pump();
                std::thread::sleep(POLL_INTERVAL);
            }
            let bring_up = central.central.connect(address);
            central.queue(bring_up);

            let start = Instant::now();
            let budget = Duration::from_secs(30);
            let mut discovered = None;
            while start.elapsed() < budget {
                peripheral.pump();
                central.pump();
                for event in central.central.take_events() {
                    if let CentralEvent::ServicesDiscovered { services } = event {
                        discovered = Some(services);
                    }
                }
                if discovered.is_some() {
                    break;
                }
                std::thread::sleep(POLL_INTERVAL);
            }

            // The same distinction the raw tests make: "never connected" and
            // "connected but discovery stalled" send a reader to different
            // halves of the stack, so the message says which.
            let services = discovered.unwrap_or_else(|| {
                panic!(
                    "no GATT discovery within {budget:?}. The central got as far as \
                     {} (MTU {}, handle {:#06x}). {}",
                    central.central.phase().label(),
                    central.central.mtu(),
                    central.central.connection_handle(),
                    if central.central.connection_handle() == 0 {
                        "No link — the radios never met, or the peripheral never \
                         advertised; check the raw-HCI tests first."
                    } else {
                        "The link is up and discovery stalled — an ATT problem, \
                         not a radio one."
                    }
                )
            });
            eprintln!(
                "GATT over the air in {:?}: {services} service(s), MTU {}",
                start.elapsed(),
                central.central.mtu()
            );

            assert!(
                central.central.is_ready(),
                "discovery finished but the central is in {}",
                central.central.phase().label()
            );
            let heart_rate = simble::types::Uuid::from_u16(0x180D);
            let measurement = simble::types::Uuid::from_u16(0x2A37);
            assert!(
                central
                    .central
                    .services()
                    .iter()
                    .any(|s| s.uuid == heart_rate),
                "the Heart Rate service the script declares was not discovered \
                 over the air; found {:?}",
                central
                    .central
                    .services()
                    .iter()
                    .map(|s| s.uuid)
                    .collect::<Vec<_>>()
            );
            assert!(
                central.central.value_handle(measurement).is_some(),
                "the Heart Rate Measurement characteristic has no value handle"
            );

            let teardown = central.central.disconnect();
            central.queue(teardown);
            for _ in 0..100 {
                peripheral.pump();
                central.pump();
                std::thread::sleep(POLL_INTERVAL);
            }
        },
    );
}
