// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! WebSocket HCI transport to Android's netsim (Simble's primary client),
//! carrying the same H4-framed packets as `rootcanal::RootcanalTransport`
//! but over a WebSocket connection instead of a raw TCP pipe. netsim's real
//! endpoint, confirmed against its own source
//! (`rust/daemon/src/transport/websocket.rs`), is
//! `ws://localhost:7681/v1/websocket/bt?name=<device-name>&address=<mac>` —
//! the device's name and address are read straight from the connection URI's
//! query string, no separate handshake message. See the README's "Testing
//! Against netsim" section: this endpoint requires Android Studio Canary,
//! not Stable, whose bundled `netsimd` doesn't start the WebSocket frontend
//! server at all.
//!
//! This is the *client* half of the WebSocket link; the RFC 6455 codec it
//! shares with the `usb-ble-ws` *server* ([`super::ws`]) lives there. All this
//! module adds is the client-side opening handshake and the [`NetsimTransport`]
//! pump (which masks its outgoing frames, as clients must).

use super::ws::{
    DecodedFrame, OPCODE_BINARY, OPCODE_CLOSE, OPCODE_CONTINUATION, OPCODE_PING, OPCODE_PONG,
    WsFrameReader, encode_frame, encode_masked_binary_frame, expected_accept, mask_key,
    read_http_headers,
};
use crate::types::SimbleError;
use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned,
    byteorder::big_endian::{U32, U64},
};

/// btsnoop file header: the identification pattern, then the format version
/// and datalink type.
///
/// Every multi-byte field in btsnoop is **big-endian**, unlike the
/// little-endian HCI structures everywhere else in this crate. Writing these
/// fields little-endian yields a file that tools reject or, worse, misread —
/// so the byte order is pinned by test rather than left to inspection.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
struct BtsnoopFileHeader {
    /// Always `b"btsnoop\0"`.
    identification: [u8; 8],
    /// Format version number; 1 is the only version in use.
    version: U32,
    /// Datalink type; 1002 is HCI UART (H4).
    datalink: U32,
}

impl BtsnoopFileHeader {
    /// Datalink type for HCI UART (H4) — the framing this transport carries.
    const DATALINK_H4: u32 = 1002;

    /// Builds the fixed 16-byte header written once at the start of a trace.
    fn new() -> Self {
        Self {
            identification: *b"btsnoop\0",
            version: U32::new(1),
            datalink: U32::new(Self::DATALINK_H4),
        }
    }
}

/// btsnoop per-packet record header, immediately followed by the packet bytes.
/// Big-endian throughout, like [`BtsnoopFileHeader`].
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
struct BtsnoopRecordHeader {
    /// Length of the original packet.
    original_length: U32,
    /// Length actually captured; equal to `original_length` (never truncated).
    included_length: U32,
    /// Per-packet flags; see [`Self::FLAG_RECEIVED`].
    flags: U32,
    /// Packets dropped since the previous record; always 0 here.
    cumulative_drops: U32,
    /// Capture timestamp, microseconds since year 0.
    timestamp_micros: U64,
}

impl BtsnoopRecordHeader {
    /// btsnoop timestamps are microseconds since year 0; this constant is the
    /// Unix epoch on that scale (what Wireshark expects).
    const UNIX_EPOCH_OFFSET: u64 = 0x00DC_DDB3_0F2F_8000;
    /// Flags bit 0: set when the packet travelled controller→host.
    const FLAG_RECEIVED: u32 = 0x01;

    /// Builds a record header for a packet of `len` bytes captured at
    /// `micros` (already offset to the btsnoop epoch).
    fn new(len: u32, received: bool, micros: u64) -> Self {
        Self {
            original_length: U32::new(len),
            included_length: U32::new(len),
            flags: U32::new(if received { Self::FLAG_RECEIVED } else { 0 }),
            cumulative_drops: U32::new(0),
            timestamp_micros: U64::new(micros),
        }
    }
}

/// A `ws://host:port/path` URL, split into the pieces the handshake needs.
struct WsUrl {
    host: String,
    port: u16,
    path: String,
}

fn parse_ws_url(url: &str) -> Result<WsUrl, SimbleError> {
    let rest = url
        .strip_prefix("ws://")
        .ok_or_else(|| SimbleError::Transport(format!("unsupported WebSocket URL: {url}")))?;
    let (authority, path) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (
            h.to_string(),
            p.parse::<u16>()
                .map_err(|_| SimbleError::Transport(format!("invalid port in URL: {url}")))?,
        ),
        None => (authority.to_string(), 80),
    };
    if host.is_empty() {
        return Err(SimbleError::Transport(format!(
            "missing host in URL: {url}"
        )));
    }
    Ok(WsUrl {
        host,
        port,
        path: path.to_string(),
    })
}

fn build_handshake_request(host: &str, port: u16, path: &str, key: &str) -> String {
    format!(
        "GET {path} HTTP/1.1\r\n\
         Host: {host}:{port}\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Key: {key}\r\n\
         Sec-WebSocket-Version: 13\r\n\
         \r\n"
    )
}

fn validate_handshake_response(response: &str, key: &str) -> Result<(), SimbleError> {
    let mut lines = response.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| SimbleError::Transport("empty handshake response".to_string()))?;
    if !status_line.contains("101") {
        return Err(SimbleError::Transport(format!(
            "handshake rejected: {status_line}"
        )));
    }

    let mut accept = None;
    for line in lines {
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("Sec-WebSocket-Accept")
        {
            accept = Some(value.trim().to_string());
        }
    }
    let accept = accept.ok_or_else(|| {
        SimbleError::Transport("handshake response missing Sec-WebSocket-Accept".to_string())
    })?;
    let expected = expected_accept(key);
    if accept != expected {
        return Err(SimbleError::Transport(format!(
            "Sec-WebSocket-Accept mismatch: got {accept}, expected {expected}"
        )));
    }
    Ok(())
}

fn perform_handshake<S: Read + Write>(
    stream: &mut S,
    host: &str,
    port: u16,
    path: &str,
) -> Result<(), SimbleError> {
    let key = super::ws::base64_encode(&super::ws::pseudo_random_bytes(16));
    let request = build_handshake_request(host, port, path, &key);
    stream
        .write_all(request.as_bytes())
        .map_err(|e| SimbleError::Transport(e.to_string()))?;
    let response = read_http_headers(stream)?;
    validate_handshake_response(&response, &key)
}

/// Bidirectional HCI transport to netsim over a hand-rolled WebSocket
/// client, generic over any `Read + Write` stream (a `TcpStream` in
/// practice, matching netsim's `ws://host:port/hci` interface).
pub struct NetsimTransport<S: Read + Write> {
    stream: S,
    reader: WsFrameReader,
    /// Accumulates a message's payload across `Continuation` frames — not
    /// expected from netsim in practice (H4 packets are sent as single
    /// unfragmented binary frames both ways) but handled for spec fidelity.
    fragment: Vec<u8>,
    fragment_opcode: Option<u8>,
    /// Optional btsnoop capture of every H4 packet both ways (Wireshark
    /// opens it directly) — the HCI-level equivalent of a sniffer, for
    /// debugging exchanges against real stacks. See [`Self::set_trace`].
    trace: Option<std::fs::File>,
}

impl NetsimTransport<TcpStream> {
    /// Connects to netsim at `url` (e.g. `"ws://localhost:9922/hci"`):
    /// opens a TCP connection, performs the WebSocket opening handshake,
    /// then puts the socket in non-blocking mode so `pump` never stalls.
    pub fn connect(url: &str) -> Result<Self, SimbleError> {
        let parsed = parse_ws_url(url)?;
        let mut stream = TcpStream::connect((parsed.host.as_str(), parsed.port)).map_err(|e| {
            // A refused connection almost always means netsimd isn't running
            // (or was started without its WebSocket frontend). Turn the bare
            // OS error into an actionable hint rather than "os error 61".
            if e.kind() == ErrorKind::ConnectionRefused {
                SimbleError::Transport(format!(
                    "could not reach netsim at {}:{} — is netsimd running with its \
                     WebSocket frontend enabled? Start it with:\n    \
                     netsimd --logtostderr --no-shutdown --ws-port {}\n  \
                     (needs the canary-channel emulator; see the README's \"Testing \
                     Against netsim\" section). Underlying error: {e}",
                    parsed.host, parsed.port, parsed.port
                ))
            } else {
                SimbleError::Transport(e.to_string())
            }
        })?;
        perform_handshake(&mut stream, &parsed.host, parsed.port, &parsed.path)?;
        stream
            .set_nonblocking(true)
            .map_err(|e| SimbleError::Transport(e.to_string()))?;
        Ok(Self::new(stream))
    }
}

impl<S: Read + Write> NetsimTransport<S> {
    /// Wraps an already-connected WebSocket stream as a netsim transport.
    pub fn new(stream: S) -> Self {
        Self {
            stream,
            reader: WsFrameReader::default(),
            fragment: Vec::new(),
            fragment_opcode: None,
            trace: None,
        }
    }

    /// Starts capturing every H4 packet (both directions) into `file` in
    /// btsnoop format (datalink 1002, HCI UART/H4) — the format Wireshark
    /// and `tshark` read natively. The header is written immediately.
    pub fn set_trace(&mut self, mut file: std::fs::File) -> std::io::Result<()> {
        file.write_all(BtsnoopFileHeader::new().as_bytes())?;
        self.trace = Some(file);
        Ok(())
    }

    /// Appends one btsnoop record; `received` is the direction flag
    /// (false = host→controller). Trace failures are swallowed — capture
    /// must never break the transport.
    fn trace_record(&mut self, packet: &[u8], received: bool) {
        let Some(file) = self.trace.as_mut() else {
            return;
        };
        let micros = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_micros() as u64)
            .unwrap_or(0)
            + BtsnoopRecordHeader::UNIX_EPOCH_OFFSET;
        let header = BtsnoopRecordHeader::new(packet.len() as u32, received, micros);
        let mut record = Vec::with_capacity(size_of::<BtsnoopRecordHeader>() + packet.len());
        record.extend_from_slice(header.as_bytes());
        record.extend_from_slice(packet);
        let _ = file.write_all(&record);
        let _ = file.flush();
    }

    fn handle_frame(&mut self, frame: DecodedFrame) -> Result<Option<Vec<u8>>, SimbleError> {
        match frame.opcode {
            OPCODE_BINARY | OPCODE_CONTINUATION => {
                if frame.opcode == OPCODE_BINARY {
                    self.fragment_opcode = Some(OPCODE_BINARY);
                    self.fragment.clear();
                }
                self.fragment.extend_from_slice(&frame.payload);
                if frame.fin && self.fragment_opcode.is_some() {
                    self.fragment_opcode = None;
                    return Ok(Some(std::mem::take(&mut self.fragment)));
                }
                Ok(None)
            }
            OPCODE_PING => {
                let pong = encode_frame(OPCODE_PONG, &frame.payload, Some(mask_key()));
                self.stream
                    .write_all(&pong)
                    .map_err(|e| SimbleError::Transport(e.to_string()))?;
                Ok(None)
            }
            OPCODE_PONG => Ok(None),
            OPCODE_CLOSE => Err(SimbleError::Transport(
                "netsim closed the WebSocket connection".to_string(),
            )),
            other => Err(SimbleError::Transport(format!(
                "unsupported WebSocket opcode {other:#x}"
            ))),
        }
    }

    /// Moves packets in both directions between netsim and `channel`,
    /// mirroring `RootcanalTransport::pump`: drains every packet `channel`
    /// has queued for the controller and sends it as one masked binary WS
    /// frame, then reads whatever bytes are currently available from the
    /// socket, decodes any complete WS frames they form (replying to Pings
    /// automatically), and hands each complete binary message to `channel`
    /// as-is — it's already a complete H4 packet, no further framing needed.
    pub fn pump(&mut self, channel: &super::HciChannel) -> Result<(), SimbleError> {
        while let Some(packet) = channel.poll_host_packet() {
            self.trace_record(&packet, false);
            let frame = encode_masked_binary_frame(&packet);
            self.stream
                .write_all(&frame)
                .map_err(|e| SimbleError::Transport(e.to_string()))?;
        }

        let mut chunk = [0u8; 4096];
        loop {
            match self.stream.read(&mut chunk) {
                Ok(0) => {
                    return Err(SimbleError::Transport(
                        "netsim WebSocket connection closed".to_string(),
                    ));
                }
                Ok(n) => self.reader.feed(&chunk[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) => return Err(SimbleError::Transport(e.to_string())),
            }
        }

        while let Some(frame) = self.reader.next_frame() {
            if let Some(packet) = self.handle_frame(frame)? {
                self.trace_record(&packet, true);
                channel.receive_from_controller(packet)?;
            }
        }
        Ok(())
    }
}

// --- Scene over netsim ------------------------------------------------------

/// The WebSocket frontend of a netsimd started by Android Studio's emulator
/// (or by hand: `netsimd --logtostderr --no-shutdown --ws-port 7681`).
pub const DEFAULT_WS_URL: &str = "ws://127.0.0.1:7681";

/// A [`LiveScene`] whose backend is netsim: each peripheral is its own
/// WebSocket connection to netsimd, which routes advertising and data
/// between them, the Android emulator, and any other netsim clients. The
/// emulator (or another netsim device) plays the central.
pub struct NetsimScene {
    ws_url: String,
    scene: super::live_scene::LiveScene<NetsimTransport<TcpStream>>,
}

impl NetsimScene {
    /// Creates an empty netsim scene targeting `ws_url` (no connection is
    /// made until the first peripheral is added).
    pub fn new(ws_url: &str) -> Self {
        Self {
            ws_url: ws_url.trim_end_matches('/').to_string(),
            scene: super::live_scene::LiveScene::new(),
        }
    }

    /// Runs `script` and registers the resulting device with netsimd under
    /// its script name and `address` (netsim reads both from the connection
    /// URI's query string). A connection failure returns the "is netsimd
    /// running" hint from [`NetsimTransport::connect`].
    pub fn add_peripheral(
        &mut self,
        address: crate::types::Address,
        script: &str,
    ) -> Result<usize, String> {
        self.add_peripheral_named(address, script, None)
    }

    /// As [`Self::add_peripheral`], but registers the device under
    /// `node_name` instead of the name its script gave the GATT server.
    ///
    /// The node name is *placement*, not identity: it is the label netsim
    /// lists the device under (and the only handle a human has on it in
    /// `netsim devices`), and a scene file names it so two devices built from
    /// the same catalog script are still tellable apart. It does not change
    /// what the device advertises, which still comes from the script.
    pub fn add_peripheral_named(
        &mut self,
        address: crate::types::Address,
        script: &str,
        node_name: Option<&str>,
    ) -> Result<usize, String> {
        let ws_url = self.ws_url.clone();
        let node_name = node_name.map(str::to_string);
        self.scene.add_peripheral(address, script, |peripheral| {
            // The name goes verbatim into the query string; keep it URL-safe
            // (spaces are the only realistic offender in a device name).
            let name = node_name
                .unwrap_or_else(|| peripheral.device_name())
                .replace(' ', "%20");
            let addr_lsb = address.to_netsim_wire_string();
            let url = format!("{ws_url}/v1/websocket/bt?name={name}&address={addr_lsb}");
            let mut transport = NetsimTransport::connect(&url).map_err(|e| e.to_string())?;
            // SIMBLE_BTSNOOP=<dir> captures this device's HCI traffic to
            // <dir>/<name>.btsnoop (Wireshark-readable) for debugging
            // exchanges against real stacks.
            if let Ok(dir) = std::env::var("SIMBLE_BTSNOOP") {
                let path = std::path::Path::new(&dir).join(format!("{name}.btsnoop"));
                match std::fs::File::create(&path) {
                    Ok(file) => {
                        let _ = transport.set_trace(file);
                    }
                    Err(e) => eprintln!("SIMBLE_BTSNOOP: cannot create {path:?}: {e}"),
                }
            }
            Ok(transport)
        })
    }

    /// See [`LiveScene::pump`].
    pub fn pump(&mut self) {
        self.scene.pump();
    }

    /// See [`LiveScene::tick`].
    pub fn tick(&mut self, seconds: f64) {
        self.scene.tick(seconds);
    }

    /// The current script-clock time in seconds.
    pub fn now(&self) -> f64 {
        self.scene.now()
    }

    /// The number of peripherals in the scene.
    pub fn device_count(&self) -> usize {
        self.scene.device_count()
    }

    /// The GATT status JSON of peripheral `index`, or `None` for an unknown
    /// index.
    pub fn peripheral_status_json(&self, index: usize) -> Option<String> {
        self.scene.peripheral_status_json(index)
    }
}

/// The shared [`Scene`](super::Scene) shape, forwarding to the inherent methods
/// above. Netsim scanning is not wired (a scan needs a role on netsim's ether),
/// so `add_scanner` says so and the scanner defaults (`false`/`None`) hold.
impl super::Scene for NetsimScene {
    fn name(&self) -> &'static str {
        "netsim"
    }
    fn add_peripheral(
        &mut self,
        address: crate::types::Address,
        script: &str,
    ) -> Result<usize, String> {
        NetsimScene::add_peripheral(self, address, script)
    }
    fn pump(&mut self) {
        self.pump()
    }
    fn tick(&mut self, millis: f64) -> Option<f64> {
        // The trait speaks milliseconds; the inherent clock is seconds.
        NetsimScene::tick(self, millis / 1000.0);
        self.next_timeout_ms()
    }
    fn now_ms(&self) -> f64 {
        NetsimScene::now(self) * 1000.0
    }
    fn device_count(&self) -> usize {
        NetsimScene::device_count(self)
    }
    fn peripheral_status_json(&self, index: usize) -> Option<String> {
        NetsimScene::peripheral_status_json(self, index)
    }
    fn add_scanner(&mut self) -> Result<(), String> {
        Err("a real-RF scan needs run_on(\"usb\") — netsim scanning is not wired yet".to_string())
    }
}

#[cfg(test)]
#[path = "netsim_tests.rs"]
mod tests;
