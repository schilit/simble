// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! A bulk-transfer benchmark: a central pushes 256 KB to a peripheral over
//! GATT write-without-response, and both ends are timed.
//!
//! # Why the phases matter more than the megabits
//!
//! A speed-test gauge answers "how wide is the pipe". For 256 KB over BLE
//! that is the smaller half of the question: at a few hundred kB/s the
//! transfer itself is a second or two, while *getting to the point of
//! transferring* — hearing an advertisement, opening the connection,
//! agreeing an MTU, walking the peer's attribute table — costs anywhere
//! between a few milliseconds on a simulated link and a couple of seconds on
//! real radio with a slow advertising interval. So this reports four
//! segments, in order, and the total is the sum of them:
//!
//! | segment | starts | ends |
//! |---|---|---|
//! | discover | [`BulkCentral::start`] | the peer's advertisement is heard |
//! | connect | LE Create Connection | LE Connection Complete |
//! | negotiate | ATT MTU exchange | the control point is subscribed |
//! | transfer | the first data chunk | **the last byte arrives** |
//!
//! # Both ends are measured, and that is not optional
//!
//! Write-without-response is unacknowledged. A central can hand 256 KB to
//! its controller far faster than the link drains it, so "the central
//! finished writing" happens well before "the peripheral received the last
//! byte" — and a client-only stopwatch is therefore wrong in the flattering
//! direction. It would also be blind to loss, which is the single most
//! interesting thing a bulk transfer can reveal.
//!
//! The split follows what each end can actually observe. The central owns
//! the setup segments, because the peripheral does not know discovery
//! happened. The peripheral owns arrival: it counts the bytes and stamps
//! when the last one landed. [`BulkCentral::report`] says which of the three
//! it got, in [`BulkReport::confirmation`]:
//!
//! - `server-stamped` — a [`BulkSink`] on the caller's own clock reported
//!   its arrival time through [`BulkCentral::note_server`]. The honest
//!   number.
//! - `peer-reported` — the sink is ours but not on our clock, so its byte
//!   count is real and the end time is when its report notification arrived.
//! - `unconfirmed` — the peer is somebody else's and has no control point.
//!   The transfer figure is **bytes sent, not confirmed delivered**, and a
//!   reader must be told so.
//!
//! # The wire protocol
//!
//! One 128-bit service with two characteristics ([`bulk_uuid`]):
//!
//! - **Data** (write, write-without-response) — the bytes.
//! - **Control** (write, notify) — `[BEGIN, total_u32]` resets the sink's
//!   counters; `[FINISH]` makes it notify `[REPORT, bytes_u32, chunks_u32]`
//!   whatever it has, so a short count is visible rather than a hang.
//!
//! # No clock of its own
//!
//! `std::time::Instant` panics on `wasm32-unknown-unknown`, so every method
//! that needs the time takes `now_ms` from the caller — the same shape
//! [`A2dpSourceRunner`](crate::device::a2dp_source_runner::A2dpSourceRunner)
//! uses, and the reason this can be driven identically from a browser frame
//! callback and from a native test with a fake clock.

use std::sync::{Arc, Mutex};

use crate::controller::sim::Link;
use crate::device::central::{CentralEvent, CentralPhase, LeCentral};
use crate::device::host::{LeHost, acl_packets, command};
use crate::device::virtual_device::VirtualDevice;
use crate::gatt::database::AttributeHandler;
use crate::gatt::{AttributePermissions, CharacteristicProperties, GattDatabase};
use crate::transport::HciChannel;
use crate::types::{Address, AddressType};

/// The benchmark service and its two characteristics.
///
/// Custom 128-bit UUIDs, in the little-endian order [`Uuid::Uuid128`](crate::types::Uuid)
/// stores: `f0bb0001-1234-5678-90ab-cdef01234567` and its two siblings.
/// They share the `f0bb`-prefixed space the other simble demo devices use,
/// so a scanner that meets one recognises the family.
pub mod bulk_uuid {
    use crate::types::Uuid;

    /// Bulk Transfer Service.
    pub const SERVICE: Uuid = Uuid::Uuid128([
        0x67, 0x45, 0x23, 0x01, 0xEF, 0xCD, 0xAB, 0x90, 0x78, 0x56, 0x34, 0x12, 0x01, 0x00, 0xBB,
        0xF0,
    ]);
    /// Where the payload is written. Write and Write Without Response.
    pub const DATA: Uuid = Uuid::Uuid128([
        0x67, 0x45, 0x23, 0x01, 0xEF, 0xCD, 0xAB, 0x90, 0x78, 0x56, 0x34, 0x12, 0x02, 0x00, 0xBB,
        0xF0,
    ]);
    /// Begin/finish in, the sink's own count out. Write and Notify.
    pub const CONTROL: Uuid = Uuid::Uuid128([
        0x67, 0x45, 0x23, 0x01, 0xEF, 0xCD, 0xAB, 0x90, 0x78, 0x56, 0x34, 0x12, 0x03, 0x00, 0xBB,
        0xF0,
    ]);
}

/// Control-point opcodes.
pub mod control_op {
    /// `[BEGIN, total_u32_le]` — reset the counters, expect this many bytes.
    pub const BEGIN: u8 = 0x01;
    /// `[FINISH]` — notify the count now, whatever it is.
    pub const FINISH: u8 = 0x02;
    /// `[REPORT, bytes_u32_le, chunks_u32_le]` — the sink's answer.
    pub const REPORT: u8 = 0x03;
}

/// 256 KB. Large enough that the transfer segment is not swamped by setup,
/// small enough to finish inside one page visit on a slow link.
pub const DEFAULT_TRANSFER_BYTES: usize = 262_144;

/// How many MTU-sized chunks may be handed to the controller before waiting
/// for it to drain them, on a controller that does not say how many ACL
/// buffers it has.
///
/// This host does not implement Number Of Completed Packets flow control in
/// its ATT queue, so on a controller that *does* report its buffers
/// [`BulkCentral`] tracks credits properly (see
/// [`BulkReport::acl_credits`]) and this fallback is not used. Simble's own
/// in-process controller answers LE Read Buffer Size as an unknown command,
/// which is where the fallback earns its keep — nothing there can overrun.
pub const DEFAULT_WINDOW_CHUNKS: usize = 64;

/// How long a segment may make no progress before the run is called failed.
pub const DEFAULT_TIMEOUT_MS: f64 = 15_000.0;

/// The knobs one run is given.
///
/// A struct rather than a widening constructor because the interesting
/// settings are not reachable yet — PHY preference, connection interval,
/// advertising interval, an MTU target, Data Length Extension — and each of
/// them will want to arrive without churning every caller. Deserialised from
/// the JSON a page sends, with every field optional.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct BulkOptions {
    /// How many bytes to write.
    pub total_bytes: usize,
    /// Write Request (acknowledged, one outstanding at a time) instead of
    /// Write Command. The comparison is the point: only the unacknowledged
    /// form can fill the link, and only the acknowledged form cannot lose a
    /// byte silently.
    pub with_response: bool,
    /// Chunks handed over per turn on a controller that reports no ACL
    /// buffers. See [`DEFAULT_WINDOW_CHUNKS`].
    pub window_chunks: usize,
    /// How long a segment may stall before the run is called failed.
    pub timeout_ms: f64,
}

impl Default for BulkOptions {
    fn default() -> Self {
        Self {
            total_bytes: DEFAULT_TRANSFER_BYTES,
            with_response: false,
            window_chunks: DEFAULT_WINDOW_CHUNKS,
            timeout_ms: DEFAULT_TIMEOUT_MS,
        }
    }
}

impl BulkOptions {
    /// Options from the JSON a page sends. Unparseable JSON is the defaults
    /// rather than an error: a benchmark that refuses to run because a
    /// setting was misspelled helps nobody.
    pub fn from_json(json: &str) -> Self {
        serde_json::from_str(json).unwrap_or_default()
    }

    /// The settings normalised into ranges the run can honour.
    fn sane(mut self) -> Self {
        self.total_bytes = self.total_bytes.clamp(1, 64 * 1024 * 1024);
        self.window_chunks = self.window_chunks.max(1);
        self.timeout_ms = self.timeout_ms.max(100.0);
        self
    }
}

/// LE Read Buffer Size (Vol 4, Part E, Section 7.8.2).
const LE_READ_BUFFER_SIZE: [u8; 2] = [0x02, 0x20];
/// LE Set PHY (Vol 4, Part E, Section 7.8.49).
const LE_SET_PHY: [u8; 2] = [0x32, 0x20];
/// LE PHY Update Complete subevent (Vol 4, Part E, Section 7.7.65.12).
const LE_PHY_UPDATE_COMPLETE: u8 = 0x0C;
/// Number Of Completed Packets (Vol 4, Part E, Section 7.7.19).
const NUMBER_OF_COMPLETED_PACKETS: u8 = 0x13;
/// The LE ACL payload one H4 ACL packet carries, matching
/// `crate::device::host`'s fragmenting.
const LE_ACL_DATA_LEN: usize = 27;
/// ATT opcode + attribute handle, in front of every written value.
const ATT_WRITE_OVERHEAD: usize = 3;
/// The L2CAP basic header in front of every ATT PDU.
const L2CAP_HEADER: usize = 4;

/// A PHY identifier as a report would name it.
fn phy_label(value: u8) -> Option<&'static str> {
    match value {
        0x01 => Some("LE 1M"),
        0x02 => Some("LE 2M"),
        0x03 => Some("LE Coded"),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// The peripheral half
// ---------------------------------------------------------------------------

/// What the sink has seen. Shared with the two attribute handlers, which run
/// inside [`GattDatabase::write`] and so cannot reach the sink itself.
#[derive(Debug, Default)]
struct SinkShared {
    bytes: u64,
    chunks: u32,
    expected: u64,
    /// Set by a `FINISH` write, cleared once the report has been notified.
    report_due: bool,
}

/// The counting half of the data characteristic.
///
/// It deliberately does **not** store the value: 256 KB of writes would
/// otherwise reallocate a half-kilobyte attribute five hundred times, and
/// the benchmark would be measuring `Vec::extend`.
#[derive(Debug)]
struct DataHandler(Arc<Mutex<SinkShared>>);

impl AttributeHandler for DataHandler {
    fn on_write(&mut self, _db: &mut GattDatabase, value: &[u8]) -> Result<(), u8> {
        let mut shared = lock(&self.0);
        shared.bytes += value.len() as u64;
        shared.chunks += 1;
        Ok(())
    }
}

/// The control point: begin resets, finish asks for the count.
#[derive(Debug)]
struct ControlHandler(Arc<Mutex<SinkShared>>);

impl AttributeHandler for ControlHandler {
    fn on_write(&mut self, _db: &mut GattDatabase, value: &[u8]) -> Result<(), u8> {
        let mut shared = lock(&self.0);
        match value.first().copied() {
            Some(control_op::BEGIN) => {
                shared.bytes = 0;
                shared.chunks = 0;
                shared.expected = match value.get(1..5) {
                    Some(bytes) => {
                        u64::from(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
                    }
                    None => 0,
                };
                shared.report_due = false;
            }
            Some(control_op::FINISH) => shared.report_due = true,
            // An opcode this sink does not know is not a protocol error
            // worth an ATT error code — a future central may write more.
            _ => {}
        }
        Ok(())
    }
}

/// A poisoned mutex here means a handler panicked mid-write; the counters
/// are still readable and a benchmark that refuses to report is worse than
/// one that reports a suspicious number.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// What the peripheral observed: the half of the measurement the central
/// cannot make.
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct SinkCounters {
    /// Bytes written to the data characteristic since the last `BEGIN`.
    pub bytes: u64,
    /// How many writes those bytes arrived in.
    pub chunks: u32,
    /// How many bytes the central said it would send.
    pub expected: u64,
    /// Caller-clock time the first byte arrived.
    pub first_byte_ms: Option<f64>,
    /// Caller-clock time the most recent byte arrived — the end of the
    /// transfer segment, and the only end worth quoting.
    pub last_byte_ms: Option<f64>,
}

/// The benchmark peripheral: advertises, serves the bulk service, counts
/// what lands and stamps when.
///
/// Transport-free in the same way [`LeHost`] is: H4 in, H4 out. The caller
/// owns the socket (netsim, the `simble --usb` bridge) or the in-process
/// [`Link`], and owns the clock.
pub struct BulkSink {
    device: VirtualDevice,
    host: LeHost,
    shared: Arc<Mutex<SinkShared>>,
    control_value_handle: u16,
    first_byte_ms: Option<f64>,
    last_byte_ms: Option<f64>,
    /// The count at the end of the previous packet, so an arrival can be
    /// stamped with the time the packet carrying it was processed.
    seen_bytes: u64,
}

impl BulkSink {
    /// Builds the sink. `name` is what it advertises as.
    pub fn new(name: &str, address: Address) -> Self {
        // Random, not Public: the benchmark names the address both ends
        // agree on, and only a random address can be set on a controller
        // that owns its public one.
        let mut device = VirtualDevice::new(name, address, AddressType::Random);
        let shared: Arc<Mutex<SinkShared>> = Arc::default();

        device.gatt_db.add_service(bulk_uuid::SERVICE, true);
        let (_, data_value_handle) = device.gatt_db.add_characteristic(
            bulk_uuid::DATA,
            CharacteristicProperties(
                CharacteristicProperties::WRITE | CharacteristicProperties::WRITE_WITHOUT_RESPONSE,
            ),
            Vec::new(),
            AttributePermissions::write_only(),
        );
        let (_, control_value_handle) = device.gatt_db.add_characteristic_with_cccd(
            bulk_uuid::CONTROL,
            CharacteristicProperties(
                CharacteristicProperties::WRITE | CharacteristicProperties::NOTIFY,
            ),
            Vec::new(),
            AttributePermissions::default(),
        );
        let _ = device
            .gatt_db
            .set_handler(data_value_handle, Box::new(DataHandler(shared.clone())));
        let _ = device.gatt_db.set_handler(
            control_value_handle,
            Box::new(ControlHandler(shared.clone())),
        );

        Self {
            device,
            host: LeHost::new(),
            shared,
            control_value_handle,
            first_byte_ms: None,
            last_byte_ms: None,
            seen_bytes: 0,
        }
    }

    /// The controller bring-up and advertising commands, as H4 packets.
    ///
    /// The service UUID is 128-bit and a legacy advertisement has 31 octets,
    /// so it is not advertised: the central finds this device by address,
    /// which is what the benchmark's caller already knows.
    pub fn start_commands(&self) -> Vec<Vec<u8>> {
        self.host
            .start_advertising(&self.device, &[])
            .unwrap_or_default()
    }

    /// Narrows the `LE_Event_Mask` this sink's bring-up asks for — pass
    /// [`LE_EVENT_MASK_CORE_4_0`](crate::device::host::LE_EVENT_MASK_CORE_4_0)
    /// for a dongle of unknown vintage, which refuses the wider mask outright
    /// and then never reports the connection.
    pub fn set_le_event_mask(&mut self, mask: [u8; 8]) {
        self.host.set_le_event_mask(mask);
    }

    /// The address this sink advertises with.
    pub fn address(&self) -> Address {
        self.device.address
    }

    /// Feeds one controller→host packet in and returns what to send back.
    ///
    /// `now_ms` stamps arrival: if this packet carried payload, the sink's
    /// last-byte time becomes `now_ms`. That is the timestamp the whole
    /// two-sided measurement rests on, so it is taken here — at the moment
    /// the bytes were actually processed — rather than on a later poll.
    pub fn on_packet(&mut self, packet: &[u8], now_ms: f64) -> Vec<Vec<u8>> {
        let out = self
            .host
            .handle_packet(&mut self.device, packet)
            .unwrap_or_default();
        let bytes = lock(&self.shared).bytes;
        if bytes > self.seen_bytes {
            self.first_byte_ms.get_or_insert(now_ms);
            self.last_byte_ms = Some(now_ms);
            self.seen_bytes = bytes;
        } else if bytes < self.seen_bytes {
            // A fresh BEGIN reset the counters under us.
            self.seen_bytes = bytes;
            self.first_byte_ms = None;
            self.last_byte_ms = None;
        }
        out
    }

    /// Anything the sink wants to send unprompted — currently only the
    /// report notification a `FINISH` write asked for.
    pub fn poll(&mut self) -> Vec<Vec<u8>> {
        let due = {
            let mut shared = lock(&self.shared);
            let due = shared.report_due;
            shared.report_due = false;
            due
        };
        if !due {
            return Vec::new();
        }
        let Some((connection_handle, _)) = self.host.connection() else {
            return Vec::new();
        };
        let counters = self.counters();
        let mut value = Vec::with_capacity(9);
        value.push(control_op::REPORT);
        value.extend_from_slice(&(counters.bytes as u32).to_le_bytes());
        value.extend_from_slice(&counters.chunks.to_le_bytes());
        let pdu = self.device.create_notification_for(
            connection_handle,
            self.control_value_handle,
            &value,
        );
        acl_packets(connection_handle, &pdu)
    }

    /// What the sink has seen.
    pub fn counters(&self) -> SinkCounters {
        let shared = lock(&self.shared);
        SinkCounters {
            bytes: shared.bytes,
            chunks: shared.chunks,
            expected: shared.expected,
            first_byte_ms: self.first_byte_ms,
            last_byte_ms: self.last_byte_ms,
        }
    }

    /// The GATT server, for a caller that wants to inspect the database.
    pub fn device(&self) -> &VirtualDevice {
        &self.device
    }

    /// Whether a central is connected.
    pub fn is_connected(&self) -> bool {
        self.host.connection().is_some()
    }
}

// ---------------------------------------------------------------------------
// The central half
// ---------------------------------------------------------------------------

/// Where the benchmark has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BulkPhase {
    /// [`BulkCentral::start`] has not been called.
    Idle,
    /// Controller bring-up and scanning, until the peer is heard.
    Discovering,
    /// LE Create Connection sent; waiting for the connection.
    Connecting,
    /// MTU exchange, service discovery, PHY request, control-point
    /// subscription.
    Negotiating,
    /// Bytes are moving.
    Transferring,
    /// Every byte was handed over and (where the peer says so) arrived.
    Complete,
    /// The run stopped; see [`BulkCentral::failure`].
    Failed,
}

impl BulkPhase {
    /// A stable identifier for a status document and a chart legend.
    pub fn name(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Discovering => "discover",
            Self::Connecting => "connect",
            Self::Negotiating => "negotiate",
            Self::Transferring => "transfer",
            Self::Complete => "complete",
            Self::Failed => "failed",
        }
    }
}

/// The sub-state of the transfer itself, once GATT discovery is done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Writing the control point's CCCD.
    Subscribing,
    /// The `BEGIN` write is in flight.
    Beginning,
    /// Chunks are being handed over.
    Streaming,
    /// Every chunk is out; the `FINISH` write is in flight and the sink's
    /// report is awaited.
    Finishing,
    /// Nothing left to do.
    Done,
}

/// One benchmark run, from the central's side.
///
/// The caller moves packets both ways and supplies the clock:
///
/// 1. [`Self::start`] → outgoing packets
/// 2. inbound packets → [`Self::on_packet`] → outgoing packets
/// 3. [`Self::step`] → outgoing packets, once per turn
pub struct BulkCentral {
    central: LeCentral,
    target: Address,
    options: BulkOptions,

    phase: BulkPhase,
    step: Step,
    sent: usize,
    chunks_sent: u32,
    chunk_bytes: usize,
    mtu: u16,
    tx_phy: Option<u8>,
    rx_phy: Option<u8>,
    phy_requested: bool,
    buffer_size_asked: bool,
    /// The controller's ACL buffer count, when it reports one.
    acl_total: Option<u16>,
    /// ACL packets handed over and not yet completed, when credits are known.
    acl_outstanding: u32,

    started_ms: Option<f64>,
    discover_end_ms: Option<f64>,
    connect_end_ms: Option<f64>,
    negotiate_end_ms: Option<f64>,
    last_queued_ms: Option<f64>,
    report_arrived_ms: Option<f64>,
    server_last_byte_ms: Option<f64>,
    last_progress_ms: f64,

    peer_bytes: Option<u64>,
    peer_chunks: Option<u32>,
    server_bytes: Option<u64>,
    server_chunks: Option<u32>,

    has_control: bool,
    failure: Option<String>,
    log: Vec<String>,
}

impl BulkCentral {
    /// A run against the peer at `target`, with the given settings.
    pub fn new(target: Address, options: BulkOptions) -> Self {
        Self {
            central: LeCentral::new(),
            target,
            options: options.sane(),
            phase: BulkPhase::Idle,
            step: Step::Subscribing,
            sent: 0,
            chunks_sent: 0,
            chunk_bytes: 0,
            mtu: 23,
            tx_phy: None,
            rx_phy: None,
            phy_requested: false,
            buffer_size_asked: false,
            acl_total: None,
            acl_outstanding: 0,
            started_ms: None,
            discover_end_ms: None,
            connect_end_ms: None,
            negotiate_end_ms: None,
            last_queued_ms: None,
            report_arrived_ms: None,
            server_last_byte_ms: None,
            last_progress_ms: 0.0,
            peer_bytes: None,
            peer_chunks: None,
            server_bytes: None,
            server_chunks: None,
            has_control: false,
            failure: None,
            log: Vec::new(),
        }
    }

    /// The settings this run is using.
    pub fn options(&self) -> &BulkOptions {
        &self.options
    }

    /// Narrows the `LE_Event_Mask` bring-up asks for — pass
    /// [`LE_EVENT_MASK_CORE_4_0`](crate::device::host::LE_EVENT_MASK_CORE_4_0)
    /// for a dongle of unknown vintage, which refuses the wider mask outright
    /// and then never reports an advertisement.
    pub fn set_le_event_mask(&mut self, mask: [u8; 8]) {
        self.central.set_le_event_mask(mask);
    }

    /// Begins the run at `now_ms`, returning the controller bring-up.
    pub fn start(&mut self, now_ms: f64) -> Vec<Vec<u8>> {
        self.started_ms = Some(now_ms);
        self.last_progress_ms = now_ms;
        self.phase = BulkPhase::Discovering;
        self.log.push(format!(
            "run started: {} bytes to {}",
            self.options.total_bytes, self.target
        ));
        let mut out = vec![command(LE_READ_BUFFER_SIZE, &[])];
        out.extend(self.central.connect(self.target));
        out
    }

    /// Feeds one controller→host H4 packet in.
    pub fn on_packet(&mut self, packet: &[u8], now_ms: f64) -> Vec<Vec<u8>> {
        self.observe_controller(packet);
        let mut out = self.central.on_packet(packet);
        self.drain_events(now_ms);
        out.extend(self.step(now_ms));
        self.charge(&out);
        out
    }

    /// One turn of the run: advances the phase clock and hands over as much
    /// payload as the controller will take.
    pub fn step(&mut self, now_ms: f64) -> Vec<Vec<u8>> {
        if matches!(self.phase, BulkPhase::Idle | BulkPhase::Complete) || self.failure.is_some() {
            return Vec::new();
        }
        self.advance_phase(now_ms);
        let mut out = Vec::new();
        if self.phase == BulkPhase::Negotiating || self.phase == BulkPhase::Transferring {
            self.drive(now_ms, &mut out);
        }
        out.extend(self.central.pump());
        self.watchdog(now_ms);
        self.charge(&out);
        out
    }

    /// Tells the run what the peripheral saw, on the *caller's* clock.
    ///
    /// This is what turns a one-sided stopwatch into a measurement: where
    /// both ends are the caller's — the in-page link, or netsim with a
    /// socket each — the sink's last-byte time is the end of the transfer,
    /// and its byte count is the only way loss becomes visible.
    pub fn note_server(&mut self, counters: SinkCounters) {
        self.server_bytes = Some(counters.bytes);
        self.server_chunks = Some(counters.chunks);
        self.server_last_byte_ms = counters.last_byte_ms;
    }

    /// Progress lines since the last call.
    pub fn take_log(&mut self) -> Vec<String> {
        std::mem::take(&mut self.log)
    }

    /// The current phase.
    pub fn phase(&self) -> BulkPhase {
        self.phase
    }

    /// Why the run stopped, if it did.
    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    /// Whether the run reached its end, successfully or not.
    pub fn is_finished(&self) -> bool {
        matches!(self.phase, BulkPhase::Complete | BulkPhase::Failed)
    }

    /// The underlying central, for a caller that wants its discovered GATT.
    pub fn central(&self) -> &LeCentral {
        &self.central
    }

    // -- the phase clock ----------------------------------------------------

    /// Stamps the boundary between segments from the central's own phase.
    fn advance_phase(&mut self, now_ms: f64) {
        match self.central.phase() {
            CentralPhase::Idle | CentralPhase::Initializing | CentralPhase::Scanning => {}
            CentralPhase::Connecting => {
                if self.discover_end_ms.is_none() {
                    self.discover_end_ms = Some(now_ms);
                    self.phase = BulkPhase::Connecting;
                    self.progress(now_ms, "heard the peer; connecting");
                }
            }
            CentralPhase::ExchangingMtu
            | CentralPhase::DiscoveringServices
            | CentralPhase::DiscoveringCharacteristics(_) => {
                self.discover_end_ms.get_or_insert(now_ms);
                if self.connect_end_ms.is_none() {
                    self.connect_end_ms = Some(now_ms);
                    self.phase = BulkPhase::Negotiating;
                    self.progress(now_ms, "connected; negotiating");
                }
            }
            CentralPhase::Ready => {
                self.discover_end_ms.get_or_insert(now_ms);
                self.connect_end_ms.get_or_insert(now_ms);
                if self.phase == BulkPhase::Connecting {
                    self.phase = BulkPhase::Negotiating;
                }
            }
            CentralPhase::Disconnected => {
                if self.failure.is_none() && self.phase != BulkPhase::Complete {
                    self.fail(format!("the link dropped during {}", self.phase.name()));
                }
            }
        }
    }

    /// Everything the central does once its GATT view is complete.
    fn drive(&mut self, now_ms: f64, out: &mut Vec<Vec<u8>>) {
        if self.central.phase() != CentralPhase::Ready {
            return;
        }
        if !self.phy_requested {
            self.phy_requested = true;
            // All PHYs allowed both ways, no preference among coded options.
            // A controller that has never heard of the command answers
            // Unknown HCI Command and the report simply says the PHY was not
            // reported, which is the honest outcome.
            let handle = self.central.connection_handle();
            let mut params = Vec::with_capacity(7);
            params.extend_from_slice(&handle.to_le_bytes());
            params.push(0x00); // all_phys: both TX and RX are specified below
            params.push(0x07); // tx_phys: 1M | 2M | Coded
            params.push(0x07); // rx_phys: 1M | 2M | Coded
            params.extend_from_slice(&0u16.to_le_bytes()); // phy_options
            out.push(command(LE_SET_PHY, &params));
        }
        match self.step {
            Step::Subscribing => self.begin_subscribing(now_ms),
            Step::Beginning => {}
            Step::Streaming => self.stream(now_ms),
            Step::Finishing | Step::Done => {}
        }
    }

    /// Subscribes to the control point, or — if the peer has none — falls
    /// back to an unconfirmed run.
    fn begin_subscribing(&mut self, now_ms: f64) {
        if self.central.value_handle(bulk_uuid::DATA).is_none() {
            self.fail(
                "the peer has no Bulk Transfer data characteristic — it is not a benchmark sink"
                    .to_string(),
            );
            return;
        }
        self.mtu = self.central.mtu();
        self.chunk_bytes = usize::from(self.mtu)
            .saturating_sub(ATT_WRITE_OVERHEAD)
            .max(1);
        if self.central.value_handle(bulk_uuid::CONTROL).is_some() {
            self.has_control = true;
            self.central.queue_subscribe(bulk_uuid::CONTROL, true);
            self.step = Step::Beginning;
            self.progress(now_ms, "subscribing to the peer's control point");
            return;
        }
        // No control point: the peer is somebody else's. The bytes still go,
        // and the report will say the arrival was never confirmed.
        self.log.push(
            "the peer has no control point — this run measures bytes SENT, not delivered"
                .to_string(),
        );
        self.enter_transfer(now_ms);
    }

    /// Leaves negotiation and starts the transfer segment.
    fn enter_transfer(&mut self, now_ms: f64) {
        self.negotiate_end_ms = Some(now_ms);
        self.phase = BulkPhase::Transferring;
        self.step = Step::Streaming;
        self.progress(
            now_ms,
            &format!(
                "transferring {} bytes in {}-byte chunks (MTU {})",
                self.options.total_bytes, self.chunk_bytes, self.mtu
            ),
        );
    }

    /// Hands over as many chunks as the controller will accept.
    ///
    /// Refilling only once the ATT queue has drained is what keeps `sent`
    /// meaning *handed to the controller* rather than *pushed into a Rust
    /// `VecDeque`*. Without the gate an acknowledged run would queue all
    /// five hundred writes in nine turns and declare itself finished while
    /// the link had carried almost nothing.
    fn stream(&mut self, now_ms: f64) {
        if self.chunk_bytes == 0 || !self.central.is_idle() {
            return;
        }
        let mut queued = 0usize;
        while self.sent < self.options.total_bytes && queued < self.budget_chunks() {
            let len = self.chunk_bytes.min(self.options.total_bytes - self.sent);
            let index = self.chunks_sent;
            // A recognisable, non-constant payload: a link or a bridge that
            // silently compresses runs of zeroes would otherwise flatter the
            // number.
            let value: Vec<u8> = (0..len)
                .map(|i| (i as u32).wrapping_add(index).wrapping_mul(31) as u8)
                .collect();
            self.central
                .queue_write(bulk_uuid::DATA, value, self.options.with_response);
            self.sent += len;
            self.chunks_sent += 1;
            queued += 1;
        }
        if queued > 0 {
            self.last_queued_ms = Some(now_ms);
            self.progress_silently(now_ms);
        }
        if self.sent >= self.options.total_bytes && self.step == Step::Streaming {
            self.step = Step::Finishing;
            if self.has_control {
                self.central
                    .queue_write(bulk_uuid::CONTROL, vec![control_op::FINISH], true);
                self.progress(
                    now_ms,
                    "all chunks handed over; asking the peer for its count",
                );
            } else {
                self.progress(now_ms, "all chunks handed over; nothing will confirm them");
            }
        }
    }

    /// How many more chunks may be handed over this turn.
    ///
    /// With a controller that reports its ACL buffers this is real flow
    /// control — a chunk costs as many buffers as it takes fragments, and a
    /// host that ignores that overruns a real dongle. Without one it is the
    /// fixed window, which is all an in-process controller with an unbounded
    /// queue needs.
    fn budget_chunks(&self) -> usize {
        let per_chunk = self.fragments_per_chunk().max(1);
        match self.acl_total {
            Some(total) if total > 0 => {
                let free = usize::from(total).saturating_sub(self.acl_outstanding as usize);
                let chunks = free / per_chunk;
                // One MTU-sized write can need more ACL buffers than a small
                // dongle owns (twenty fragments against eight buffers is an
                // ordinary CSR8510). The queue below cannot split a PDU
                // across a refill, so the only safe moment for such a chunk
                // is with the pool empty — which is still correct pacing,
                // just a slow kind.
                if chunks == 0 && self.acl_outstanding == 0 {
                    1
                } else {
                    chunks
                }
            }
            _ => self.options.window_chunks,
        }
    }

    /// How many H4 ACL packets one chunk becomes.
    fn fragments_per_chunk(&self) -> usize {
        let l2cap = L2CAP_HEADER + ATT_WRITE_OVERHEAD + self.chunk_bytes;
        l2cap.div_ceil(LE_ACL_DATA_LEN)
    }

    /// Counts outgoing ACL packets against the controller's buffers.
    fn charge(&mut self, packets: &[Vec<u8>]) {
        if self.acl_total.is_none() {
            return;
        }
        let sent = packets
            .iter()
            .filter(|p| p.first() == Some(&crate::transport::h4_type::HCI_ACL_DATA))
            .count();
        self.acl_outstanding += sent as u32;
    }

    /// Reads the two controller answers this run cares about out of the
    /// event stream, before [`LeCentral`] (which ignores both) sees it.
    fn observe_controller(&mut self, packet: &[u8]) {
        if packet.first() != Some(&crate::transport::h4_type::HCI_EVENT) {
            return;
        }
        let Some(code) = packet.get(1).copied() else {
            return;
        };
        let params = &packet[3.min(packet.len())..];
        match code {
            crate::packets::hci_events::event_code::COMMAND_COMPLETE => {
                if params.get(1..3) != Some(&LE_READ_BUFFER_SIZE[..]) {
                    return;
                }
                // status, LE_ACL_Data_Packet_Length (2), Total_Num (1). A
                // controller that does not implement the command answers
                // with the status alone, which is why the length is checked
                // rather than assumed.
                if params.get(3) == Some(&0x00)
                    && let Some(total) = params.get(6).copied()
                    && total > 0
                {
                    self.acl_total = Some(u16::from(total));
                    self.log
                        .push(format!("the controller reports {total} LE ACL buffers"));
                }
                self.buffer_size_asked = true;
            }
            NUMBER_OF_COMPLETED_PACKETS => {
                let Some(&handles) = params.first() else {
                    return;
                };
                let mut completed = 0u32;
                for i in 0..usize::from(handles) {
                    let at = 1 + usize::from(handles) * 2 + i * 2;
                    if let Some(bytes) = params.get(at..at + 2) {
                        completed += u32::from(u16::from_le_bytes([bytes[0], bytes[1]]));
                    }
                }
                self.acl_outstanding = self.acl_outstanding.saturating_sub(completed);
            }
            crate::packets::hci_events::event_code::LE_META => {
                if params.first() != Some(&LE_PHY_UPDATE_COMPLETE) {
                    return;
                }
                if params.get(1) == Some(&0x00) {
                    self.tx_phy = params.get(4).copied();
                    self.rx_phy = params.get(5).copied();
                    if let (Some(tx), Some(rx)) = (self.tx_phy, self.rx_phy) {
                        self.log.push(format!(
                            "PHY update: TX {} RX {}",
                            phy_label(tx).unwrap_or("?"),
                            phy_label(rx).unwrap_or("?")
                        ));
                    }
                }
            }
            _ => {}
        }
    }

    /// Turns the central's events into segment boundaries.
    fn drain_events(&mut self, now_ms: f64) {
        for event in self.central.take_events() {
            match event {
                CentralEvent::ConnectionStateChange {
                    connected: false,
                    status,
                    ..
                } => {
                    if self.phase != BulkPhase::Complete && self.failure.is_none() {
                        self.fail(format!(
                            "the link went away during {} (status {status:#04x})",
                            self.phase.name()
                        ));
                    }
                }
                CentralEvent::MtuChanged { mtu } => {
                    self.mtu = mtu;
                    self.progress(now_ms, &format!("ATT MTU {mtu}"));
                }
                CentralEvent::ServicesDiscovered { services } => {
                    self.progress(now_ms, &format!("{services} service(s) discovered"));
                }
                CentralEvent::SubscriptionChanged {
                    uuid,
                    enabled,
                    status,
                    ..
                } if uuid == bulk_uuid::CONTROL => {
                    if !enabled || status != 0 {
                        self.fail(format!(
                            "the peer refused the control-point subscription (status {status:#04x})"
                        ));
                        continue;
                    }
                    let mut value = Vec::with_capacity(5);
                    value.push(control_op::BEGIN);
                    value.extend_from_slice(&(self.options.total_bytes as u32).to_le_bytes());
                    self.central.queue_write(bulk_uuid::CONTROL, value, true);
                }
                CentralEvent::CharacteristicWrite { uuid, status, .. }
                    if uuid == bulk_uuid::CONTROL =>
                {
                    if status != 0 {
                        self.fail(format!("the control point refused a write ({status:#04x})"));
                        continue;
                    }
                    if self.step == Step::Beginning {
                        self.enter_transfer(now_ms);
                    }
                }
                CentralEvent::CharacteristicChanged { uuid, value, .. }
                    if uuid == bulk_uuid::CONTROL =>
                {
                    self.on_peer_report(&value, now_ms);
                }
                CentralEvent::OperationFailed {
                    uuid,
                    operation,
                    reason,
                } => self.fail(format!("{operation} on {uuid} failed: {reason}")),
                _ => {}
            }
        }
    }

    /// The sink's own count, arriving as a notification.
    fn on_peer_report(&mut self, value: &[u8], now_ms: f64) {
        if value.first() != Some(&control_op::REPORT) || value.len() < 9 {
            return;
        }
        let bytes = u32::from_le_bytes([value[1], value[2], value[3], value[4]]);
        let chunks = u32::from_le_bytes([value[5], value[6], value[7], value[8]]);
        self.peer_bytes = Some(u64::from(bytes));
        self.peer_chunks = Some(chunks);
        self.report_arrived_ms = Some(now_ms);
        self.step = Step::Done;
        if u64::from(bytes) < self.sent as u64 {
            self.progress(
                now_ms,
                &format!(
                    "the peer received {bytes} of {} bytes — {} lost",
                    self.sent,
                    self.sent as u64 - u64::from(bytes)
                ),
            );
        } else {
            self.progress(now_ms, &format!("the peer received all {bytes} bytes"));
        }
        self.phase = BulkPhase::Complete;
    }

    /// Fails a run that has stopped making progress, naming the segment it
    /// stopped in. A stall is a measurement too, and one that says where.
    fn watchdog(&mut self, now_ms: f64) {
        if self.is_finished() || self.failure.is_some() {
            return;
        }
        if now_ms - self.last_progress_ms > self.options.timeout_ms {
            let detail = match self.phase {
                BulkPhase::Discovering => " — is the peer advertising?".to_string(),
                BulkPhase::Transferring => {
                    format!(
                        " — {} of {} bytes handed over",
                        self.sent, self.options.total_bytes
                    )
                }
                _ => String::new(),
            };
            self.fail(format!(
                "stalled in {} for {:.0} ms{detail}",
                self.phase.name(),
                now_ms - self.last_progress_ms
            ));
        }
        // An unconfirmed run has nobody to tell it the bytes landed, so it
        // ends when the last one has been handed to the controller and the
        // ATT queue has drained. That is a weaker claim than arrival and the
        // report says so.
        if !self.has_control
            && self.step == Step::Finishing
            && self.central.is_idle()
            && self.acl_outstanding == 0
        {
            self.last_queued_ms = Some(now_ms);
            self.step = Step::Done;
            self.phase = BulkPhase::Complete;
        }
    }

    fn progress(&mut self, now_ms: f64, line: &str) {
        self.last_progress_ms = now_ms;
        self.log.push(line.to_string());
    }

    fn progress_silently(&mut self, now_ms: f64) {
        self.last_progress_ms = now_ms;
    }

    fn fail(&mut self, reason: String) {
        if self.failure.is_some() {
            return;
        }
        self.log.push(format!("FAIL: {reason}"));
        self.failure = Some(reason);
        self.phase = BulkPhase::Failed;
        self.step = Step::Done;
    }

    // -- the report ---------------------------------------------------------

    /// When the transfer segment ended, and how much that end can be trusted.
    fn transfer_end(&self) -> (Option<f64>, &'static str) {
        if let Some(stamped) = self.server_last_byte_ms {
            return (Some(stamped), "server-stamped");
        }
        if let Some(arrived) = self.report_arrived_ms {
            return (Some(arrived), "peer-reported");
        }
        (self.last_queued_ms, "unconfirmed")
    }

    /// Everything the run measured.
    pub fn report(&self) -> BulkReport {
        let (transfer_end_ms, confirmation) = self.transfer_end();
        let span = |from: Option<f64>, to: Option<f64>| match (from, to) {
            (Some(a), Some(b)) if b >= a => Some(b - a),
            _ => None,
        };
        let discover_ms = span(self.started_ms, self.discover_end_ms);
        let connect_ms = span(self.discover_end_ms, self.connect_end_ms);
        let negotiate_ms = span(self.connect_end_ms, self.negotiate_end_ms);
        let transfer_ms = span(self.negotiate_end_ms, transfer_end_ms);
        let total_ms = span(self.started_ms, transfer_end_ms);

        let bytes_received = self.server_bytes.or(self.peer_bytes);
        let landed = bytes_received.unwrap_or(self.sent as u64);
        let throughput_kb_s = transfer_ms
            .filter(|ms| *ms > 0.0)
            .map(|ms| landed as f64 / 1024.0 / (ms / 1000.0));

        BulkReport {
            phase: self.phase.name(),
            complete: self.phase == BulkPhase::Complete,
            failure: self.failure.clone(),
            peer: self.target.to_string(),
            requested_bytes: self.options.total_bytes as u64,
            bytes_sent: self.sent as u64,
            bytes_received,
            chunks_sent: self.chunks_sent,
            chunks_received: self.server_chunks.or(self.peer_chunks),
            chunk_bytes: self.chunk_bytes as u32,
            mtu: self.mtu,
            tx_phy: self.tx_phy.and_then(phy_label),
            rx_phy: self.rx_phy.and_then(phy_label),
            window_chunks: self.options.window_chunks as u32,
            with_response: self.options.with_response,
            acl_credits: self.acl_total,
            discover_ms,
            connect_ms,
            negotiate_ms,
            transfer_ms,
            total_ms,
            throughput_kb_s,
            confirmation,
        }
    }

    /// The report as JSON, for a page to render.
    pub fn report_json(&self) -> String {
        serde_json::to_string(&self.report()).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
    }
}

/// What one run measured. Every duration is milliseconds on the caller's
/// clock; `None` means that segment never happened.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct BulkReport {
    /// Where the run got to ([`BulkPhase::name`]).
    pub phase: &'static str,
    /// Whether every byte was handed over and, where the peer says so,
    /// arrived.
    pub complete: bool,
    /// Why the run stopped, if it did. A failed run is still a measurement.
    pub failure: Option<String>,
    /// The peer written to.
    pub peer: String,
    /// The transfer size asked for.
    pub requested_bytes: u64,
    /// Bytes the central handed to its controller.
    pub bytes_sent: u64,
    /// Bytes the peripheral says it received. `None` on an unconfirmed run —
    /// and a reader must not be shown `bytes_sent` in its place without
    /// being told.
    pub bytes_received: Option<u64>,
    /// Writes the central issued.
    pub chunks_sent: u32,
    /// Writes the peripheral counted.
    pub chunks_received: Option<u32>,
    /// The payload each write carried (MTU − 3).
    pub chunk_bytes: u32,
    /// The negotiated ATT MTU.
    pub mtu: u16,
    /// The transmit PHY, where the controller reported one.
    pub tx_phy: Option<&'static str>,
    /// The receive PHY, where the controller reported one.
    pub rx_phy: Option<&'static str>,
    /// The fixed chunk window used when the controller reports no buffers.
    pub window_chunks: u32,
    /// Whether each chunk was an acknowledged Write Request.
    pub with_response: bool,
    /// The controller's LE ACL buffer count, when it reports one — the run
    /// then obeys real Number Of Completed Packets flow control.
    pub acl_credits: Option<u16>,
    /// Bring-up and scanning, until the peer was heard.
    pub discover_ms: Option<f64>,
    /// LE Create Connection until LE Connection Complete.
    pub connect_ms: Option<f64>,
    /// MTU exchange, service discovery and the control-point subscription.
    pub negotiate_ms: Option<f64>,
    /// The first chunk out until the last byte in.
    pub transfer_ms: Option<f64>,
    /// Run start until the last byte in — the honest answer to "how long
    /// until 256 KB lands".
    pub total_ms: Option<f64>,
    /// Bytes that landed, over the transfer segment only. Never over the
    /// whole run: that would fold the setup latency into the pipe width.
    pub throughput_kb_s: Option<f64>,
    /// How the end of the transfer was established: `server-stamped`,
    /// `peer-reported`, or `unconfirmed`.
    pub confirmation: &'static str,
}

// ---------------------------------------------------------------------------
// Both ends on one simulated medium
// ---------------------------------------------------------------------------

/// The benchmark with both ends in one process, over
/// [`Link`] — the browser's "in-page
/// controller", and the shape the unit tests drive.
///
/// Nothing here is wired directly together: the bytes cross a simulated
/// radio, get fragmented into 27-octet ACL packets and reassembled, and pass
/// through the same ATT server a netsim peripheral runs. What it does *not*
/// model is airtime, which is exactly why the page must label these numbers
/// as simulation.
pub struct ThroughputScene {
    link: Link,
    sink_channel: std::sync::Arc<HciChannel>,
    central_channel: std::sync::Arc<HciChannel>,
    sink: BulkSink,
    runner: BulkCentral,
    started: bool,
}

impl ThroughputScene {
    /// Builds the scene: a sink at `sink_address`, and a central at
    /// `central_address` that will write to it under `options`.
    pub fn new(sink_address: Address, central_address: Address, options: BulkOptions) -> Self {
        let mut link = Link::new();
        let sink_channel = link.add_device(sink_address);
        let central_channel = link.add_device(central_address);
        Self {
            link,
            sink_channel,
            central_channel,
            sink: BulkSink::new("simble-bulk-sink", sink_address),
            runner: BulkCentral::new(sink_address, options),
            started: false,
        }
    }

    /// Advances everything one step at `now_ms`: both ends produce, the
    /// medium routes, both ends consume, and the central is told what the
    /// sink saw.
    pub fn tick(&mut self, now_ms: f64) {
        if !self.started {
            for packet in self.sink.start_commands() {
                let _ = self.sink_channel.inject_host_packet(packet);
            }
            for packet in self.runner.start(now_ms) {
                let _ = self.central_channel.inject_host_packet(packet);
            }
            self.started = true;
        }
        for packet in self.sink.poll() {
            let _ = self.sink_channel.inject_host_packet(packet);
        }
        for packet in self.runner.step(now_ms) {
            let _ = self.central_channel.inject_host_packet(packet);
        }

        self.link.tick();

        while let Some(packet) = self.sink_channel.poll_controller_packet() {
            for out in self.sink.on_packet(&packet, now_ms) {
                let _ = self.sink_channel.inject_host_packet(out);
            }
        }
        while let Some(packet) = self.central_channel.poll_controller_packet() {
            for out in self.runner.on_packet(&packet, now_ms) {
                let _ = self.central_channel.inject_host_packet(out);
            }
        }
        // Both ends share this caller's clock, so the sink's arrival stamp is
        // directly comparable with the central's start — which is what makes
        // an in-page run "server-stamped" rather than merely "peer-reported".
        self.runner.note_server(self.sink.counters());
    }

    /// Runs until the benchmark finishes or `steps` have passed. `clock`
    /// supplies `now_ms` for each step, so a test can drive a fake clock and
    /// a page can pass its own.
    pub fn run(&mut self, steps: usize, mut clock: impl FnMut(usize) -> f64) -> bool {
        for step in 0..steps {
            if self.runner.is_finished() {
                return true;
            }
            self.tick(clock(step));
        }
        self.runner.is_finished()
    }

    /// The central half.
    pub fn central(&self) -> &BulkCentral {
        &self.runner
    }

    /// The central half, mutably — for draining its log.
    pub fn central_mut(&mut self) -> &mut BulkCentral {
        &mut self.runner
    }

    /// The peripheral half.
    pub fn sink(&self) -> &BulkSink {
        &self.sink
    }

    /// What the run measured.
    pub fn report(&self) -> BulkReport {
        self.runner.report()
    }

    /// What the run measured, as JSON.
    pub fn report_json(&self) -> String {
        self.runner.report_json()
    }
}

#[cfg(test)]
#[path = "throughput_tests.rs"]
mod tests;
