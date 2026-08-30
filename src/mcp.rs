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
use crate::scene::SceneEngine;
use crate::scripting::test_script::{lint_script, run_test_script};
use crate::transport::Scene;
use crate::transport::netsim::{self, NetsimScene};
use crate::transport::usb::{UsbScene, UsbSelector, list_bluetooth_dongles};
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
type LiveBackend = Box<dyn Scene>;

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

        // Every live backend is peripheral-only for the *connection* tools:
        // something on the far side — the Android emulator, a phone in the room
        // — plays the central, so simble's central-side tools have nothing
        // in-scene to run on. `scan` is the exception: on a USB dongle it is a
        // real HCI scan of the air, which `tool_scan` handles directly.
        if let Some(live) = self.live.as_ref()
            && matches!(
                name,
                "connect"
                    | "read"
                    | "write"
                    | "assert"
                    | "subscribe"
                    | "assert_over"
                    | "add_central"
            )
        {
            let far_side = match live.name() {
                "usb" => "a real phone or laptop over real RF",
                _ => "the Android emulator (or another netsim client)",
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

    /// The dongles plugged in right now, named every way `device:` accepts.
    ///
    /// An agent cannot choose without a list, and two dongles of one model
    /// share a `vid:pid` — so `run_on("usb")` prints this whether it
    /// succeeded or not. Enumeration only reads descriptors: nothing is
    /// opened or claimed, so this is safe with no dongle plugged in and with
    /// a dongle another process is already using.
    fn dongle_listing() -> String {
        match list_bluetooth_dongles() {
            Err(e) => format!("(could not enumerate USB devices: {e})"),
            Ok(d) if d.is_empty() => "No Bluetooth-class USB dongle is plugged in. \
                 (A Mac's built-in controller is PCIe-attached and never appears here.)"
                .to_string(),
            Ok(dongles) => {
                let lines: Vec<String> = dongles
                    .iter()
                    .map(|d| {
                        format!(
                            "  {} — select it as \"#{}\", \"{}\" (bus/address) or \
                             \"{}\" (bus.port, survives a re-plug)",
                            d.describe(),
                            d.index,
                            d.address_selector(),
                            d.port_selector()
                        )
                    })
                    .collect();
                format!(
                    "Dongles plugged in ({}):\n{}",
                    dongles.len(),
                    lines.join("\n")
                )
            }
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
                    live: Some(Box::new(NetsimScene::new(netsim::DEFAULT_WS_URL)) as LiveBackend),
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
                    Some(spec) => match UsbSelector::parse(spec) {
                        Ok(selector) => selector,
                        // Reworded rather than passed through: the transport's
                        // "Transport Error:" prefix is noise to an agent that
                        // asked about an argument. The listing goes with it,
                        // because the next thing the agent needs is the set of
                        // names that would have worked.
                        Err(_) => {
                            return tool_text(
                                id,
                                &format!(
                                    "invalid device selector {spec:?} — expected \"#0\" \
                                     (index), \"0a12:0001\" (vid:pid), \"02/4\" \
                                     (bus/address), or \"02.1\" (bus.port).\n{}",
                                    Self::dongle_listing()
                                ),
                                true,
                            );
                        }
                    },
                    None => UsbSelector::First,
                };
                let scene = UsbScene::new(selected);
                let selector = scene.selector();
                *self = Server {
                    live: Some(Box::new(scene) as LiveBackend),
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
                         peripheral side.\n{}",
                        Self::dongle_listing()
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
            // The MCP tick tool is seconds-facing; the Scene trait clock is µs.
            live.tick((seconds * 1_000_000.0) as u64);
            return tool_text(
                id,
                &format!(
                    "advanced to t={:.3}s ({} device(s) on {})",
                    live.now_us() as f64 / 1_000_000.0,
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
        // On a live backend, `scan` means a real HCI scan: hear whatever is
        // actually on the medium — real devices on real RF with a USB dongle —
        // not the agent's own scene. Written when every device lived in the
        // in-process scene; a dongle-backed session wants the room, not the sim.
        if let Some(live) = self.live.as_mut() {
            if !live.has_scanner()
                && let Err(e) = live.add_scanner()
            {
                return tool_text(id, &format!("cannot start a real-RF scan: {e}"), true);
            }
            // Advertisements arrive across seconds, so pump the dongle against
            // the wall clock rather than ticking a script forward.
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(2500);
            while std::time::Instant::now() < deadline {
                live.pump();
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            let reports = live
                .scanner_reports_json()
                .unwrap_or_else(|| "[]".to_string());
            return tool_text(id, &annotate_json(&dedupe_scan_reports(&reports)), false);
        }
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
                    "device": { "type": "string", "description": "usb only: which dongle. \
                        \"#0\" picks by index in the list run_on prints; \"0a12:0001\" is a hex \
                        vid:pid and works only when exactly one device carries it (two dongles \
                        of the same model share one, and the call errors rather than guessing); \
                        \"02/4\" is bus/address; \"02.1\" is a bus.port path, the only form that \
                        still names the same dongle after a re-plug. Omit to take the first \
                        Bluetooth-class dongle found." },
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
                advertisements it hears, as a real central would. On a USB dongle (run_on \"usb\") \
                this is a real HCI scan of the air — every device in radio range, not just the \
                scene; on \"self\" it is the in-process peripherals. Answers \"scan for devices\".",
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
#[path = "mcp_tests.rs"]
mod tests;
