// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! A minimal Model Context Protocol server (`simble mcp`), exposing SimBLE to
//! agents as tools — over stdio, or over WebSocket with `--ws-server [PORT]`.
//!
//! MCP is **JSON-RPC 2.0**, newline-delimited over stdio — not gRPC — so this
//! needs only `serde_json` (already a dependency) and `std::io`; no tonic, no
//! protobuf. Unlike the one-shot CLI, an MCP server registers once and stays
//! alive, so it holds a **live scene** across tool calls.
//!
//! This is SimBLE's **agent-first surface**, alongside the web (wasm) and
//! native (library + CLI) ones. An agent needs no checkout and no build step:
//! `example` hands it a working device script, `lookup` answers the assigned-
//! number questions, and the rest build and interrogate a live scene.
//!
//! Tools:
//! - `example` / `lookup` — serve a ready-to-run device script; search the
//!   vendored SIG assigned numbers by name or UUID. Both exist so an agent
//!   never has to guess at the API or at a UUID.
//! - `lint` / `run_test` — stateless; compile or run a script (same functions
//!   the CLI and browser Testing page use, so the surfaces can't diverge).
//! - `run_on` — choose which controller the scene runs on: `self` (in-process,
//!   deterministic), `netsim` (the emulator's ether), or `usb` (a real dongle,
//!   optionally chosen by `vid:pid`).
//! - `add_peripheral` / `tick` / `status` / `scan` — build and drive the live
//!   scene; `status` is the god-view, `scan` is what a scanner actually hears.
//! - `connect` / `read` / `write` / `assert` — drive a central against a
//!   peripheral, naming characteristics by UUID.
//! - `subscribe` / `assert_over` — a real monitor: a condition that must hold
//!   across a window, failing on the first violating sample. `subscribe` with
//!   a condition arms a *watch* that pushes an unsolicited
//!   `notifications/message` the moment the condition breaks, instead of
//!   waiting to be polled.
//!
//! A *scene* is the set of devices the agent has added; the controller is where
//! they run. `run_on` re-targets the controller; the devices are the agent's,
//! hosted by this process (peers on netsim / in a browser are not).

use crate::devices::catalog::{self, EXAMPLES};
use crate::gatt::sig_names;
use crate::transport::netsim::{self, NetsimScene};
use crate::transport::usb::{UsbScene, parse_vid_pid};
use crate::transport::wasm_ws::{SceneEngine, lint_script, run_test_script};
use crate::types::Address;
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::sync::mpsc;
use std::time::Duration;

/// The MCP revision this server implements (returned from `initialize`).
const PROTOCOL_VERSION: &str = "2024-11-05";

/// How long an otherwise-quiet actor loop idles between passes — small enough
/// to stay responsive to a request, large enough not to spin a core. There is
/// no async runtime (see `HANDOFF.md`), so both transports poll.
const IDLE_INTERVAL: Duration = Duration::from_millis(5);

/// A scene whose controller is on the far side of a real transport. Every one
/// of these is **peripheral-only** — the central is the Android emulator, a
/// phone, a laptop — and they are mutually exclusive with each other and with
/// the in-process `self` scene, because `run_on` resets the server.
enum LiveBackend {
    Netsim(NetsimScene),
    Usb(UsbScene),
}

impl LiveBackend {
    /// What `status` calls this controller.
    fn name(&self) -> &'static str {
        match self {
            LiveBackend::Netsim(_) => "netsim",
            LiveBackend::Usb(_) => "usb",
        }
    }

    fn add_peripheral(&mut self, address: Address, script: &str) -> Result<usize, String> {
        match self {
            LiveBackend::Netsim(scene) => scene.add_peripheral(address, script),
            LiveBackend::Usb(scene) => scene.add_peripheral(address, script),
        }
    }

    fn pump(&mut self) {
        match self {
            LiveBackend::Netsim(scene) => scene.pump(),
            LiveBackend::Usb(scene) => scene.pump(),
        }
    }

    fn tick(&mut self, seconds: f64) {
        match self {
            LiveBackend::Netsim(scene) => scene.tick(seconds),
            LiveBackend::Usb(scene) => scene.tick(seconds),
        }
    }

    fn now(&self) -> f64 {
        match self {
            LiveBackend::Netsim(scene) => scene.now(),
            LiveBackend::Usb(scene) => scene.now(),
        }
    }

    fn device_count(&self) -> usize {
        match self {
            LiveBackend::Netsim(scene) => scene.device_count(),
            LiveBackend::Usb(scene) => scene.device_count(),
        }
    }

    fn peripheral_status_json(&self, index: usize) -> Option<String> {
        match self {
            LiveBackend::Netsim(scene) => scene.peripheral_status_json(index),
            LiveBackend::Usb(scene) => scene.peripheral_status_json(index),
        }
    }
}

/// An armed watch: the condition a `subscribe` call asked to be told about.
/// It is the *safety* condition, so the interesting event is it **breaking** —
/// "HR exceeded 200" is `op: "<", threshold: 200` no longer holding.
struct Monitor {
    uuid: String,
    handle: u16,
    op: String,
    threshold: i64,
    byte: usize,
    /// Set once the condition has broken, so a sustained violation announces
    /// itself once rather than every tick.
    fired: bool,
}

/// The live server: a scene on the `self` controller, plus the deterministic
/// address allocator and simulated clock it advances.
pub struct Server {
    scene: Option<SceneEngine>,
    /// The scene when `run_on` chose a real backend instead of `self`.
    live: Option<LiveBackend>,
    next_addr: u16,
    elapsed: f64,
    /// Lazily-added scanner device index, reused across `scan` calls.
    scanner: Option<usize>,
    /// Added peripherals as (device index, address) — `connect` targets one.
    peripherals: Vec<(usize, Address)>,
    /// The most recently connected central device index, driven by `read`.
    central: Option<usize>,
    /// Conditions armed by `subscribe`, checked after every clock advance.
    monitors: Vec<Monitor>,
    /// Server→client messages waiting to go out, drained by whichever loop is
    /// serving this server (see [`take_notifications`](Self::take_notifications)).
    notifications: Vec<Value>,
}

impl Default for Server {
    fn default() -> Self {
        Self {
            scene: None,
            live: None,
            next_addr: 1,
            elapsed: 0.0,
            scanner: None,
            peripherals: Vec::new(),
            central: None,
            monitors: Vec::new(),
            notifications: Vec::new(),
        }
    }
}

/// Runs the server on stdio: one JSON-RPC message per line in, one response
/// line per request out, until stdin reaches EOF. Notifications (no `id`) get
/// no reply, and the server pushes its own between responses.
pub fn serve_stdio() -> std::io::Result<()> {
    let stdout = std::io::stdout();
    // `BufReader<Stdin>` rather than `StdinLock`: the reader half is moved to
    // its own thread and `StdinLock` is not `Send`.
    serve_lines(
        Server::default(),
        std::io::BufReader::new(std::io::stdin()),
        stdout.lock(),
    )
}

/// The actor loop, over any line source and message sink.
///
/// A tiny reader thread ferries *lines* over a channel — it never touches the
/// scene, so the (non-`Send`) scripting engine stays on this thread. The main
/// loop then polls the channel **without ever blocking on input**, which is
/// what leaves room to pump live backends and push notifications between
/// requests. A regression that reinstates a blocking read presents as "the MCP
/// server hangs" with every request still answered eventually, so
/// `test_actor_loop_pushes_notifications_while_input_is_idle` pins it.
fn serve_lines<R, W>(mut server: Server, reader: R, mut out: W) -> std::io::Result<()>
where
    R: BufRead + Send + 'static,
    W: Write,
{
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut reader = reader;
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break, // EOF: input closed
                Ok(_) => {
                    if tx.send(std::mem::take(&mut line)).is_err() {
                        break; // main loop gone
                    }
                }
                Err(_) => break,
            }
        }
    });

    loop {
        // Drain every request that has arrived, without blocking.
        let mut idle = true;
        let mut done = false;
        loop {
            match rx.try_recv() {
                Ok(line) => {
                    idle = false;
                    if let Some(response) = server.handle_line(&line) {
                        write_message(&mut out, &response)?;
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                // Input closed (the queue is already drained — `try_recv`
                // reports Disconnected only once it is empty). Finish this
                // pass so the last responses' notifications still go out,
                // rather than returning mid-pass and swallowing them.
                Err(mpsc::TryRecvError::Disconnected) => {
                    done = true;
                    break;
                }
            }
        }

        // Pump live backends between requests, so netsim/usb peripherals
        // answer their centrals' connections and reads while no tool call is
        // active, then flush anything the server wants to say unprompted.
        server.pump_live();
        for notification in server.take_notifications() {
            write_message(&mut out, &notification)?;
        }
        if done {
            return Ok(());
        }
        if idle {
            std::thread::sleep(IDLE_INTERVAL);
        }
    }
}

/// Serves MCP over WebSocket instead of stdio (`simble mcp --ws-server PORT`),
/// one client at a time — the same actor loop, with RFC 6455 text messages in
/// place of newline-delimited lines.
///
/// The WebSocket half is [`WsServerConn`](crate::transport::WsServerConn), the
/// same hand-rolled codec the `--usb` bridge serves from; only the payload
/// differs (JSON-RPC text, not H4 packets). Each client gets a **fresh
/// scene**: a scene is the set of devices *that* agent added, and handing the
/// next connection the previous one's devices would be a surprise, not a
/// feature.
pub fn serve_ws(port: u16) -> std::io::Result<()> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", port))?;
    eprintln!(
        "simble mcp: serving MCP over ws://127.0.0.1:{port}/ (one client at a time; \
         stdio is not served in this mode)"
    );
    for incoming in listener.incoming() {
        match incoming {
            Ok(stream) => {
                if let Err(e) = serve_ws_client(stream) {
                    // A client disconnect surfaces as an error too; it is the
                    // clean end of a session, so log it and await the next.
                    eprintln!("simble mcp: session ended: {e}");
                }
            }
            Err(e) => eprintln!("simble mcp: accept failed: {e}"),
        }
    }
    Ok(())
}

/// Serves one accepted WebSocket client end-to-end: handshake, then the actor
/// loop until the peer closes.
fn serve_ws_client(stream: std::net::TcpStream) -> Result<(), crate::types::SimbleError> {
    let (mut conn, _query) = crate::transport::WsServerConn::accept(stream)?;
    let mut server = Server::default();
    loop {
        // `poll_messages` never blocks (the socket is non-blocking), so the
        // same "pump between requests" property holds here as on stdio.
        let messages = conn.poll_messages()?;
        let idle = messages.is_empty();
        for message in messages {
            let text = String::from_utf8_lossy(&message).into_owned();
            if let Some(response) = server.handle_line(&text) {
                conn.send_text(&response.to_string())?;
            }
        }

        server.pump_live();
        for notification in server.take_notifications() {
            conn.send_text(&notification.to_string())?;
        }
        if idle {
            std::thread::sleep(IDLE_INTERVAL);
        }
    }
}

/// Writes one JSON-RPC message as a single newline-delimited line — responses
/// and server-initiated notifications alike.
fn write_message<W: Write>(out: &mut W, message: &Value) -> std::io::Result<()> {
    serde_json::to_writer(&mut *out, message)?;
    out.write_all(b"\n")?;
    out.flush()
}

impl Server {
    /// Moves packets for any live backend (netsim, usb) without handling a
    /// request — the actor loop calls this between requests so peripherals
    /// stay responsive to their centrals.
    pub fn pump_live(&mut self) {
        if let Some(live) = self.live.as_mut() {
            live.pump();
        }
    }

    /// Drives the server programmatically (the non-stdio entry point): pass a
    /// JSON-RPC request `Value`, get its response, or `None` for a notification.
    /// Same dispatch the actor loop runs per message — useful for embedding and
    /// for scenario tests that exercise the tools without a pipe.
    pub fn request(&mut self, request: &Value) -> Option<Value> {
        self.handle(request)
    }

    /// Handles one raw inbound message (a stdio line, or one WebSocket text
    /// message): parse, dispatch, and return the response to send back, or
    /// `None` for a blank line or a JSON-RPC notification.
    fn handle_line(&mut self, line: &str) -> Option<Value> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return None;
        }
        match serde_json::from_str::<Value>(trimmed) {
            Ok(request) => self.handle(&request),
            Err(e) => Some(error_response(None, -32700, &format!("parse error: {e}"))),
        }
    }

    /// Takes the server→client messages queued since the last call. These are
    /// **unsolicited**: the transport writes them out whenever it next gets
    /// the chance, without any request having asked for them.
    pub fn take_notifications(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.notifications)
    }

    /// Queues one MCP `notifications/message` (the spec's logging
    /// notification). The wire-visible difference from a response is that a
    /// notification carries **no `id`** — nothing is replying to it.
    fn push_notification(&mut self, level: &str, logger: &str, data: Value) {
        self.notifications.push(json!({
            "jsonrpc": "2.0",
            "method": "notifications/message",
            "params": { "level": level, "logger": logger, "data": data },
        }));
    }

    /// Dispatches one JSON-RPC request. `Some(response)` for requests, `None`
    /// for notifications (a message with no `id` is never answered).
    fn handle(&mut self, request: &Value) -> Option<Value> {
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        let id = request.get("id").cloned();
        id.as_ref()?; // notification: no id, no response

        Some(match method {
            "initialize" => result_response(id, initialize_result()),
            "tools/list" => result_response(id, tools_list()),
            "tools/call" => self.tools_call(id, request.get("params")),
            "ping" => result_response(id, json!({})),
            other => error_response(id, -32601, &format!("method not found: {other}")),
        })
    }

    fn tools_call(&mut self, id: Option<Value>, params: Option<&Value>) -> Value {
        let Some(params) = params else {
            return error_response(id, -32602, "missing params");
        };
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        let args = params.get("arguments");

        // Every live backend is peripheral-only: something on the far side —
        // the Android emulator, a phone in the room — plays the central, so
        // simble's central-side tools have nothing in-scene to run on.
        if let Some(live) = self.live.as_ref()
            && matches!(
                name,
                "scan"
                    | "connect"
                    | "read"
                    | "write"
                    | "assert"
                    | "subscribe"
                    | "assert_over"
                    | "add_central"
            )
        {
            let far_side = match live {
                LiveBackend::Netsim(_) => "the Android emulator (or another netsim client)",
                LiveBackend::Usb(_) => "a real phone or laptop over real RF",
            };
            return tool_text(
                id,
                &format!(
                    "{name} is self-mode only: on {} {far_side} plays the central — \
                     scan/connect from there, and use status here to watch the \
                     peripheral side",
                    live.name()
                ),
                true,
            );
        }

        match name {
            "lint" => match require_script(args) {
                Ok(s) => match lint_script(s) {
                    Ok(()) => tool_text(id, "OK — compiles cleanly", false),
                    Err(e) => tool_text(id, &format!("lint error: {e}"), true),
                },
                Err(msg) => tool_text(id, msg, true),
            },
            "run_test" => match require_script(args) {
                Ok(s) => match run_test_script(s) {
                    Ok(()) => tool_text(id, "PASS — all assertions held", false),
                    Err(e) => tool_text(id, &format!("FAIL — {e}"), true),
                },
                Err(msg) => tool_text(id, msg, true),
            },
            "run_on" => {
                let target = args
                    .and_then(|a| a.get("target"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let device = args.and_then(|a| a.get("device")).and_then(Value::as_str);
                self.tool_run_on(id, target, device)
            }
            "add_peripheral" => match require_script(args) {
                Ok(s) => self.tool_add_peripheral(id, s),
                Err(msg) => tool_text(id, msg, true),
            },
            "add_central" => match require_script(args) {
                Ok(s) => {
                    let to = args
                        .and_then(|a| a.get("to"))
                        .and_then(Value::as_u64)
                        .map(|n| n as usize);
                    self.tool_add_central(id, s, to)
                }
                Err(msg) => tool_text(id, msg, true),
            },
            "tick" => {
                let seconds = args
                    .and_then(|a| a.get("seconds"))
                    .and_then(Value::as_f64)
                    .unwrap_or(0.1);
                self.tool_tick(id, seconds)
            }
            "status" => self.tool_status(id),
            "scan" => self.tool_scan(id),
            "connect" => {
                let to = args
                    .and_then(|a| a.get("to"))
                    .and_then(Value::as_u64)
                    .map(|n| n as usize);
                self.tool_connect(id, to)
            }
            "read" => match args.and_then(|a| a.get("uuid")).and_then(Value::as_str) {
                Some(uuid) => self.tool_read(id, uuid),
                None => tool_text(id, "read needs a uuid argument", true),
            },
            "write" => self.tool_write(id, args),
            "assert" => self.tool_assert(id, args),
            "subscribe" => match args.and_then(|a| a.get("uuid")).and_then(Value::as_str) {
                Some(uuid) => self.tool_subscribe(id, uuid, args),
                None => tool_text(id, "subscribe needs a uuid argument", true),
            },
            "assert_over" => self.tool_assert_over(id, args),
            "example" => {
                let name = args.and_then(|a| a.get("name")).and_then(Value::as_str);
                tool_example(id, name)
            }
            "lookup" => {
                let query = args.and_then(|a| a.get("query")).and_then(Value::as_str);
                match query {
                    Some(q) if !q.trim().is_empty() => tool_lookup(id, q),
                    _ => tool_text(
                        id,
                        "lookup needs a query (a name fragment or a 16-bit UUID)",
                        true,
                    ),
                }
            }
            other => error_response(id, -32602, &format!("unknown tool: {other}")),
        }
    }

    fn tool_run_on(&mut self, id: Option<Value>, target: &str, device: Option<&str>) -> Value {
        match target {
            "self" => {
                *self = Server {
                    scene: Some(SceneEngine::new()),
                    ..Server::default()
                };
                tool_text(
                    id,
                    "scene now runs on: self (in-process, deterministic)",
                    false,
                )
            }
            "netsim" => {
                *self = Server {
                    live: Some(LiveBackend::Netsim(NetsimScene::new(
                        netsim::DEFAULT_WS_URL,
                    ))),
                    ..Server::default()
                };
                tool_text(
                    id,
                    &format!(
                        "scene now runs on: netsim ({} — shared with the Android emulator). \
                         Peripherals you add join netsim's ether as real devices; scan and \
                         connect to them FROM the emulator (simble-side scan/connect/read/\
                         write/assert are self-mode only). If adding a peripheral fails, \
                         netsimd may not be running with its WebSocket frontend.",
                        netsim::DEFAULT_WS_URL
                    ),
                    false,
                )
            }
            // A `vid:pid` typo must be caught here, where the agent can see
            // which argument it got wrong — not several calls later as a
            // "dongle not found". Opening is still deferred to the first
            // add_peripheral, exactly as netsim defers its connection.
            "usb" => {
                let selected = match device {
                    Some(spec) => match parse_vid_pid(spec) {
                        Ok(pair) => Some(pair),
                        // Reworded rather than passed through: the transport's
                        // "Transport Error:" prefix is noise to an agent that
                        // asked about an argument.
                        Err(_) => {
                            return tool_text(
                                id,
                                &format!(
                                    "invalid device selector {spec:?} — expected hex \
                                     vid:pid, e.g. \"0a12:0001\" (lsusb / system_profiler \
                                     SPUSBDataType lists them)"
                                ),
                                true,
                            );
                        }
                    },
                    None => None,
                };
                let scene = UsbScene::new(selected);
                let selector = scene.selector();
                *self = Server {
                    live: Some(LiveBackend::Usb(scene)),
                    ..Server::default()
                };
                tool_text(
                    id,
                    &format!(
                        "scene now runs on: usb ({selector}). The dongle is opened when you \
                         add a peripheral, and a dongle is one controller — so this scene \
                         holds ONE device, advertising on real RF for real phones to find. \
                         Scan and connect from the phone (simble-side scan/connect/read/\
                         write/assert are self-mode only); use status here to watch the \
                         peripheral side. Pass device:\"vid:pid\" to pick a specific dongle."
                    ),
                    false,
                )
            }
            "" => tool_text(
                id,
                "missing required argument: target (self|netsim|usb)",
                true,
            ),
            other => tool_text(
                id,
                &format!("unknown target {other:?} (expected self|netsim|usb)"),
                true,
            ),
        }
    }

    fn tool_add_peripheral(&mut self, id: Option<Value>, script: &str) -> Value {
        let address = self.alloc_address();
        if let Some(live) = self.live.as_mut() {
            let backend = live.name();
            return match live.add_peripheral(address, script) {
                Ok(index) => {
                    let status = live
                        .peripheral_status_json(index)
                        .unwrap_or_else(|| "{}".to_string());
                    // First pump queues HCI bring-up so the device goes on the
                    // air immediately, not at the next tool call.
                    live.pump();
                    tool_text(
                        id,
                        &format!(
                            "added peripheral #{index} to {backend} as {address} — scan for \
                             it from the other side\n{status}"
                        ),
                        false,
                    )
                }
                Err(e) => tool_text(id, &format!("device rejected: {e}"), true),
            };
        }
        if self.scene.is_none() {
            self.scene = Some(SceneEngine::new());
        }
        // Take the result before re-borrowing self, so the push doesn't clash.
        let result = self.scene.as_mut().unwrap().add_peripheral(address, script);
        match result {
            Ok(index) => {
                self.peripherals.push((index, address));
                let status = self
                    .scene
                    .as_ref()
                    .unwrap()
                    .peripheral_status_json(index)
                    .unwrap_or_else(|| "{}".to_string());
                tool_text(
                    id,
                    &format!("added peripheral #{index} (call tick, then status)\n{status}"),
                    false,
                )
            }
            Err(e) => tool_text(id, &format!("device rejected: {e}"), true),
        }
    }

    /// Adds a scripted GATT client to the scene, points it at a peripheral,
    /// and lets discovery complete — so what comes back already says whether
    /// the script's `assert`s held.
    ///
    /// The counterpart of `add_peripheral`: without it an agent can build a
    /// device but not the thing that drives it, and every interaction has to
    /// be spelled out one `read`/`write` tool call at a time.
    fn tool_add_central(&mut self, id: Option<Value>, script: &str, to: Option<usize>) -> Value {
        let address = self.alloc_address();
        if self.scene.is_none() {
            self.scene = Some(SceneEngine::new());
        }
        let scene = self.scene.as_mut().unwrap();
        let index = match scene.add_scripted_central(address, script) {
            Ok(index) => index,
            Err(e) => return tool_text(id, &format!("client rejected: {e}"), true),
        };

        // The script named an address; the scene allocated the real ones. If
        // the two disagree the script could never connect, so re-point it —
        // and say so, rather than silently changing what the script asked for.
        let requested = scene
            .scripted_central(index)
            .map(|c| c.client().with_central(|inner| inner.target()));
        let explicit = to.and_then(|i| {
            self.peripherals
                .iter()
                .find(|(idx, _)| *idx == i)
                .map(|(_, a)| *a)
        });
        let known = requested.is_some_and(|r| self.peripherals.iter().any(|(_, a)| *a == r));
        let retarget = explicit.or_else(|| {
            if known {
                None
            } else {
                self.peripherals.first().map(|(_, a)| *a)
            }
        });
        let mut note = String::new();
        if let Some(target) = retarget
            && let Some(central) = self.scene.as_mut().unwrap().scripted_central_mut(index)
        {
            central.set_target(target);
            note = format!(
                " (pointed at {target}; the script asked for {})",
                requested.map(|r| r.to_string()).unwrap_or_default()
            );
        }
        if to.is_some() && explicit.is_none() {
            return tool_text(id, "no peripheral with that index — call status", true);
        }

        // connect + MTU + service and characteristic discovery, plus room for
        // whatever the script queued from on_services_discovered.
        self.advance(40, 0.02);
        let scene = self.scene.as_mut().unwrap();
        let Some(central) = scene.scripted_central_mut(index) else {
            return tool_text(id, "central vanished from the scene", true);
        };
        let failure = central.failure().map(str::to_string);
        let emitted = central.take_emitted();
        let status = central.status_json();
        let mut body = format!("added central #{index}{note}\n{}", annotate_json(&status));
        if !emitted.is_empty() {
            body.push_str(&format!("\nemitted: {}", emitted.join(", ")));
        }
        match failure {
            Some(failure) => {
                body.push_str(&format!("\nFAIL — {failure}"));
                tool_text(id, &body, true)
            }
            None => tool_text(id, &body, false),
        }
    }

    /// Connects a central to a peripheral (by index, or the first one) and lets
    /// discovery complete, so a following `read` can name characteristics by
    /// UUID. Returns the central's discovered GATT as JSON.
    fn tool_connect(&mut self, id: Option<Value>, to: Option<usize>) -> Value {
        let target = match to {
            Some(i) => self
                .peripherals
                .iter()
                .find(|(idx, _)| *idx == i)
                .map(|(_, a)| *a),
            None => self.peripherals.first().map(|(_, a)| *a),
        };
        let Some(target) = target else {
            return tool_text(
                id,
                "no peripheral in the scene — add_peripheral first",
                true,
            );
        };
        let central_addr = self.alloc_address();
        let scene = self.scene.get_or_insert_with(SceneEngine::new);
        let index = scene.add_central(central_addr, target);
        self.central = Some(index);
        self.advance(30, 0.02); // connect + MTU + service/characteristic discovery
        let status = self
            .scene
            .as_ref()
            .unwrap()
            .central_status_json(index)
            .unwrap_or_default();
        tool_text(
            id,
            &format!("connected central #{index}\n{}", annotate_json(&status)),
            false,
        )
    }

    /// Reads a characteristic (matched by UUID against the connected central's
    /// discovered database) and returns the central's updated state, including
    /// the value just read.
    fn tool_read(&mut self, id: Option<Value>, uuid: &str) -> Value {
        let Some(central) = self.central else {
            return tool_text(id, "not connected — call connect first", true);
        };
        let status = self
            .scene
            .as_ref()
            .and_then(|s| s.central_status_json(central))
            .unwrap_or_default();
        let Some(handle) = handle_for_uuid(&status, uuid) else {
            return tool_text(
                id,
                &format!("no discovered characteristic matching {uuid:?}\n{status}"),
                true,
            );
        };
        self.scene.as_mut().unwrap().central_read(central, handle);
        self.advance(10, 0.02);
        let after = self
            .scene
            .as_ref()
            .unwrap()
            .central_status_json(central)
            .unwrap_or_default();
        tool_text(
            id,
            &format!("read {uuid} (handle {handle}):\n{after}"),
            false,
        )
    }

    /// A real central write: the bytes travel the ATT path into the
    /// peripheral's live GATT database, where the device's script sees them
    /// (`server.value(uuid)`, or a "characteristic_write" event from
    /// `take_events`). This is the agent's knob for settable devices —
    /// setpoints, control points, alert levels.
    fn tool_write(&mut self, id: Option<Value>, args: Option<&Value>) -> Value {
        let uuid = args.and_then(|a| a.get("uuid")).and_then(Value::as_str);
        let bytes: Option<Vec<u8>> = args
            .and_then(|a| a.get("value"))
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|v| v.as_u64().and_then(|n| u8::try_from(n).ok()))
                    .collect::<Option<Vec<u8>>>()
            })
            .unwrap_or(None);
        let (Some(uuid), Some(bytes)) = (uuid, bytes) else {
            return tool_text(id, "write needs: uuid, value (array of bytes 0-255)", true);
        };
        let Some(central) = self.central else {
            return tool_text(id, "not connected — call connect first", true);
        };
        let status = self
            .scene
            .as_ref()
            .and_then(|s| s.central_status_json(central))
            .unwrap_or_default();
        let Some(handle) = handle_for_uuid(&status, uuid) else {
            return tool_text(
                id,
                &format!("no discovered characteristic matching {uuid:?}\n{status}"),
                true,
            );
        };
        self.scene
            .as_mut()
            .unwrap()
            .central_write(central, handle, bytes.clone());
        self.advance(10, 0.02);
        let after = self
            .scene
            .as_ref()
            .unwrap()
            .central_status_json(central)
            .unwrap_or_default();
        tool_text(
            id,
            &format!("wrote {bytes:?} to {uuid} (handle {handle}):\n{after}"),
            false,
        )
    }

    /// A behavioural assertion: read a characteristic, take one byte of its
    /// value (default byte 1 — e.g. the 8-bit heart rate in a HR Measurement),
    /// and check it against a threshold. PASS/FAIL as `isError`, so an agent's
    /// "create a test that monitors HR < 200" is one machine-checked call.
    fn tool_assert(&mut self, id: Option<Value>, args: Option<&Value>) -> Value {
        let uuid = args.and_then(|a| a.get("uuid")).and_then(Value::as_str);
        let op = args.and_then(|a| a.get("op")).and_then(Value::as_str);
        let threshold = args.and_then(|a| a.get("value")).and_then(Value::as_i64);
        let byte = args
            .and_then(|a| a.get("byte"))
            .and_then(Value::as_u64)
            .unwrap_or(1) as usize;
        let (Some(uuid), Some(op), Some(threshold)) = (uuid, op, threshold) else {
            return tool_text(id, "assert needs: uuid, op (< > <= >= == !=), value", true);
        };
        let Some(central) = self.central else {
            return tool_text(id, "not connected — call connect first", true);
        };
        let status = self
            .scene
            .as_ref()
            .and_then(|s| s.central_status_json(central))
            .unwrap_or_default();
        let Some(handle) = handle_for_uuid(&status, uuid) else {
            return tool_text(id, &format!("no characteristic matching {uuid:?}"), true);
        };
        self.scene.as_mut().unwrap().central_read(central, handle);
        self.advance(10, 0.02);
        let after = self
            .scene
            .as_ref()
            .unwrap()
            .central_status_json(central)
            .unwrap_or_default();
        let Some(actual) = value_byte(&after, handle, byte) else {
            return tool_text(
                id,
                &format!("characteristic {uuid} has no byte {byte} yet\n{after}"),
                true,
            );
        };
        let Some(held) = compare(actual, op, threshold) else {
            return tool_text(id, &format!("unknown op {op:?}"), true);
        };
        let verb = if held { "PASS" } else { "FAIL" };
        tool_text(
            id,
            &format!("{verb} — {uuid} byte {byte} = {actual}, expected {op} {threshold}"),
            !held,
        )
    }

    /// Subscribe the connected central to a characteristic's notifications, so
    /// the peripheral's value changes (from its `fn tick`) push to the central.
    ///
    /// With `op` and `value`, it also arms a **watch**: from then on every
    /// clock advance checks the notified value, and the first time the
    /// condition breaks the server pushes an unsolicited
    /// `notifications/message` — "HR exceeded 200" the moment it happens,
    /// rather than at the next poll. `assert_over` is the same predicate asked
    /// synchronously over a fixed window; this is the asynchronous form.
    fn tool_subscribe(&mut self, id: Option<Value>, uuid: &str, args: Option<&Value>) -> Value {
        let Some(central) = self.central else {
            return tool_text(id, "not connected — call connect first", true);
        };
        let status = self
            .scene
            .as_ref()
            .and_then(|s| s.central_status_json(central))
            .unwrap_or_default();
        let Some(handle) = handle_for_uuid(&status, uuid) else {
            return tool_text(id, &format!("no characteristic matching {uuid:?}"), true);
        };

        let op = args.and_then(|a| a.get("op")).and_then(Value::as_str);
        let threshold = args.and_then(|a| a.get("value")).and_then(Value::as_i64);
        let byte = args
            .and_then(|a| a.get("byte"))
            .and_then(Value::as_u64)
            .unwrap_or(1) as usize;
        let watch = match (op, threshold) {
            (Some(op), Some(threshold)) => {
                if compare(0, op, threshold).is_none() {
                    return tool_text(id, &format!("unknown op {op:?}"), true);
                }
                Some((op.to_string(), threshold))
            }
            // Half a condition is a mistake, not a plain subscribe: silently
            // ignoring it is how an agent concludes watches do not work.
            (Some(_), None) | (None, Some(_)) => {
                return tool_text(
                    id,
                    "subscribe takes op AND value together to arm a watch (or neither, to \
                     just enable notifications)",
                    true,
                );
            }
            (None, None) => None,
        };

        self.scene
            .as_mut()
            .unwrap()
            .central_subscribe(central, handle);
        self.advance(8, 0.02); // CCCD write + first notifications
        let armed = match watch {
            Some((op, threshold)) => {
                // Re-arming the same characteristic replaces its watch rather
                // than stacking a second one on it.
                self.monitors.retain(|m| m.handle != handle);
                self.monitors.push(Monitor {
                    uuid: uuid.to_string(),
                    handle,
                    op: op.clone(),
                    threshold,
                    byte,
                    fired: false,
                });
                // Check immediately: a value already out of range is news now,
                // not at the next tick.
                self.poll_monitors();
                format!(
                    "\nwatching: a notifications/message goes out the first time byte \
                     {byte} stops holding {op} {threshold}"
                )
            }
            None => String::new(),
        };
        let after = self
            .scene
            .as_ref()
            .unwrap()
            .central_status_json(central)
            .unwrap_or_default();
        tool_text(
            id,
            &format!("subscribed to {uuid} (handle {handle}){armed}\n{after}"),
            false,
        )
    }

    /// Checks every armed watch against the central's latest notified values
    /// and queues a `notifications/message` for each condition that has just
    /// broken. Called after each clock advance — the only place values move.
    fn poll_monitors(&mut self) {
        if self.monitors.is_empty() {
            return;
        }
        let Some(central) = self.central else { return };
        let Some(status) = self
            .scene
            .as_ref()
            .and_then(|s| s.central_status_json(central))
        else {
            return;
        };

        // Decide first, then queue: `push_notification` needs `&mut self`,
        // which the monitors are borrowed out of.
        let mut broke = Vec::new();
        for monitor in &mut self.monitors {
            let Some(actual) = value_byte(&status, monitor.handle, monitor.byte) else {
                continue;
            };
            match compare(actual, &monitor.op, monitor.threshold) {
                // Holding again re-arms the watch, so a value that swings back
                // out of range is reported a second time.
                Some(true) => monitor.fired = false,
                Some(false) if !monitor.fired => {
                    monitor.fired = true;
                    broke.push((
                        monitor.uuid.clone(),
                        monitor.op.clone(),
                        monitor.threshold,
                        monitor.byte,
                        actual,
                    ));
                }
                _ => {}
            }
        }

        let t = self.elapsed;
        for (uuid, op, threshold, byte, actual) in broke {
            self.push_notification(
                "warning",
                "simble.monitor",
                json!({
                    "event": "condition_violated",
                    "uuid": uuid,
                    "byte": byte,
                    "value": actual,
                    "expected": format!("{op} {threshold}"),
                    "t": t,
                    "message": format!("{uuid} byte {byte} = {actual}, no longer {op} {threshold}"),
                }),
            );
        }
    }

    /// A *temporal* assertion — the honest "monitor": subscribe, run the clock
    /// for `seconds`, and require the condition to hold on **every** notified
    /// sample. FAILs on the first violation (with the offending value); PASSes
    /// if it never breaks. This is what makes "monitor HR < 200" literally true.
    fn tool_assert_over(&mut self, id: Option<Value>, args: Option<&Value>) -> Value {
        let uuid = args.and_then(|a| a.get("uuid")).and_then(Value::as_str);
        let op = args.and_then(|a| a.get("op")).and_then(Value::as_str);
        let threshold = args.and_then(|a| a.get("value")).and_then(Value::as_i64);
        let seconds = args
            .and_then(|a| a.get("seconds"))
            .and_then(Value::as_f64)
            .unwrap_or(2.0);
        let byte = args
            .and_then(|a| a.get("byte"))
            .and_then(Value::as_u64)
            .unwrap_or(1) as usize;
        let (Some(uuid), Some(op), Some(threshold)) = (uuid, op, threshold) else {
            return tool_text(
                id,
                "assert_over needs: uuid, op, value (+ optional seconds, byte)",
                true,
            );
        };
        let Some(central) = self.central else {
            return tool_text(id, "not connected — call connect first", true);
        };
        let status = self
            .scene
            .as_ref()
            .and_then(|s| s.central_status_json(central))
            .unwrap_or_default();
        let Some(handle) = handle_for_uuid(&status, uuid) else {
            return tool_text(id, &format!("no characteristic matching {uuid:?}"), true);
        };
        // Subscribe (so notify-capable peripherals push) and also poll each
        // sample with a read, so a monitor works whether the value is pushed
        // or steady.
        self.scene
            .as_mut()
            .unwrap()
            .central_subscribe(central, handle);
        self.advance(6, 0.02);

        let steps = ((seconds / 0.1).ceil() as usize).max(1);
        let mut samples = 0u32;
        let mut extreme: Option<i64> = None;
        for _ in 0..steps {
            self.scene.as_mut().unwrap().central_read(central, handle);
            self.advance(5, 0.02);
            let now = self
                .scene
                .as_ref()
                .unwrap()
                .central_status_json(central)
                .unwrap_or_default();
            if let Some(actual) = value_byte(&now, handle, byte) {
                samples += 1;
                extreme = Some(extreme.map_or(actual, |e| extreme_for(op, e, actual)));
                match compare(actual, op, threshold) {
                    Some(true) => {}
                    Some(false) => {
                        return tool_text(
                            id,
                            &format!(
                                "FAIL — {uuid} byte {byte} = {actual} violated {op} {threshold} while monitoring"
                            ),
                            true,
                        );
                    }
                    None => return tool_text(id, &format!("unknown op {op:?}"), true),
                }
            }
        }
        if samples == 0 {
            return tool_text(
                id,
                &format!("no samples for {uuid} — did discovery find a readable value?"),
                true,
            );
        }
        tool_text(
            id,
            &format!(
                "PASS — {uuid} byte {byte} held {op} {threshold} across {samples} samples over {seconds:.1}s (extreme {})",
                extreme.unwrap_or_default()
            ),
            false,
        )
    }

    /// Advance the scene `steps` times by `dt` seconds each (the polling loop
    /// connect/read need, since discovery and reads span several ticks), then
    /// check the armed watches against what those ticks notified.
    fn advance(&mut self, steps: usize, dt: f64) {
        for _ in 0..steps {
            self.elapsed += dt;
            let t = self.elapsed;
            if let Some(scene) = self.scene.as_mut() {
                scene.tick(t);
            }
            self.poll_monitors();
        }
    }

    fn tool_tick(&mut self, id: Option<Value>, seconds: f64) -> Value {
        if let Some(live) = self.live.as_mut() {
            live.tick(seconds);
            return tool_text(
                id,
                &format!(
                    "advanced to t={:.3}s ({} device(s) on {})",
                    live.now(),
                    live.device_count(),
                    live.name()
                ),
                false,
            );
        }
        self.elapsed += seconds;
        let t = self.elapsed;
        let Some(scene) = self.scene.as_mut() else {
            return tool_text(
                id,
                "no scene — call run_on(\"self\") or add_peripheral first",
                true,
            );
        };
        scene.tick(t);
        let count = scene.device_count();
        // A tick is where a watched value moves, so it is where a broken
        // condition becomes news.
        self.poll_monitors();
        tool_text(
            id,
            &format!("advanced to t={t:.3}s ({count} device(s))"),
            false,
        )
    }

    fn tool_status(&self, id: Option<Value>) -> Value {
        if let Some(live) = self.live.as_ref() {
            let devices: Vec<Value> = (0..live.device_count())
                .map(|i| match live.peripheral_status_json(i) {
                    Some(j) => serde_json::from_str(&j).unwrap_or(Value::String(j)),
                    None => json!({ "index": i, "role": "non-peripheral" }),
                })
                .collect();
            let mut body = json!({ "controller": live.name(), "devices": devices });
            annotate_uuid_names(&mut body);
            return tool_text(id, &serde_json::to_string_pretty(&body).unwrap(), false);
        }
        let Some(scene) = self.scene.as_ref() else {
            return tool_text(
                id,
                "no scene yet — call run_on(\"self\") or add_peripheral",
                true,
            );
        };
        let devices: Vec<Value> = (0..scene.device_count())
            .map(|i| {
                // A scripted central is a device of the scene like any other,
                // and its assertion state is the whole result of a client
                // script — reporting it as "non-peripheral" hid the answer.
                if let Some(central) = scene.scripted_central(i) {
                    let mut view: Value = serde_json::from_str(&central.status_json())
                        .unwrap_or_else(|_| json!({ "index": i }));
                    view["role"] = json!("central");
                    if let Some(failure) = central.failure() {
                        view["failure"] = json!(failure);
                    }
                    return view;
                }
                match scene.peripheral_status_json(i) {
                    Some(j) => serde_json::from_str(&j).unwrap_or(Value::String(j)),
                    None => json!({ "index": i, "role": "non-peripheral" }),
                }
            })
            .collect();
        let mut body = json!({ "controller": "self", "devices": devices });
        annotate_uuid_names(&mut body);
        tool_text(id, &serde_json::to_string_pretty(&body).unwrap(), false)
    }

    /// The BLE-radio view: stand up a scanner (once, reused) on the shared
    /// medium, let a few ticks pass so advertisements propagate, and return
    /// what it heard — the peripherals' adverts, exactly as a real central
    /// would see them. This is different from `status` (the scene's god-view
    /// of every device it hosts): `scan` only sees what's actually on the air.
    fn tool_scan(&mut self, id: Option<Value>) -> Value {
        if self.scene.is_none() {
            self.scene = Some(SceneEngine::new());
        }
        if self.scanner.is_none() {
            let address = self.alloc_address();
            let index = self.scene.as_mut().unwrap().add_scanner(address);
            self.scanner = Some(index);
        }
        let index = self.scanner.unwrap();
        for _ in 0..5 {
            self.elapsed += 0.05;
            let t = self.elapsed;
            self.scene.as_mut().unwrap().tick(t);
        }
        let reports = self.scene.as_mut().unwrap().scanner_reports_json(index);
        tool_text(id, &annotate_json(&dedupe_scan_reports(&reports)), false)
    }

    /// Deterministic per-scene address (base + counter) — identical inputs
    /// produce identical devices, which is what makes agent loops converge.
    fn alloc_address(&mut self) -> Address {
        let n = self.next_addr;
        self.next_addr = self.next_addr.wrapping_add(1);
        let [hi, lo] = n.to_be_bytes();
        Address::from_be_bytes([0xF0, 0xDE, 0xC0, 0x00, hi, lo])
    }
}

/// Collapses a drained scan backlog to one entry per advertiser — the latest
/// report, with a `reports` count of how many raw ones it stands for. Ticking
/// between scans (assert_over, a long tick) piles up duplicate adverts, and
/// returning them all can outgrow what an agent's tool-result window accepts.
fn dedupe_scan_reports(raw: &str) -> String {
    let Ok(Value::Array(all)) = serde_json::from_str(raw) else {
        return raw.to_string();
    };
    // Reports are chronological; keep first-heard order, latest content.
    let mut order: Vec<String> = Vec::new();
    let mut latest: std::collections::HashMap<String, (Value, u64)> =
        std::collections::HashMap::new();
    for report in all {
        let addr = report
            .get("address")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        match latest.get_mut(&addr) {
            Some((slot, count)) => {
                *slot = report;
                *count += 1;
            }
            None => {
                order.push(addr.clone());
                latest.insert(addr, (report, 1));
            }
        }
    }
    let deduped: Vec<Value> = order
        .into_iter()
        .map(|addr| {
            let (mut report, count) = latest.remove(&addr).unwrap();
            report["reports"] = json!(count);
            report
        })
        .collect();
    serde_json::to_string(&deduped).unwrap_or_else(|_| "[]".to_string())
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        // `logging` is what declares that this server sends unsolicited
        // `notifications/message` — an armed `subscribe` watch pushes one the
        // moment its condition breaks.
        "capabilities": { "tools": {}, "logging": {} },
        // The version is the crate version plus a git description, because the
        // registration recipe pins a client to target/release/simble and
        // nothing rebuilds it. A stale binary previously served invented RAS
        // UUIDs while its source was already fixed, and reported "0.1.0"
        // either way. Now the handshake says exactly which build answered.
        "serverInfo": {
            "name": "simble",
            "version": concat!(env!("CARGO_PKG_VERSION"), "+", env!("SIMBLE_BUILD")),
        },
    })
}

fn tools_list() -> Value {
    fn script_schema(desc: &str) -> Value {
        json!({
            "type": "object",
            "properties": { "script": { "type": "string", "description": desc } },
            "required": ["script"],
        })
    }
    json!({ "tools": [
        {
            "name": "lint",
            "description": "Compile a SimBLE Rhai script WITHOUT running it; reports a syntax/parse \
                error with position, or that it compiles cleanly. Side-effect-free pre-flight.",
            "inputSchema": script_schema("The Rhai script source."),
        },
        {
            "name": "run_test",
            "description": "Run a SimBLE Rhai script in a fresh deterministic engine (no radio) and \
                report whether every assert(...) held. A device is a script; add assert(cond, \
                \"msg\") and it is a test.",
            "inputSchema": script_schema("The Rhai script source."),
        },
        {
            "name": "run_on",
            "description": "Choose which controller the live scene runs on: \"self\" (in-process, \
                deterministic, no setup), \"netsim\" (peripherals join the Android emulator's \
                Bluetooth ether via a running netsimd — scan/connect from the emulator; needs \
                netsimd's WebSocket frontend, e.g. netsimd --ws-port 7681), or \"usb\" (a real \
                Bluetooth dongle, so a real phone in the room can find the device over real RF; \
                optionally pass device:\"vid:pid\" to pick one, else the first Bluetooth-class \
                dongle is used — a dongle is ONE controller, so a usb scene holds one device, \
                and it is opened when you add it). netsim and usb are peripheral-only: the far \
                side plays the central. Resets the scene.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": { "type": "string", "enum": ["self", "netsim", "usb"] },
                    "device": { "type": "string", "description": "usb only: dongle selector as hex \"vid:pid\", e.g. \"0a12:0001\"." },
                },
                "required": ["target"],
            },
        },
        {
            "name": "add_peripheral",
            "description": "Add a scripted peripheral to the live scene. The script must create an \
                android::BluetoothGattServer and build its GATT, e.g.: let s = \
                android::BluetoothGattServer(\"HRM\"); let svc = android::BluetoothGattService(\
                uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY); svc.add_characteristic(\
                android::BluetoothGattCharacteristic(uuid::HEART_RATE_MEASUREMENT, \
                android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ)); \
                s.add_service(svc); — an optional fn tick(server, t) runs every scene tick and \
                server.update_value(uuid, bytes) notifies subscribers (full samples: the example \
                tool). Returns the device index; call tick then status to run it. A bad script is \
                rejected with its error.",
            "inputSchema": script_schema("A Rhai peripheral script (creates a BluetoothGattServer)."),
        },
        {
            "name": "add_central",
            "description": "Add a scripted GATT *client* to the live scene — the counterpart of \
                add_peripheral, and the way to drive a device with behaviour rather than one tool \
                call at a time. The script must create an android::BluetoothGatt and connect it, \
                e.g.: let c = android::BluetoothGatt(\"Probe\"); c.connect(\"AA:BB:CC:00:00:01\"); \
                fn on_services_discovered(client) { client.subscribe(\
                uuid::HEART_RATE_MEASUREMENT); } fn on_characteristic_changed(client, uuid, value) \
                { assert(value[1] < 200, \"plausible\"); } — callbacks are on_connection_state_change\
                /on_services_discovered/on_characteristic_read/on_characteristic_write/\
                on_characteristic_changed/on_subscribed/on_mtu_changed/on_error, plus fn tick(\
                client, t) and a catch-all fn on_event(client, event). assert(...) inside a \
                callback fails the run, which is what makes a client script a test. Pass `to` to \
                point it at a peripheral by index (otherwise the scene's first peripheral is used \
                when the script's address matches none). Self-controller only. Client samples: \
                the example tool.",
            "inputSchema": json!({
                "type": "object",
                "properties": {
                    "script": { "type": "string", "description": "A Rhai central script (creates a BluetoothGatt)." },
                    "to": { "type": "integer", "description": "Peripheral device index to connect to." },
                },
                "required": ["script"],
            }),
        },
        {
            "name": "lookup",
            "description": "Search the Bluetooth SIG assigned numbers (vendored registry): a name \
                fragment (\"therm\", \"pulse ox\") lists matching services/characteristics/\
                descriptors with their 16-bit UUIDs; a hex UUID (\"1809\", \"0x2A1C\") resolves \
                to its SIG name. Answers \"what UUID is X\" and \"what is UUID Y\" without \
                leaving the session.",
            "inputSchema": {
                "type": "object",
                "properties": { "query": { "type": "string", "description": "Name fragment or 16-bit hex UUID." } },
                "required": ["query"],
            },
        },
        {
            "name": "example",
            "description": "Serve a named, ready-to-run sample peripheral script — the fastest way \
                to learn the Rhai scripting API without the simble repo. Call with no name to list \
                the samples; pass the script to add_peripheral as-is, or adapt it (lint checks \
                without running).",
            "inputSchema": {
                "type": "object",
                "properties": { "name": { "type": "string", "description": "Sample name (omit to list them)." } },
            },
        },
        {
            "name": "tick",
            "description": "Advance the live scene's simulated clock, letting peripherals run their \
                scripts and the shared radio route advertising and data.",
            "inputSchema": {
                "type": "object",
                "properties": { "seconds": { "type": "number", "description": "Delta to advance (default 0.1)." } },
            },
        },
        {
            "name": "status",
            "description": "The scene's god-view: report every device this server hosts as JSON \
                (GATT structure, values, connection state). Answers \"what devices are present\".",
            "inputSchema": { "type": "object", "properties": {} },
        },
        {
            "name": "scan",
            "description": "The BLE-radio view: run a scanner on the shared medium and return the \
                advertisements it hears — the peripherals actually on the air, as a real central \
                would see them. Answers \"scan for devices\" (a subset of status).",
            "inputSchema": { "type": "object", "properties": {} },
        },
        {
            "name": "connect",
            "description": "Connect a central to a peripheral (by index, or the first one) and run \
                discovery, so read/assert can name characteristics by UUID. Returns the discovered \
                GATT.",
            "inputSchema": {
                "type": "object",
                "properties": { "to": { "type": "integer", "description": "Peripheral index (default: first)." } },
            },
        },
        {
            "name": "read",
            "description": "Read a characteristic (matched by UUID) from the connected peripheral and \
                return its value. Call connect first.",
            "inputSchema": {
                "type": "object",
                "properties": { "uuid": { "type": "string", "description": "Characteristic UUID (16-bit or full)." } },
                "required": ["uuid"],
            },
        },
        {
            "name": "write",
            "description": "Write bytes to a characteristic (matched by UUID) on the connected \
                peripheral — a real central write: the value lands in the live GATT database, \
                where the device's script reads it back (server.value(uuid)) or reacts to the \
                characteristic_write event. The knob for settable devices (setpoints, control \
                points, alert levels). Call connect first.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "uuid": { "type": "string", "description": "Characteristic UUID (16-bit or full)." },
                    "value": { "type": "array", "items": { "type": "integer" }, "description": "Bytes to write (each 0-255)." },
                },
                "required": ["uuid", "value"],
            },
        },
        {
            "name": "assert",
            "description": "Behavioural test: read a characteristic and check one byte of its value \
                against a threshold. E.g. \"HR < 200\" is uuid 2A37, byte 1, op \"<\", value 200. \
                Returns PASS/FAIL. Call connect first.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "uuid": { "type": "string" },
                    "op": { "type": "string", "enum": ["<", ">", "<=", ">=", "==", "!="] },
                    "value": { "type": "integer" },
                    "byte": { "type": "integer", "description": "Byte index of the value (default 1)." },
                },
                "required": ["uuid", "op", "value"],
            },
        },
        {
            "name": "subscribe",
            "description": "Enable notifications on a characteristic (matched by UUID) for the \
                connected central, so the peripheral's fn tick value changes push to it. Pass \
                op + value as well to arm a WATCH: the server then pushes an unsolicited \
                notifications/message the first time the condition stops holding, without you \
                asking again — \"tell me if HR ever exceeds 200\" is uuid 2A37, op \"<\", value \
                200. That is the asynchronous form of assert_over (which blocks for a fixed \
                window instead). Call connect first.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "uuid": { "type": "string" },
                    "op": { "type": "string", "enum": ["<", ">", "<=", ">=", "==", "!="], "description": "With value: the condition to watch. Omit both for a plain subscribe." },
                    "value": { "type": "integer", "description": "With op: the threshold to watch against." },
                    "byte": { "type": "integer", "description": "Byte index (default 1)." },
                },
                "required": ["uuid"],
            },
        },
        {
            "name": "assert_over",
            "description": "Temporal test (a real monitor): subscribe, run the clock for `seconds`, \
                and require the condition to hold on EVERY notified sample — FAIL on the first \
                violation. \"monitor HR < 200 for 5s\" is uuid 2A37, op \"<\", value 200, seconds 5.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "uuid": { "type": "string" },
                    "op": { "type": "string", "enum": ["<", ">", "<=", ">=", "==", "!="] },
                    "value": { "type": "integer" },
                    "seconds": { "type": "number", "description": "Monitor window (default 2)." },
                    "byte": { "type": "integer", "description": "Byte index (default 1)." },
                },
                "required": ["uuid", "op", "value"],
            },
        },
    ]})
}

// --- JSON-RPC / MCP response envelopes -------------------------------------

fn require_script(args: Option<&Value>) -> Result<&str, &'static str> {
    args.and_then(|a| a.get("script"))
        .and_then(Value::as_str)
        .ok_or("missing required argument: script")
}

/// Finds the `value_handle` of the first discovered characteristic whose UUID
/// contains `uuid_query` (case-insensitive), in a central's `status_json`.
fn handle_for_uuid(status_json: &str, uuid_query: &str) -> Option<u16> {
    let view: Value = serde_json::from_str(status_json).ok()?;
    let query = uuid_query.to_lowercase();
    for svc in view.get("services")?.as_array()? {
        for chr in svc
            .get("characteristics")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let uuid = chr.get("uuid").and_then(Value::as_str).unwrap_or("");
            if uuid.to_lowercase().contains(&query) {
                return chr
                    .get("value_handle")
                    .and_then(Value::as_u64)
                    .map(|h| h as u16);
            }
        }
    }
    None
}

/// Evaluates `actual <op> threshold`; `None` for an unrecognized operator.
fn compare(actual: i64, op: &str, threshold: i64) -> Option<bool> {
    Some(match op {
        "<" => actual < threshold,
        ">" => actual > threshold,
        "<=" => actual <= threshold,
        ">=" => actual >= threshold,
        "==" => actual == threshold,
        "!=" => actual != threshold,
        _ => return None,
    })
}

/// The sample furthest toward violating `op` — the max for `<`/`<=`, the min
/// for `>`/`>=` — reported so a passing monitor shows how close it came.
fn extreme_for(op: &str, a: i64, b: i64) -> i64 {
    match op {
        ">" | ">=" => a.min(b),
        _ => a.max(b),
    }
}

/// Reads byte `index` of the characteristic at `handle` from a central's
/// `status_json`, whose value is uppercase hex (e.g. "0048" -> byte 1 = 72).
fn value_byte(status_json: &str, handle: u16, index: usize) -> Option<i64> {
    let view: Value = serde_json::from_str(status_json).ok()?;
    for svc in view.get("services")?.as_array()? {
        for chr in svc
            .get("characteristics")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if chr.get("value_handle").and_then(Value::as_u64) == Some(handle as u64) {
                let hex = chr.get("value").and_then(Value::as_str)?;
                let byte_hex = hex.get(index * 2..index * 2 + 2)?;
                return u8::from_str_radix(byte_hex, 16).ok().map(i64::from);
            }
        }
    }
    None
}

fn result_response(id: Option<Value>, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_response(id: Option<Value>, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// Serves the `lookup` tool: search the vendored Bluetooth SIG assigned
/// numbers. A hex query ("1809", "0x2A1C") resolves that 16-bit UUID; any
/// other query is a case-insensitive name search across services,
/// characteristics, and descriptors.
fn tool_lookup(id: Option<Value>, query: &str) -> Value {
    let query = query.trim();
    // A query is a UUID only when unambiguous: 0x-prefixed, or exactly 4 hex
    // digits ("1809"). Shorter hex-looking strings ("e", "ba") search names.
    let prefixed = query
        .strip_prefix("0x")
        .or_else(|| query.strip_prefix("0X"));
    let as_hex = prefixed.unwrap_or(query);
    if (prefixed.is_some() || as_hex.len() == 4)
        && !as_hex.is_empty()
        && as_hex.len() <= 4
        && let Ok(uuid16) = u16::from_str_radix(as_hex, 16)
    {
        return match sig_names::name_of(uuid16) {
            Some((kind, name)) => tool_text(id, &format!("0x{uuid16:04X} {kind} — {name}"), false),
            None => tool_text(
                id,
                &format!(
                    "0x{uuid16:04X} has no SIG-assigned service/characteristic/descriptor name"
                ),
                true,
            ),
        };
    }

    let needle = query.to_lowercase();
    let mut lines = Vec::new();
    for (kind, table) in [
        ("service", sig_names::SERVICE_NAMES),
        ("characteristic", sig_names::CHARACTERISTIC_NAMES),
        ("descriptor", sig_names::DESCRIPTOR_NAMES),
    ] {
        for (uuid16, name) in table {
            if name.to_lowercase().contains(&needle) {
                lines.push(format!("0x{uuid16:04X} {kind} — {name}"));
            }
        }
    }
    const CAP: usize = 40;
    match lines.len() {
        0 => tool_text(id, &format!("no SIG name matches {query:?}"), true),
        n if n > CAP => {
            let shown = lines[..CAP].join("\n");
            tool_text(
                id,
                &format!("{shown}\n… and {} more — narrow the query", n - CAP),
                false,
            )
        }
        _ => tool_text(id, &lines.join("\n"), false),
    }
}

/// Adds `"name"` fields beside 16-bit `"uuid"` fields (and a `"service_names"`
/// list beside scan reports' `"service_uuids"`), so tool output is
/// self-describing instead of bare hex. Unknown and 128-bit UUIDs are left
/// unannotated.
fn annotate_uuid_names(value: &mut Value) {
    fn name_for(uuid: &str) -> Option<&'static str> {
        let uuid16 = u16::from_str_radix(uuid, 16).ok()?;
        sig_names::name_of(uuid16).map(|(_, name)| name)
    }
    match value {
        Value::Object(map) => {
            if let Some(name) = map.get("uuid").and_then(Value::as_str).and_then(name_for) {
                map.insert("name".to_string(), json!(name));
            }
            if let Some(uuids) = map.get("service_uuids").and_then(Value::as_array) {
                let names: Vec<Value> = uuids
                    .iter()
                    .map(|u| {
                        u.as_str()
                            .and_then(name_for)
                            .map_or(Value::Null, |n| json!(n))
                    })
                    .collect();
                if names.iter().any(|n| !n.is_null()) {
                    map.insert("service_names".to_string(), json!(names));
                }
            }
            for v in map.values_mut() {
                annotate_uuid_names(v);
            }
        }
        Value::Array(items) => {
            for v in items {
                annotate_uuid_names(v);
            }
        }
        _ => {}
    }
}

/// Runs `annotate_uuid_names` over a JSON string, passing non-JSON through.
fn annotate_json(raw: &str) -> String {
    match serde_json::from_str::<Value>(raw) {
        Ok(mut v) => {
            annotate_uuid_names(&mut v);
            serde_json::to_string_pretty(&v).unwrap_or_else(|_| raw.to_string())
        }
        Err(_) => raw.to_string(),
    }
}

/// Serves the `example` tool: no name lists the samples, a known name returns
/// its script verbatim, an unknown name errors with the valid names.
fn tool_example(id: Option<Value>, name: Option<&str>) -> Value {
    match name {
        None | Some("") => {
            let peripherals: String = EXAMPLES
                .iter()
                .map(|e| format!("{} — {}\n", e.name, e.summary))
                .collect();
            let centrals: String = catalog::CENTRAL_EXAMPLES
                .iter()
                .map(|e| format!("{} — {} (drives: {})\n", e.name, e.summary, e.peer))
                .collect();
            tool_text(
                id,
                &format!(
                    "Peripherals (add_peripheral):\n{peripherals}\n\
                     Clients (add_central):\n{centrals}\n\
                     Call example with a name to get its script."
                ),
                false,
            )
        }
        Some(query) => match catalog::script(query) {
            Some(script) => tool_text(id, script, false),
            None => tool_text(
                id,
                &format!(
                    "unknown example {query:?} (have: {})",
                    catalog::names_joined()
                ),
                true,
            ),
        },
    }
}

/// A `tools/call` result: a single text content block plus the `isError` flag
/// an agent uses to notice a failing test, a bad script, or an unmet request.
fn tool_text(id: Option<Value>, text: &str, is_error: bool) -> Value {
    result_response(
        id,
        json!({ "content": [{ "type": "text", "text": text }], "isError": is_error }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const HRM: &str = r#"
        let server = android::BluetoothGattServer("HRM");
        let hrs = android::BluetoothGattService(uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY);
        hrs.add_characteristic(android::BluetoothGattCharacteristic(
            uuid::HEART_RATE_MEASUREMENT,
            android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ));
        server.add_service(hrs);
    "#;

    fn call(server: &mut Server, name: &str, args: Value) -> Value {
        server
            .handle(&json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": name, "arguments": args },
            }))
            .unwrap()
    }

    #[test]
    fn test_initialize_and_tools_list() {
        let mut s = Server::default();
        let init = s
            .handle(&json!({"jsonrpc":"2.0","id":1,"method":"initialize"}))
            .unwrap();
        assert_eq!(init["result"]["serverInfo"]["name"], "simble");
        assert_eq!(init["result"]["protocolVersion"], PROTOCOL_VERSION);

        let list = s
            .handle(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}))
            .unwrap();
        let names: Vec<&str> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        for expected in [
            "lint",
            "run_test",
            "run_on",
            "add_peripheral",
            "tick",
            "status",
        ] {
            assert!(names.contains(&expected), "missing tool {expected}");
        }
    }

    #[test]
    fn add_central_points_a_scripted_client_at_the_peripheral_the_scene_allocated() {
        // The script names an address it cannot know — MCP allocates them —
        // so the tool re-points it and says so. Without that, every client
        // script an agent copied out of `example` would sit in "connecting".
        let mut s = Server::default();
        call(
            &mut s,
            "add_peripheral",
            json!({ "script": catalog::script("hrm").unwrap() }),
        );
        let added = call(
            &mut s,
            "add_central",
            json!({ "script": catalog::script("hrm_client").unwrap() }),
        );
        assert_eq!(added["result"]["isError"], false);
        let text = added["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("pointed at"), "{text}");
        assert!(text.contains("\"phase\": \"ready\""), "{text}");
        assert!(text.contains("2A37"), "{text}");
    }

    #[test]
    fn add_central_reports_a_failed_assertion_as_a_tool_error() {
        // A client script is a test; if its assertions do not hold, the agent
        // must be told so rather than reading a healthy-looking GATT dump.
        let mut s = Server::default();
        call(
            &mut s,
            "add_peripheral",
            json!({ "script": catalog::script("hrm").unwrap() }),
        );
        let added = call(
            &mut s,
            "add_central",
            json!({ "script": r#"
                let client = android::BluetoothGatt("Probe");
                client.connect("AA:BB:CC:00:00:01");
                fn on_services_discovered(client) {
                    assert(client.services().len() == 99, "impossible service count");
                }
            "# }),
        );
        assert_eq!(added["result"]["isError"], true);
        let text = added["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("impossible service count"), "{text}");
    }

    #[test]
    fn add_central_is_refused_on_netsim_where_the_far_side_plays_the_central() {
        let mut s = Server::default();
        call(&mut s, "run_on", json!({ "target": "netsim" }));
        let added = call(&mut s, "add_central", json!({ "script": "let c = 1;" }));
        assert_eq!(added["result"]["isError"], true);
        let text = added["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("self-mode only"), "{text}");
    }

    #[test]
    fn test_run_test_pass_and_fail() {
        let mut s = Server::default();
        let pass = call(
            &mut s,
            "run_test",
            json!({"script": r#"let x = android::BluetoothGattServer("t"); assert(x.name == "t", "n");"#}),
        );
        assert_eq!(pass["result"]["isError"], false);
        assert!(
            pass["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("PASS")
        );

        let fail = call(
            &mut s,
            "run_test",
            json!({"script": r#"assert(1 == 2, "nope");"#}),
        );
        assert_eq!(fail["result"]["isError"], true);
    }

    #[test]
    fn test_lint_without_running() {
        let mut s = Server::default();
        assert_eq!(
            call(&mut s, "lint", json!({"script": "let a = 1;"}))["result"]["isError"],
            false
        );
        assert_eq!(
            call(&mut s, "lint", json!({"script": "let a = ;"}))["result"]["isError"],
            true
        );
    }

    #[test]
    fn test_example_lists_serves_and_rejects() {
        let mut s = Server::default();

        let listing = call(&mut s, "example", json!({}));
        assert_eq!(listing["result"]["isError"], false);
        let text = listing["result"]["content"][0]["text"].as_str().unwrap();
        for example in EXAMPLES {
            let name = example.name;
            assert!(text.contains(name), "listing should name {name}: {text}");
        }

        let hrm = call(&mut s, "example", json!({"name": "hrm"}));
        assert_eq!(hrm["result"]["isError"], false);
        assert!(
            hrm["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("BluetoothGattServer")
        );

        let unknown = call(&mut s, "example", json!({"name": "toaster"}));
        assert_eq!(unknown["result"]["isError"], true);
    }

    #[test]
    fn test_lookup_by_name_and_by_uuid() {
        let mut s = Server::default();

        let by_name = call(&mut s, "lookup", json!({"query": "therm"}));
        assert_eq!(by_name["result"]["isError"], false);
        let text = by_name["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("0x1809 service — Health Thermometer"),
            "{text}"
        );

        let chars = call(&mut s, "lookup", json!({"query": "temperature meas"}));
        assert!(
            chars["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("0x2A1C characteristic — Temperature Measurement")
        );

        let by_uuid = call(&mut s, "lookup", json!({"query": "0x181A"}));
        assert_eq!(by_uuid["result"]["isError"], false);
        assert!(
            by_uuid["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("Environmental Sensing")
        );

        let miss = call(&mut s, "lookup", json!({"query": "FFFF"}));
        assert_eq!(miss["result"]["isError"], true);
        let broad = call(&mut s, "lookup", json!({"query": "e"}));
        let text = broad["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("more — narrow the query"), "capped: {text}");
    }

    #[test]
    fn test_status_and_scan_annotate_sig_names() {
        let mut s = Server::default();
        call(&mut s, "add_peripheral", json!({"script": HRM}));
        call(&mut s, "tick", json!({"seconds": 0.2}));

        let status = call(&mut s, "status", json!({}));
        let text = status["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Heart Rate Measurement"), "status: {text}");

        let scan = call(&mut s, "scan", json!({}));
        let text = scan["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Heart Rate"), "scan: {text}");
    }

    #[test]
    fn test_every_example_lints_runs_and_ticks() {
        // The samples are the served API docs — each must lint, join a live
        // scene, and tick without a script error.
        for &catalog::DeviceExample { name, script, .. } in EXAMPLES {
            let mut s = Server::default();
            let linted = call(&mut s, "lint", json!({"script": script}));
            assert_eq!(
                linted["result"]["isError"], false,
                "example {name} should lint: {linted}"
            );

            let added = call(&mut s, "add_peripheral", json!({"script": script}));
            assert_eq!(
                added["result"]["isError"], false,
                "example {name} should load: {added}"
            );

            call(&mut s, "tick", json!({"seconds": 1.0}));
            let status = call(&mut s, "status", json!({}));
            let text = status["result"]["content"][0]["text"].as_str().unwrap();
            assert!(
                text.contains("\"last_error\": null") || text.contains("\"last_error\":null"),
                "example {name} should tick cleanly: {text}"
            );
        }
    }

    #[test]
    fn test_scene_lifecycle_self_add_tick_status() {
        let mut s = Server::default();
        assert_eq!(
            call(&mut s, "run_on", json!({"target": "self"}))["result"]["isError"],
            false
        );

        let added = call(&mut s, "add_peripheral", json!({"script": HRM}));
        assert_eq!(added["result"]["isError"], false);

        call(&mut s, "tick", json!({"seconds": 0.2}));

        let status = call(&mut s, "status", json!({}));
        assert_eq!(status["result"]["isError"], false);
        let text = status["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("\"controller\": \"self\""));
        assert!(
            text.contains("HRM"),
            "status should name the device: {text}"
        );
    }

    #[test]
    fn test_scan_hears_the_scripted_peripheral() {
        let mut s = Server::default();
        call(&mut s, "add_peripheral", json!({"script": HRM}));
        let scan = call(&mut s, "scan", json!({}));
        assert_eq!(scan["result"]["isError"], false);
        let reports = scan["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            reports.contains("HRM"),
            "scanner should hear the HRM advert: {reports}"
        );
    }

    #[test]
    fn test_scan_dedupes_accumulated_reports() {
        // Ticking between scans piles up duplicate adverts; scan must return
        // one entry per advertiser with a count, not the raw backlog.
        let mut s = Server::default();
        call(&mut s, "add_peripheral", json!({"script": HRM}));
        call(&mut s, "scan", json!({}));
        call(&mut s, "tick", json!({"seconds": 3.0}));

        let scan = call(&mut s, "scan", json!({}));
        let text = scan["result"]["content"][0]["text"].as_str().unwrap();
        let reports: Vec<Value> = serde_json::from_str(text).unwrap();
        assert_eq!(reports.len(), 1, "one entry per advertiser: {text}");
        assert!(
            reports[0]["reports"].as_u64().unwrap() > 1,
            "backlog should be counted, not repeated: {text}"
        );
        assert_eq!(reports[0]["name"], "HRM");
    }

    /// Pulls a characteristic's hex value out of a status/read JSON blob and
    /// decodes it to bytes. Used by the device tests to read a value without
    /// depending on a particular byte offset the `assert` tool would need.
    fn characteristic_value(json_text: &str, uuid: &str) -> Option<Vec<u8>> {
        let value: Value = serde_json::from_str(json_text.get(json_text.find('{')?..)?).ok()?;
        fn walk(node: &Value, uuid: &str) -> Option<String> {
            match node {
                Value::Object(map) => {
                    if map.get("uuid").and_then(Value::as_str) == Some(uuid)
                        && let Some(Value::String(hex)) = map.get("value")
                    {
                        return Some(hex.clone());
                    }
                    map.values().find_map(|v| walk(v, uuid))
                }
                Value::Array(items) => items.iter().find_map(|v| walk(v, uuid)),
                _ => None,
            }
        }
        let hex = walk(&value, uuid)?;
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
            .collect()
    }

    /// Adds the named example to a fresh server and connects a central.
    fn serve_example(name: &str) -> Server {
        let script = catalog::script(name).unwrap_or_else(|| panic!("no example named {name}"));
        let mut server = Server::default();
        let added = call(&mut server, "add_peripheral", json!({"script": script}));
        assert_eq!(added["result"]["isError"], false, "{name}: {added}");
        let connected = call(&mut server, "connect", json!({}));
        assert_eq!(connected["result"]["isError"], false, "{name}: {connected}");
        server
    }

    #[test]
    fn test_smart_lock_control_point_locks_and_unlocks() {
        // The lock is the control-point idiom over a vendor service: a write
        // is a command, and the state characteristic is the result.
        const STATE: &str = "d3a70002-1f8a-4b2c-9a11-000000000001";
        const CONTROL: &str = "d3a70003-1f8a-4b2c-9a11-000000000001";
        let mut s = serve_example("smart_lock");

        // Starts locked.
        let locked = call(
            &mut s,
            "assert",
            json!({"uuid": STATE, "op": "==", "value": 1, "byte": 0}),
        );
        assert_eq!(locked["result"]["isError"], false, "{locked}");

        // 0x02 = unlock.
        call(&mut s, "write", json!({"uuid": CONTROL, "value": [0x02]}));
        call(&mut s, "tick", json!({"seconds": 0.2}));
        let unlocked = call(
            &mut s,
            "assert",
            json!({"uuid": STATE, "op": "==", "value": 0, "byte": 0}),
        );
        assert_eq!(unlocked["result"]["isError"], false, "{unlocked}");

        // The command is consumed, so the state holds until the next write.
        call(&mut s, "tick", json!({"seconds": 0.4}));
        let still = call(
            &mut s,
            "assert",
            json!({"uuid": STATE, "op": "==", "value": 0, "byte": 0}),
        );
        assert_eq!(still["result"]["isError"], false, "{still}");

        // 0x01 = lock again.
        call(&mut s, "write", json!({"uuid": CONTROL, "value": [0x01]}));
        call(&mut s, "tick", json!({"seconds": 0.2}));
        let relocked = call(
            &mut s,
            "assert",
            json!({"uuid": STATE, "op": "==", "value": 1, "byte": 0}),
        );
        assert_eq!(relocked["result"]["isError"], false, "{relocked}");
    }

    #[test]
    fn test_hid_keyboard_emits_key_and_release_reports() {
        // A keystroke is two reports: the key held, then an empty report.
        // Byte 2 is the first key slot (after modifiers and the reserved byte).
        let mut s = serve_example("hid_keyboard");
        // The clock already advanced during connect, so sample a window
        // rather than assuming an exact `t`.
        let mut keys_seen = Vec::new();
        for _ in 0..8 {
            call(&mut s, "tick", json!({"seconds": 0.5}));
            let read = call(&mut s, "read", json!({"uuid": "2A4D"}));
            let text = read["result"]["content"][0]["text"].as_str().unwrap();
            if let Some(value) = characteristic_value(text, "2A4D")
                && value.len() >= 3
            {
                keys_seen.push(value[2]);
            }
        }
        assert!(
            keys_seen.iter().any(|&k| k != 0),
            "a key should be held at some point: {keys_seen:?}"
        );
        assert!(keys_seen.contains(&0), "and released again: {keys_seen:?}");

        // The report map must be readable and start with the HID descriptor
        // for Usage Page (Generic Desktop) — without it a host cannot decode
        // the reports at all.
        let map = call(&mut s, "read", json!({"uuid": "2A4B"}));
        assert_eq!(map["result"]["isError"], false, "{map}");
        assert!(
            map["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("0501"),
            "report map should be present: {map}"
        );
    }

    /// The example's Report Map is not decoration: it is the only thing that
    /// tells a host these bytes are pointer motion. Checked with the same
    /// descriptor walker a real host uses, so an edit that breaks the item
    /// encoding fails here rather than silently producing a device nothing
    /// can interpret.
    #[test]
    fn test_hid_mouse_report_map_identifies_a_mouse_to_a_host() {
        use crate::devices::helpers::hid_reports::top_level_usage;
        let mut s = serve_example("hid_mouse");

        let map = call(&mut s, "read", json!({"uuid": "2A4B"}));
        let text = map["result"]["content"][0]["text"].as_str().unwrap();
        let descriptor = characteristic_value(text, "2A4B").expect("report map");
        // Generic Desktop (0x01), Mouse (0x02).
        assert_eq!(top_level_usage(&descriptor), Some((0x01, 0x02)));

        call(&mut s, "tick", json!({"seconds": 0.5}));
        let read = call(&mut s, "read", json!({"uuid": "2A4D"}));
        let text = read["result"]["content"][0]["text"].as_str().unwrap();
        let report = characteristic_value(text, "2A4D").expect("input report");
        assert_eq!(
            report.len(),
            4,
            "the descriptor declares 3 relative axes plus the button byte"
        );
    }

    #[test]
    fn test_cycling_counters_only_increase() {
        // Speed is computed by the phone from cumulative counts, so the
        // counter must be monotonic — a wrapping or resetting one reads as
        // a huge negative speed.
        let mut s = serve_example("cycling");
        call(&mut s, "tick", json!({"seconds": 3.0}));
        let at_three = call(
            &mut s,
            "assert",
            json!({"uuid": "2A5B", "op": "==", "value": 3, "byte": 1}),
        );
        assert_eq!(at_three["result"]["isError"], false, "{at_three}");

        call(&mut s, "tick", json!({"seconds": 4.0}));
        let later = call(
            &mut s,
            "assert",
            json!({"uuid": "2A5B", "op": ">", "value": 3, "byte": 1}),
        );
        assert_eq!(later["result"]["isError"], false, "{later}");

        // Feature bits advertise wheel + crank data.
        let feature = call(
            &mut s,
            "assert",
            json!({"uuid": "2A5C", "op": "==", "value": 0x03, "byte": 0}),
        );
        assert_eq!(feature["result"]["isError"], false, "{feature}");
    }

    #[test]
    fn test_fitness_tracker_exposes_every_service() {
        // A wearable is several services on one server; the point of the
        // example is that they coexist and all stay live.
        let mut s = serve_example("fitness_tracker");
        call(&mut s, "tick", json!({"seconds": 1.0}));

        let status = call(&mut s, "status", json!({}));
        let text = status["result"]["content"][0]["text"].as_str().unwrap();
        for service in ["180D", "180F", "180A"] {
            assert!(text.contains(service), "missing service {service}: {text}");
        }
        // Read the device's own view rather than the central's: a device
        // mixing 16-bit and 128-bit services trips a discovery bug in
        // `CentralDevice` (phantom services, repeated characteristics), so
        // going through the central here would test that bug, not this
        // device. See docs/android-peripherals.md.
        let heart_rate = characteristic_value(text, "2A37").expect("heart rate present");
        assert!(
            heart_rate.len() >= 2 && heart_rate[1] >= 64,
            "heart rate should be live: {heart_rate:?}"
        );
        let battery = characteristic_value(text, "2A19").expect("battery present");
        assert_eq!(battery, vec![84]);
        let steps = characteristic_value(text, "f1e20002-8c3d-4a5b-9e6f-000000000001")
            .expect("step counter present");
        assert_eq!(steps.len(), 4, "steps are a 32-bit counter: {steps:?}");
    }

    #[test]
    fn test_pulse_oximeter_and_scale_report_plausible_values() {
        let mut s = serve_example("pulse_oximeter");
        call(&mut s, "tick", json!({"seconds": 1.0}));
        // SpO2 is a percentage: anything above 100 is a decoding bug.
        let spo2 = call(
            &mut s,
            "assert",
            json!({"uuid": "2A5F", "op": "<=", "value": 100, "byte": 1}),
        );
        assert_eq!(spo2["result"]["isError"], false, "{spo2}");

        let mut scale = serve_example("weight_scale");
        call(&mut scale, "tick", json!({"seconds": 0.5}));
        let status = call(&mut scale, "status", json!({}));
        let text = status["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("181D"), "weight scale service: {text}");
        assert!(text.contains("181B"), "body composition service: {text}");
    }

    #[test]
    fn test_beacons_are_non_connectable_broadcasters() {
        // A beacon's identity is its advertisement; it must not offer a
        // connection, or a scanner shows it as a connectable peripheral.
        for name in ["eddystone", "fast_pair"] {
            let script = catalog::script(name).unwrap();
            assert!(
                script.contains("advertise_connectable(false)"),
                "{name} must be broadcast-only"
            );
            assert!(
                script.contains("advertise_service_data"),
                "{name} must carry service data"
            );
        }
    }

    #[test]
    fn test_ranging_devices_publish_distance_over_the_ranging_service() {
        // Channel Sounding's measurement is a controller procedure; what a
        // phone talks to is the Ranging Service, so that is what these
        // devices must actually expose and update.
        for name in ["ranging", "ranging_tag"] {
            let script = catalog::script(name).unwrap();
            let mut s = Server::default();
            let added = call(&mut s, "add_peripheral", json!({"script": script}));
            assert_eq!(added["result"]["isError"], false, "{name}: {added}");
            assert_eq!(
                call(&mut s, "connect", json!({}))["result"]["isError"],
                false
            );

            // Real-Time Ranging Data is [f32 metres, f32 confidence] LE.
            call(&mut s, "tick", json!({"seconds": 1.0}));
            let read = call(&mut s, "read", json!({"uuid": "2C15"}));
            assert_eq!(read["result"]["isError"], false, "{name}: {read}");
            let text = read["result"]["content"][0]["text"].as_str().unwrap();
            let value = text
                .split("\"2C15\"")
                .nth(1)
                .and_then(|t| t.split("\"value\":\"").nth(1))
                .and_then(|t| t.split('"').next())
                .unwrap_or("");
            assert_eq!(value.len(), 16, "{name}: 8 bytes of ranging data: {value}");
            let bytes: Vec<u8> = (0..8)
                .map(|i| u8::from_str_radix(&value[i * 2..i * 2 + 2], 16).unwrap())
                .collect();
            let metres = f32::from_le_bytes(bytes[0..4].try_into().unwrap());
            let confidence = f32::from_le_bytes(bytes[4..8].try_into().unwrap());
            assert!(
                (0.5..10.0).contains(&metres),
                "{name}: a plausible distance, got {metres}"
            );
            assert!(
                (0.0..=1.0).contains(&confidence),
                "{name}: confidence is a fraction, got {confidence}"
            );
        }
    }

    #[test]
    fn test_volume_control_point_commands_change_state() {
        // The LE Audio control-point idiom end to end: write an opcode, the
        // device applies it and reports the new state.
        let script = catalog::script("volume").unwrap();
        let mut s = Server::default();
        call(&mut s, "add_peripheral", json!({"script": script}));
        assert_eq!(
            call(&mut s, "connect", json!({}))["result"]["isError"],
            false
        );

        // Set Absolute Volume (0x04) to 200.
        let wrote = call(
            &mut s,
            "write",
            json!({"uuid": "2B7E", "value": [0x04, 0x00, 200]}),
        );
        assert_eq!(wrote["result"]["isError"], false, "write: {wrote}");
        call(&mut s, "tick", json!({"seconds": 0.2}));
        let at_200 = call(
            &mut s,
            "assert",
            json!({"uuid": "2B7D", "op": "==", "value": 200, "byte": 0}),
        );
        assert_eq!(at_200["result"]["isError"], false, "{at_200}");

        // Relative Volume Down (0x00) steps by 16.
        call(
            &mut s,
            "write",
            json!({"uuid": "2B7E", "value": [0x00, 0x01]}),
        );
        call(&mut s, "tick", json!({"seconds": 0.2}));
        let stepped = call(
            &mut s,
            "assert",
            json!({"uuid": "2B7D", "op": "==", "value": 184, "byte": 0}),
        );
        assert_eq!(stepped["result"]["isError"], false, "{stepped}");

        // Mute (0x06) sets the mute byte without touching the volume.
        call(
            &mut s,
            "write",
            json!({"uuid": "2B7E", "value": [0x06, 0x02]}),
        );
        call(&mut s, "tick", json!({"seconds": 0.2}));
        let muted = call(
            &mut s,
            "assert",
            json!({"uuid": "2B7D", "op": "==", "value": 1, "byte": 1}),
        );
        assert_eq!(muted["result"]["isError"], false, "{muted}");
        let still_184 = call(
            &mut s,
            "assert",
            json!({"uuid": "2B7D", "op": "==", "value": 184, "byte": 0}),
        );
        assert_eq!(still_184["result"]["isError"], false, "{still_184}");
    }

    #[test]
    fn test_write_setpoint_drives_the_thermostat() {
        // The settable-device flow: connect, write the custom setpoint, and
        // the script's tick converges the ESS temperature onto it.
        let script = catalog::script("thermostat").unwrap();
        let mut s = Server::default();
        call(&mut s, "add_peripheral", json!({"script": script}));
        assert_eq!(
            call(&mut s, "connect", json!({}))["result"]["isError"],
            false
        );

        let wrote = call(
            &mut s,
            "write",
            json!({"uuid": "5e7b0002-c0de-4a11-b1e5-0000c0ffee01", "value": [25]}),
        );
        assert_eq!(wrote["result"]["isError"], false, "write: {wrote}");

        call(&mut s, "tick", json!({"seconds": 2.0}));
        let held = call(
            &mut s,
            "assert",
            json!({"uuid": "2A6E", "op": "==", "value": 25}),
        );
        assert_eq!(
            held["result"]["isError"], false,
            "temperature should reach the written setpoint: {held}"
        );

        let missing = call(&mut s, "write", json!({"uuid": "BEEF", "value": [1]}));
        assert_eq!(missing["result"]["isError"], true);
    }

    #[test]
    fn test_connect_read_assert_hr_below_200() {
        // The agentic flow behind "create a test that monitors HR < 200":
        // add a peripheral with HR = 72, connect a central, assert HR < 200.
        const HRM_72: &str = r#"
            let server = android::BluetoothGattServer("HRM");
            let hrs = android::BluetoothGattService(uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY);
            let hr = android::BluetoothGattCharacteristic(uuid::HEART_RATE_MEASUREMENT,
                android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
            hr.set_value([0x00, 72]);
            hrs.add_characteristic(hr);
            server.add_service(hrs);
        "#;
        let mut s = Server::default();
        call(&mut s, "add_peripheral", json!({"script": HRM_72}));
        let connected = call(&mut s, "connect", json!({}));
        assert_eq!(
            connected["result"]["isError"], false,
            "connect: {connected}"
        );

        let pass = call(
            &mut s,
            "assert",
            json!({"uuid": "2A37", "op": "<", "value": 200}),
        );
        assert_eq!(
            pass["result"]["isError"], false,
            "HR 72 < 200 should PASS: {pass}"
        );
        assert!(
            pass["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("PASS")
        );

        let fail = call(
            &mut s,
            "assert",
            json!({"uuid": "2A37", "op": ">", "value": 200}),
        );
        assert_eq!(fail["result"]["isError"], true, "HR 72 > 200 should FAIL");
    }

    #[test]
    fn test_assert_over_monitors_notifications() {
        // A peripheral that updates HR every tick (fn tick + update_value), so
        // the monitor samples notified values over time.
        fn hrm(hr: u8) -> String {
            format!(
                r#"
                let server = android::BluetoothGattServer("HRM");
                let hrs = android::BluetoothGattService(uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY);
                let hr = android::BluetoothGattCharacteristic(uuid::HEART_RATE_MEASUREMENT,
                    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
                hr.set_value([0x00, {hr}]);
                hrs.add_characteristic(hr);
                server.add_service(hrs);
                fn tick(server, t) {{ server.update_value(uuid::HEART_RATE_MEASUREMENT, [0x00, {hr}]); }}
            "#
            )
        }

        // Safe HR (72): monitoring "< 200" holds across all samples.
        let mut s = Server::default();
        call(&mut s, "add_peripheral", json!({"script": hrm(72)}));
        call(&mut s, "connect", json!({}));
        let ok = call(
            &mut s,
            "assert_over",
            json!({"uuid":"2A37","op":"<","value":200,"seconds":0.5}),
        );
        assert_eq!(
            ok["result"]["isError"], false,
            "72 < 200 over time should PASS: {ok}"
        );
        assert!(
            ok["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("PASS")
        );

        // Unsafe HR (220): monitoring "< 200" catches the violation.
        let mut s2 = Server::default();
        call(&mut s2, "add_peripheral", json!({"script": hrm(220)}));
        call(&mut s2, "connect", json!({}));
        let bad = call(
            &mut s2,
            "assert_over",
            json!({"uuid":"2A37","op":"<","value":200,"seconds":0.5}),
        );
        assert_eq!(
            bad["result"]["isError"], true,
            "220 < 200 should FAIL: {bad}"
        );
    }

    #[test]
    fn test_add_peripheral_rejects_bad_script() {
        let mut s = Server::default();
        // Compiles, but builds no server -> rejected by run_script.
        let resp = call(&mut s, "add_peripheral", json!({"script": "let x = 1;"}));
        assert_eq!(resp["result"]["isError"], true);
    }

    #[test]
    fn test_run_on_netsim_selects_the_backend() {
        // Selecting netsim succeeds without a running netsimd (connections
        // happen per-peripheral); central-side tools are then refused.
        let mut s = Server::default();
        let resp = call(&mut s, "run_on", json!({"target": "netsim"}));
        assert_eq!(resp["result"]["isError"], false, "{resp}");
        assert!(
            resp["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("netsim")
        );

        let scan = call(&mut s, "scan", json!({}));
        assert_eq!(scan["result"]["isError"], true);
        assert!(
            scan["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("self-mode only")
        );

        let unknown = call(&mut s, "run_on", json!({"target": "rootcanal"}));
        assert_eq!(unknown["result"]["isError"], true);
    }

    // --- run_on("usb") ------------------------------------------------------
    //
    // No test here touches a dongle: `run_on` only *selects* the backend, and
    // `UsbScene` defers opening to the first `add_peripheral` exactly as the
    // netsim scene defers its connection. What is covered is argument
    // parsing, the dispatch, and the error paths; the live path — a real
    // dongle advertising to a real phone — is not exercised by CI.

    #[test]
    fn test_run_on_usb_selects_the_dongle_backend() {
        let mut s = Server::default();
        let auto = call(&mut s, "run_on", json!({"target": "usb"}));
        assert_eq!(auto["result"]["isError"], false, "{auto}");
        let text = auto["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("runs on: usb"), "{text}");
        assert!(text.contains("first Bluetooth-class dongle"), "{text}");

        // An explicit dongle is echoed back normalized, so an agent can see
        // which one it actually asked for.
        let chosen = call(
            &mut s,
            "run_on",
            json!({"target": "usb", "device": "0A12:0001"}),
        );
        assert_eq!(chosen["result"]["isError"], false, "{chosen}");
        assert!(
            chosen["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("0a12:0001"),
            "{chosen}"
        );

        // Like every live backend, it is peripheral-only.
        let connect = call(&mut s, "connect", json!({}));
        assert_eq!(connect["result"]["isError"], true);
        let text = connect["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("self-mode only"), "{text}");
        assert!(text.contains("on usb"), "{text}");

        // …and status reports which controller is selected, with no device
        // on it and no hardware consulted.
        let status = call(&mut s, "status", json!({}));
        assert_eq!(status["result"]["isError"], false, "{status}");
        let text = status["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("\"controller\": \"usb\""), "{text}");
    }

    #[test]
    fn test_run_on_usb_rejects_a_malformed_device_selector() {
        // A vid:pid typo must fail at the call that contains it, naming the
        // expected form — not several calls later as "dongle not found".
        let mut s = Server::default();
        let bad = call(
            &mut s,
            "run_on",
            json!({"target": "usb", "device": "0a120001"}),
        );
        assert_eq!(bad["result"]["isError"], true, "{bad}");
        let text = bad["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("0a12:0001"),
            "names the expected form: {text}"
        );

        // A rejected selector leaves the previous scene alone rather than
        // half-switching to a backend that was never built.
        assert!(s.live.is_none(), "no backend selected on a bad selector");
    }

    #[test]
    fn test_usb_add_peripheral_without_a_dongle_reports_it_as_a_device_error() {
        // A vid:pid that cannot exist, so the outcome is the same whether or
        // not the machine running the tests has a dongle plugged in. This is
        // the only place the USB path really tries to open hardware.
        let mut s = Server::default();
        call(
            &mut s,
            "run_on",
            json!({"target": "usb", "device": "ffff:ffff"}),
        );
        let added = call(&mut s, "add_peripheral", json!({"script": HRM}));
        assert_eq!(added["result"]["isError"], true, "{added}");
        let text = added["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("device rejected:"), "{text}");
        assert!(
            text.contains("dongle") || text.contains("USB"),
            "should say what could not be opened: {text}"
        );
    }

    #[test]
    fn test_netsim_add_peripheral_unreachable_gives_hint() {
        // Bind-then-drop a listener to get a port that refuses connections,
        // so the test is deterministic whether or not a netsimd is running.
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let mut scene = NetsimScene::new(&format!("ws://127.0.0.1:{port}"));
        let err = scene
            .add_peripheral("F0:DE:C0:00:00:01".parse().unwrap(), "let a = 1;")
            .unwrap_err();
        // A GATT-server-less script is rejected before any connection…
        assert!(err.contains("BluetoothGattServer"), "{err}");

        let err = scene
            .add_peripheral(
                "F0:DE:C0:00:00:01".parse().unwrap(),
                r#"let server = android::BluetoothGattServer("X");"#,
            )
            .unwrap_err();
        assert!(err.contains("netsimd"), "should carry the hint: {err}");
    }

    #[test]
    fn test_notification_and_unknown_method() {
        let mut s = Server::default();
        assert!(
            s.handle(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
                .is_none()
        );
        let err = s
            .handle(&json!({"jsonrpc":"2.0","id":9,"method":"nope"}))
            .unwrap();
        assert_eq!(err["error"]["code"], -32601);
    }

    // --- server→client notifications ----------------------------------------

    /// A peripheral whose heart rate is fine until t = 1s and alarming after.
    /// The CCCD is what makes the watch a *pushed* one: without it the central
    /// has nothing to write and the peripheral never notifies.
    const HRM_SPIKES: &str = r#"
        let server = android::BluetoothGattServer("HRM");
        let hrs = android::BluetoothGattService(uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY);
        let hr = android::BluetoothGattCharacteristic(uuid::HEART_RATE_MEASUREMENT,
            android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
        hr.set_value([0x00, 70]);
        hr.add_descriptor(android::BluetoothGattDescriptor(
            uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
            android::PERMISSION_READ | android::PERMISSION_WRITE));
        hrs.add_characteristic(hr);
        server.add_service(hrs);
        fn tick(server, t) {
            if t > 1.0 {
                server.update_value(uuid::HEART_RATE_MEASUREMENT, [0x00, 220]);
            } else {
                server.update_value(uuid::HEART_RATE_MEASUREMENT, [0x00, 70]);
            }
        }
    "#;

    #[test]
    fn test_a_subscribe_watch_pushes_an_id_less_notification_when_it_breaks() {
        // The asynchronous half of the monitor: arm a condition, run the
        // clock, and the server speaks first. The wire-visible difference
        // from a response is that there is no `id` — nothing is replying.
        let mut s = Server::default();
        call(&mut s, "add_peripheral", json!({"script": HRM_SPIKES}));
        call(&mut s, "connect", json!({}));

        let armed = call(
            &mut s,
            "subscribe",
            json!({"uuid": "2A37", "op": "<", "value": 200}),
        );
        assert_eq!(armed["result"]["isError"], false, "{armed}");
        assert!(
            armed["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("watching"),
            "{armed}"
        );
        assert!(
            s.take_notifications().is_empty(),
            "a condition that holds says nothing"
        );

        let mut pushed = Vec::new();
        for _ in 0..20 {
            call(&mut s, "tick", json!({"seconds": 0.2}));
            pushed = s.take_notifications();
            if !pushed.is_empty() {
                break;
            }
        }
        assert_eq!(pushed.len(), 1, "one message for one violation: {pushed:?}");

        let note = &pushed[0];
        assert_eq!(note["jsonrpc"], "2.0");
        assert_eq!(note["method"], "notifications/message");
        assert!(
            note.get("id").is_none(),
            "a notification carries no id: {note}"
        );
        assert_eq!(note["params"]["level"], "warning");
        assert_eq!(note["params"]["logger"], "simble.monitor");
        assert_eq!(note["params"]["data"]["value"], 220);
        assert_eq!(note["params"]["data"]["expected"], "< 200");
        assert!(
            note["params"]["data"]["message"]
                .as_str()
                .unwrap()
                .contains("no longer < 200"),
            "{note}"
        );

        // A condition that stays broken announces itself once, not per tick.
        for _ in 0..5 {
            call(&mut s, "tick", json!({"seconds": 0.2}));
        }
        assert!(
            s.take_notifications().is_empty(),
            "a sustained violation must not spam the client"
        );
    }

    #[test]
    fn test_subscribe_without_a_condition_stays_a_plain_subscribe() {
        // The watch is opt-in: the pre-existing tool call must behave exactly
        // as it did, pushing nothing however long the clock runs.
        let mut s = Server::default();
        call(&mut s, "add_peripheral", json!({"script": HRM_SPIKES}));
        call(&mut s, "connect", json!({}));
        let plain = call(&mut s, "subscribe", json!({"uuid": "2A37"}));
        assert_eq!(plain["result"]["isError"], false, "{plain}");
        assert!(
            !plain["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("watching")
        );
        for _ in 0..10 {
            call(&mut s, "tick", json!({"seconds": 0.2}));
        }
        assert!(s.take_notifications().is_empty());
    }

    #[test]
    fn test_subscribe_rejects_half_a_condition() {
        let mut s = Server::default();
        call(&mut s, "add_peripheral", json!({"script": HRM_SPIKES}));
        call(&mut s, "connect", json!({}));
        for half in [
            json!({"uuid": "2A37", "op": "<"}),
            json!({"uuid": "2A37", "value": 200}),
        ] {
            let resp = call(&mut s, "subscribe", half.clone());
            assert_eq!(resp["result"]["isError"], true, "{half}: {resp}");
            assert!(
                resp["result"]["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .contains("op AND value"),
                "{resp}"
            );
        }
        let bad_op = call(
            &mut s,
            "subscribe",
            json!({"uuid": "2A37", "op": "=~", "value": 200}),
        );
        assert_eq!(bad_op["result"]["isError"], true, "{bad_op}");
    }

    // --- the actor loop -----------------------------------------------------

    /// A sink that only publishes on `flush`, so a reader never observes half
    /// a JSON object. `write_message` flushes once per complete message.
    #[derive(Default)]
    struct SharedOut {
        pending: Vec<u8>,
        sink: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    }

    impl Write for SharedOut {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.pending.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            let mut sink = self.sink.lock().unwrap();
            sink.extend_from_slice(&std::mem::take(&mut self.pending));
            Ok(())
        }
    }

    /// A reader whose `read` **blocks** until the test hands it a line — a
    /// stand-in for a real stdin that is simply quiet. The loop under test
    /// must make progress anyway.
    struct BlockingLines {
        rx: mpsc::Receiver<String>,
        pending: Vec<u8>,
        pos: usize,
    }

    impl std::io::Read for BlockingLines {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            while self.pos == self.pending.len() {
                match self.rx.recv() {
                    Ok(line) => {
                        self.pending = line.into_bytes();
                        self.pos = 0;
                    }
                    Err(_) => return Ok(0), // sender dropped: EOF
                }
            }
            let n = (self.pending.len() - self.pos).min(out.len());
            out[..n].copy_from_slice(&self.pending[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }

    /// Waits until the sink holds at least `n` complete messages.
    fn wait_for_messages(
        sink: &std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
        n: usize,
        what: &str,
    ) -> Vec<Value> {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let text = String::from_utf8(sink.lock().unwrap().clone()).unwrap();
            let messages: Vec<Value> = text
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| serde_json::from_str(line).expect("each line is one JSON message"))
                .collect();
            if messages.len() >= n {
                return messages;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {what} (have {messages:?})"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn test_actor_loop_pushes_notifications_while_input_is_idle() {
        // `serve_stdio` had no coverage at all (docs/test-strategy.md gap 7),
        // and the regression it hides is silent: reinstate a blocking read on
        // the input and every request is still answered, so the suite stays
        // green while the server can no longer pump a live backend or say
        // anything unprompted. Here nothing is ever sent until after the
        // server has spoken — a loop that blocks on input never gets there.
        let mut server = Server::default();
        server.push_notification("warning", "simble.test", json!({"event": "unprompted"}));

        let sink = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let out = SharedOut {
            pending: Vec::new(),
            sink: sink.clone(),
        };
        let (tx, rx) = mpsc::channel::<String>();
        let input = BlockingLines {
            rx,
            pending: Vec::new(),
            pos: 0,
        };

        // The scene is non-`Send` (Rhai), so the loop stays on this thread and
        // the *test* is what runs beside it. A failed assertion or a timeout
        // in the driver drops `tx`, which is the loop's EOF, so a wedged loop
        // fails the test instead of hanging it.
        let driver = std::thread::spawn({
            let sink = sink.clone();
            move || {
                let messages = wait_for_messages(&sink, 1, "the unprompted notification");
                assert_eq!(messages[0]["method"], "notifications/message");
                assert!(
                    messages[0].get("id").is_none(),
                    "a notification carries no id: {}",
                    messages[0]
                );

                // The same loop still answers a request that turns up later.
                tx.send("{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"ping\"}\n".to_string())
                    .unwrap();
                let messages = wait_for_messages(&sink, 2, "the ping response");
                assert_eq!(messages[1]["id"], 7);
                assert_eq!(messages[1]["result"], json!({}));
                drop(tx); // EOF
            }
        });

        serve_lines(server, std::io::BufReader::new(input), out).expect("the loop exits at EOF");
        driver.join().expect("the driver's assertions hold");
    }

    // --- MCP over WebSocket (`--ws-server`) ---------------------------------

    /// A minimal RFC 6455 *client* for the scenario test, built from the same
    /// codec the server uses (`transport::ws`) — the netsim client is
    /// HCI-shaped, and MCP travels as text.
    struct WsTestClient {
        stream: std::net::TcpStream,
        reader: crate::transport::ws::WsFrameReader,
    }

    impl WsTestClient {
        fn connect(addr: std::net::SocketAddr) -> Self {
            use std::io::Read;
            let mut stream = std::net::TcpStream::connect(addr).expect("connect");
            let key = "dGhlIHNhbXBsZSBub25jZQ==";
            let request = format!(
                "GET /mcp HTTP/1.1\r\n\
                 Host: 127.0.0.1\r\n\
                 Upgrade: websocket\r\n\
                 Connection: Upgrade\r\n\
                 Sec-WebSocket-Key: {key}\r\n\
                 Sec-WebSocket-Version: 13\r\n\r\n"
            );
            stream.write_all(request.as_bytes()).unwrap();
            let response = crate::transport::ws::read_http_headers(&mut stream).unwrap();
            assert!(response.starts_with("HTTP/1.1 101 "), "{response}");
            assert!(
                response.contains(&crate::transport::ws::expected_accept(key)),
                "{response}"
            );
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let _ = &mut stream as &mut dyn Read; // reads happen in `recv`
            Self {
                stream,
                reader: crate::transport::ws::WsFrameReader::default(),
            }
        }

        fn send(&mut self, request: &str) {
            let frame = crate::transport::ws::encode_frame(
                crate::transport::ws::OPCODE_TEXT,
                request.as_bytes(),
                Some(crate::transport::ws::mask_key()),
            );
            self.stream.write_all(&frame).unwrap();
        }

        fn recv(&mut self) -> Value {
            use std::io::Read;
            loop {
                if let Some(frame) = self.reader.next_frame() {
                    assert_eq!(
                        frame.opcode,
                        crate::transport::ws::OPCODE_TEXT,
                        "JSON-RPC travels as text"
                    );
                    return serde_json::from_slice(&frame.payload).expect("a JSON message");
                }
                let mut chunk = [0u8; 4096];
                let n = self.stream.read(&mut chunk).expect("a server reply");
                assert!(n > 0, "server closed before replying");
                self.reader.feed(&chunk[..n]);
            }
        }
    }

    #[test]
    fn test_ws_server_serves_initialize_and_a_tool_call() {
        // The same server, a different transport: one client connects,
        // handshakes, and drives it with real RFC 6455 text frames.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().unwrap();
        let session = std::thread::spawn(move || {
            let (stream, _peer) = listener.accept().expect("accept");
            serve_ws_client(stream)
        });

        let mut client = WsTestClient::connect(addr);

        client.send(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#);
        let init = client.recv();
        assert_eq!(init["id"], 1);
        assert_eq!(init["result"]["serverInfo"]["name"], "simble");
        assert_eq!(init["result"]["protocolVersion"], PROTOCOL_VERSION);

        // A tool call, over the socket, against a scene this connection owns.
        client.send(
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call",
                "params":{"name":"lookup","arguments":{"query":"0x180D"}}}"#,
        );
        let looked_up = client.recv();
        assert_eq!(looked_up["id"], 2);
        assert_eq!(looked_up["result"]["isError"], false, "{looked_up}");
        assert!(
            looked_up["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("Heart Rate"),
            "{looked_up}"
        );

        // A JSON-RPC notification gets no reply, so the next thing read is
        // the response to the request after it — not a stray empty frame.
        client.send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        client.send(r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#);
        let pong = client.recv();
        assert_eq!(pong["id"], 3);
        assert_eq!(pong["result"], json!({}));

        // Closing the socket ends the session rather than wedging the loop.
        drop(client);
        assert!(
            session.join().expect("session thread").is_err(),
            "a client disconnect is reported as the end of the session"
        );
    }
}
