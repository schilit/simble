// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! A minimal Model Context Protocol server over stdio (`simble mcp`), exposing
//! SimBLE to agents as tools.
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
//!   deterministic) or `netsim` (the emulator's ether). `usb` is not wired.
//! - `add_peripheral` / `tick` / `status` / `scan` — build and drive the live
//!   scene; `status` is the god-view, `scan` is what a scanner actually hears.
//! - `connect` / `read` / `write` / `assert` — drive a central against a
//!   peripheral, naming characteristics by UUID.
//! - `subscribe` / `assert_over` — a real monitor: a condition that must hold
//!   across a window, failing on the first violating sample.
//!
//! A *scene* is the set of devices the agent has added; the controller is where
//! they run. `run_on` re-targets the controller; the devices are the agent's,
//! hosted by this process (peers on netsim / in a browser are not).

use crate::gatt::sig_names;
use crate::transport::netsim::{self, NetsimScene};
use crate::transport::wasm_ws::{SceneEngine, lint_script, run_test_script};
use crate::types::Address;
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::sync::mpsc;
use std::time::Duration;

/// The MCP revision this server implements (returned from `initialize`).
const PROTOCOL_VERSION: &str = "2024-11-05";

/// The live server: a scene on the `self` controller, plus the deterministic
/// address allocator and simulated clock it advances.
pub struct Server {
    scene: Option<SceneEngine>,
    /// The netsim-backed scene when `run_on("netsim")` selected it; the two
    /// scenes are mutually exclusive (`run_on` resets the server).
    netsim: Option<NetsimScene>,
    next_addr: u16,
    elapsed: f64,
    /// Lazily-added scanner device index, reused across `scan` calls.
    scanner: Option<usize>,
    /// Added peripherals as (device index, address) — `connect` targets one.
    peripherals: Vec<(usize, Address)>,
    /// The most recently connected central device index, driven by `read`.
    central: Option<usize>,
}

impl Default for Server {
    fn default() -> Self {
        Self {
            scene: None,
            netsim: None,
            next_addr: 1,
            elapsed: 0.0,
            scanner: None,
            peripherals: Vec::new(),
            central: None,
        }
    }
}

/// Runs the server: one JSON-RPC message per line in, one response line per
/// request out, until stdin reaches EOF. Notifications (no `id`) get no reply.
pub fn serve_stdio() -> std::io::Result<()> {
    let mut server = Server::default();

    // A tiny reader thread ferries stdin *lines* over a channel — it never
    // touches the scene, so the (non-`Send`) scripting engine stays on this
    // thread. The main loop then polls the channel without ever blocking on
    // stdin, which is what leaves room to pump live backends and push
    // notifications between requests (self-only for now).
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut reader = stdin.lock();
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break, // EOF: stdin closed
                Ok(_) => {
                    if tx.send(std::mem::take(&mut line)).is_err() {
                        break; // main loop gone
                    }
                }
                Err(_) => break,
            }
        }
    });

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    loop {
        // Drain every request that has arrived, without blocking.
        let mut idle = true;
        loop {
            match rx.try_recv() {
                Ok(line) => {
                    idle = false;
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let response = match serde_json::from_str::<Value>(trimmed) {
                        Ok(request) => server.handle(&request),
                        Err(e) => Some(error_response(None, -32700, &format!("parse error: {e}"))),
                    };
                    if let Some(response) = response {
                        write_message(&mut out, &response)?;
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return Ok(()), // stdin closed
            }
        }

        // Pump live backends between requests, so netsim peripherals answer
        // the emulator's connections and reads while no tool call is active.
        server.pump_live();
        // Idle briefly so an otherwise-quiet loop doesn't spin a core.
        if idle {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// Writes one JSON-RPC message as a single newline-delimited line — used for
/// responses now, and for server-initiated notifications once live.
fn write_message<W: Write>(out: &mut W, message: &Value) -> std::io::Result<()> {
    serde_json::to_writer(&mut *out, message)?;
    out.write_all(b"\n")?;
    out.flush()
}

impl Server {
    /// Drives the server programmatically (the non-stdio entry point): pass a
    /// JSON-RPC request `Value`, get its response, or `None` for a notification.
    /// Same dispatch [`serve_stdio`] runs per line — useful for embedding and
    /// for scenario tests that exercise the tools without a pipe.
    /// Moves packets for any live backend (netsim today) without handling a
    /// request — the actor loop calls this between requests so peripherals
    /// stay responsive to their centrals.
    pub fn pump_live(&mut self) {
        if let Some(netsim) = self.netsim.as_mut() {
            netsim.pump();
        }
    }

    /// Drives the server programmatically (the non-stdio entry point): pass a
    /// JSON-RPC request `Value`, get its response, or `None` for a notification.
    pub fn request(&mut self, request: &Value) -> Option<Value> {
        self.handle(request)
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

        // On netsim the scene is peripheral-only: the emulator (or another
        // netsim client) plays the central, so simble's central-side tools
        // have nothing in-scene to run on.
        if self.netsim.is_some()
            && matches!(
                name,
                "scan" | "connect" | "read" | "write" | "assert" | "subscribe" | "assert_over"
            )
        {
            return tool_text(
                id,
                &format!(
                    "{name} is self-mode only: on netsim the Android emulator (or another \
                     netsim client) plays the central — scan/connect from there, and use \
                     status here to watch the peripheral side"
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
                self.tool_run_on(id, target)
            }
            "add_peripheral" => match require_script(args) {
                Ok(s) => self.tool_add_peripheral(id, s),
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
                Some(uuid) => self.tool_subscribe(id, uuid),
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
                    _ => tool_text(id, "lookup needs a query (a name fragment or a 16-bit UUID)", true),
                }
            }
            other => error_response(id, -32602, &format!("unknown tool: {other}")),
        }
    }

    fn tool_run_on(&mut self, id: Option<Value>, target: &str) -> Value {
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
                    netsim: Some(NetsimScene::new(netsim::DEFAULT_WS_URL)),
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
            "usb" => tool_text(
                id,
                "run_on \"usb\" is not wired yet — \"self\" and \"netsim\" for now",
                true,
            ),
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
        if let Some(netsim) = self.netsim.as_mut() {
            return match netsim.add_peripheral(address, script) {
                Ok(index) => {
                    let status = netsim
                        .peripheral_status_json(index)
                        .unwrap_or_else(|| "{}".to_string());
                    // First pump queues HCI bring-up so the device goes on
                    // netsim's air immediately, not at the next tool call.
                    netsim.pump();
                    tool_text(
                        id,
                        &format!(
                            "added peripheral #{index} to netsim as {address} — scan for it \
                             from the emulator\n{status}"
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
            return tool_text(
                id,
                "write needs: uuid, value (array of bytes 0-255)",
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
    fn tool_subscribe(&mut self, id: Option<Value>, uuid: &str) -> Value {
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
        self.scene
            .as_mut()
            .unwrap()
            .central_subscribe(central, handle);
        self.advance(8, 0.02); // CCCD write + first notifications
        let after = self
            .scene
            .as_ref()
            .unwrap()
            .central_status_json(central)
            .unwrap_or_default();
        tool_text(
            id,
            &format!("subscribed to {uuid} (handle {handle})\n{after}"),
            false,
        )
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
    /// connect/read need, since discovery and reads span several ticks).
    fn advance(&mut self, steps: usize, dt: f64) {
        for _ in 0..steps {
            self.elapsed += dt;
            let t = self.elapsed;
            if let Some(scene) = self.scene.as_mut() {
                scene.tick(t);
            }
        }
    }

    fn tool_tick(&mut self, id: Option<Value>, seconds: f64) -> Value {
        if let Some(netsim) = self.netsim.as_mut() {
            netsim.tick(seconds);
            return tool_text(
                id,
                &format!(
                    "advanced to t={:.3}s ({} device(s) on netsim)",
                    netsim.now(),
                    netsim.device_count()
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
        tool_text(
            id,
            &format!("advanced to t={t:.3}s ({} device(s))", scene.device_count()),
            false,
        )
    }

    fn tool_status(&self, id: Option<Value>) -> Value {
        if let Some(netsim) = self.netsim.as_ref() {
            let devices: Vec<Value> = (0..netsim.device_count())
                .map(|i| match netsim.peripheral_status_json(i) {
                    Some(j) => serde_json::from_str(&j).unwrap_or(Value::String(j)),
                    None => json!({ "index": i, "role": "non-peripheral" }),
                })
                .collect();
            let mut body = json!({ "controller": "netsim", "devices": devices });
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
            .map(|i| match scene.peripheral_status_json(i) {
                Some(j) => serde_json::from_str(&j).unwrap_or(Value::String(j)),
                None => json!({ "index": i, "role": "non-peripheral" }),
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
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "simble", "version": env!("CARGO_PKG_VERSION") },
    })
}

/// Named, ready-to-run sample scripts served by the `example` tool, so an
/// agent with no repo access can learn the Rhai API from the server itself.
/// Each entry is (name, one-line description, script); every script is
/// exercised by tests (lint + add_peripheral + tick), so the samples and the
/// engine cannot drift apart. Each sample teaches a distinct idiom.
const EXAMPLES: &[(&str, &str, &str)] = &[
    (
        "hrm",
        "Heart-rate monitor (180D): named uuid consts, live values via fn tick",
        r#"// Heart Rate service with a measurement that changes over time.
let server = android::BluetoothGattServer("HRM");
let hrs = android::BluetoothGattService(uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY);
let hr = android::BluetoothGattCharacteristic(uuid::HEART_RATE_MEASUREMENT,
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
hr.set_value([0x00, 72]); // [flags, bpm]
hr.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
hrs.add_characteristic(hr);
server.add_service(hrs);

// Optional: runs on every scene tick; update_value pushes notifications
// to subscribed centrals.
fn tick(server, t) {
    let bpm = 68 + (t * 2.0).to_int() % 9;
    server.update_value(uuid::HEART_RATE_MEASUREMENT, [0x00, bpm]);
}
"#,
    ),
    (
        "thermometer",
        "Health Thermometer (1809): uuid::from_u16 for UUIDs with no named const",
        r#"// Health Thermometer service. No named const for these assigned
// numbers yet, so lift the 16-bit values with uuid::from_u16.
let server = android::BluetoothGattServer("Thermo");
let hts = android::BluetoothGattService(uuid::from_u16(0x1809), android::SERVICE_TYPE_PRIMARY);
let temp = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A1C),
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
temp.set_value([0x00, 37]); // [flags, degrees C] — byte 1 is what assert checks
temp.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
hts.add_characteristic(temp);
server.add_service(hts);

fn tick(server, t) {
    let c = 36 + t.to_int() % 3;
    server.update_value(uuid::from_u16(0x2A1C), [0x00, c]);
}
"#,
    ),
    (
        "battery",
        "Battery service (180F): the minimal static peripheral — no fn tick",
        r#"// Battery service: one static read-only value. The smallest
// complete peripheral.
let server = android::BluetoothGattServer("Batt");
let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
let level = android::BluetoothGattCharacteristic(uuid::BATTERY_LEVEL,
    android::PROPERTY_READ, android::PERMISSION_READ);
level.set_value([100]); // percent
bas.add_characteristic(level);
server.add_service(bas);
"#,
    ),
    (
        "env_sensor",
        "Environmental Sensing (181A): several characteristics on one service",
        r#"// Environmental Sensing: temperature (2A6E) and humidity (2A6F)
// on the same service.
let server = android::BluetoothGattServer("EnvSense");
let ess = android::BluetoothGattService(uuid::from_u16(0x181A), android::SERVICE_TYPE_PRIMARY);
let temp = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A6E),
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
temp.set_value([0x00, 21]); // [flags, degrees C]
temp.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
let hum = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A6F),
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
hum.set_value([0x00, 45]); // [flags, percent RH]
hum.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
ess.add_characteristic(temp);
ess.add_characteristic(hum);
server.add_service(ess);

fn tick(server, t) {
    server.update_value(uuid::from_u16(0x2A6E), [0x00, 20 + t.to_int() % 4]);
}
"#,
    ),
    (
        "volume",
        "LE Audio Volume Control (1844): a control point the phone writes to change state",
        r#"// Volume Control Service — the LE Audio profile a phone uses to set a
// speaker's volume. This is the control-point idiom: the peer WRITES a
// command opcode, and the device applies it and notifies the new state.
let server = android::BluetoothGattServer("Speaker");
let vcs = android::BluetoothGattService(uuid::VOLUME_CONTROL_SERVICE, android::SERVICE_TYPE_PRIMARY);

let state = android::BluetoothGattCharacteristic(uuid::VOLUME_STATE,
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
state.set_value([128, 0, 0]); // [volume 0-255, muted, change counter]
// A characteristic that declares NOTIFY needs a CCCD, or no real central
// can subscribe to it (Core Spec Vol 3, Part G, Section 3.3.3.3).
state.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
vcs.add_characteristic(state);

let point = android::BluetoothGattCharacteristic(uuid::VOLUME_CONTROL_POINT,
    android::PROPERTY_WRITE, android::PERMISSION_WRITE);
point.set_value([0xFF]); // 0xFF = no command pending
vcs.add_characteristic(point);

let flags = android::BluetoothGattCharacteristic(uuid::VOLUME_FLAGS,
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
flags.set_value([0x01]); // volume setting persisted
flags.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
vcs.add_characteristic(flags);
server.add_service(vcs);

// Opcodes (Volume Control Service 1.0, Table 3.3): 0x00 down, 0x01 up,
// 0x02/0x03 unmute+down/up, 0x04 set absolute, 0x05 unmute, 0x06 mute.
// A write is [opcode, change_counter] (+ volume for 0x04).
fn tick(server, t) {
    let command = server.value(uuid::VOLUME_CONTROL_POINT);
    if command.len() < 1 || command[0] == 0xFF { return; }
    let state = server.value(uuid::VOLUME_STATE);
    let volume = state[0];
    let muted = state[1];
    let op = command[0];
    if op == 0x00 || op == 0x02 { volume = if volume > 16 { volume - 16 } else { 0 }; }
    if op == 0x01 || op == 0x03 { volume = if volume < 239 { volume + 16 } else { 255 }; }
    if op == 0x02 || op == 0x03 || op == 0x05 { muted = 0; }
    if op == 0x04 && command.len() > 2 { volume = command[2]; }
    if op == 0x06 { muted = 1; }
    // The change counter increments on every state change, so a peer can
    // detect a command it raced against.
    server.update_value(uuid::VOLUME_STATE, [volume, muted, (state[2] + 1) % 256]);
    server.update_value(uuid::VOLUME_CONTROL_POINT, [0xFF]); // consumed
}
"#,
    ),
    (
        "hid_keyboard",
        "HID over GATT keyboard (1812): report map + input reports Android reads as a keyboard",
        r#"// HOGP keyboard. The Report Map (2A4B) is a USB HID report
// descriptor: it tells the host how to interpret the bytes that arrive
// on the Report characteristic, so the same 8-byte report becomes
// keystrokes. A Report Reference descriptor (2908) tags each report with
// its ID and direction, which is how a host tells inputs from outputs.
let server = android::BluetoothGattServer("SimKeyboard");
let hid = android::BluetoothGattService(uuid::from_u16(0x1812), android::SERVICE_TYPE_PRIMARY);

// bcdHID 1.11, country 0 (not localized), flags: remote wake + normally connectable.
let info = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4A),
    android::PROPERTY_READ, android::PERMISSION_READ);
info.set_value([0x11, 0x01, 0x00, 0x03]);
hid.add_characteristic(info);

let map = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4B),
    android::PROPERTY_READ, android::PERMISSION_READ);
map.set_value([
    0x05, 0x01,       // Usage Page (Generic Desktop)
    0x09, 0x06,       // Usage (Keyboard)
    0xA1, 0x01,       // Collection (Application)
    0x05, 0x07,       //   Usage Page (Keyboard/Keypad)
    0x19, 0xE0, 0x29, 0xE7, // Usage Min/Max (modifier keys)
    0x15, 0x00, 0x25, 0x01, // Logical 0..1
    0x75, 0x01, 0x95, 0x08, // 8 x 1-bit
    0x81, 0x02,       //   Input (Data,Var,Abs) — modifier byte
    0x95, 0x01, 0x75, 0x08,
    0x81, 0x01,       //   Input (Const) — reserved byte
    0x95, 0x06, 0x75, 0x08, // 6 x 8-bit
    0x15, 0x00, 0x25, 0x65, // Logical 0..101
    0x05, 0x07, 0x19, 0x00, 0x29, 0x65,
    0x81, 0x00,       //   Input (Data,Array) — the 6 key slots
    0xC0,             // End Collection
]);
hid.add_characteristic(map);

// The input report: [modifiers, reserved, key1..key6].
let report = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4D),
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
report.set_value([0, 0, 0, 0, 0, 0, 0, 0]);
report.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
// Report Reference: report ID 1, type 1 (Input).
let reference = android::BluetoothGattDescriptor(uuid::from_u16(0x2908),
    android::PERMISSION_READ);
reference.set_value([0x01, 0x01]);
report.add_descriptor(reference);
hid.add_characteristic(report);

// Protocol Mode: 1 = Report (0 would be Boot).
let mode = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4E),
    android::PROPERTY_READ | android::PROPERTY_WRITE_NO_RESPONSE,
    android::PERMISSION_READ | android::PERMISSION_WRITE);
mode.set_value([0x01]);
hid.add_characteristic(mode);

// HID Control Point: the host writes 0x00 (suspend) / 0x01 (exit suspend).
let control = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4C),
    android::PROPERTY_WRITE_NO_RESPONSE, android::PERMISSION_WRITE);
hid.add_characteristic(control);
server.add_service(hid);

// Types "hello" on a loop: press a key, then release it. A real keyboard
// sends the same two reports — a key is held until an empty report.
fn tick(server, t) {
    let keys = [0x0B, 0x08, 0x0F, 0x0F, 0x12]; // HID usage codes: h e l l o
    let step = (t * 2.0).to_int();
    let slot = (step / 2) % 5;
    let key = keys[slot];
    if step % 2 == 1 { key = 0; } // the release report
    server.update_value(uuid::from_u16(0x2A4D), [0, 0, key, 0, 0, 0, 0, 0]);
}
"#,
    ),
    (
        "hid_mouse",
        "HID over GATT mouse (1812): relative-motion reports, buttons + X/Y",
        r#"// HOGP mouse — same shape as the keyboard, different report map.
// The report is [buttons, dx, dy, wheel] with dx/dy/wheel as SIGNED
// relative motion, which is why the descriptor declares Logical Minimum
// -127: read as unsigned, one step left becomes 255 steps right.
let server = android::BluetoothGattServer("SimMouse");
let hid = android::BluetoothGattService(uuid::from_u16(0x1812), android::SERVICE_TYPE_PRIMARY);

let info = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4A),
    android::PROPERTY_READ, android::PERMISSION_READ);
info.set_value([0x11, 0x01, 0x00, 0x03]);
hid.add_characteristic(info);

let map = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4B),
    android::PROPERTY_READ, android::PERMISSION_READ);
map.set_value([
    0x05, 0x01,       // Usage Page (Generic Desktop)
    0x09, 0x02,       // Usage (Mouse)
    0xA1, 0x01,       // Collection (Application)
    0x09, 0x01,       //   Usage (Pointer)
    0xA1, 0x00,       //   Collection (Physical)
    0x05, 0x09,       //     Usage Page (Button)
    0x19, 0x01, 0x29, 0x03, //   Buttons 1..3
    0x15, 0x00, 0x25, 0x01,
    0x95, 0x03, 0x75, 0x01,
    0x81, 0x02,       //     Input (Data,Var,Abs) — 3 button bits
    0x95, 0x01, 0x75, 0x05,
    0x81, 0x01,       //     Input (Const) — 5 bits padding
    0x05, 0x01,       //     Usage Page (Generic Desktop)
    0x09, 0x30, 0x09, 0x31, //   Usage X, Y
    0x09, 0x38,       //     Usage Wheel
    0x15, 0x81, 0x25, 0x7F, //   Logical -127..127
    0x75, 0x08, 0x95, 0x03,
    0x81, 0x06,       //     Input (Data,Var,Rel) — relative motion
    0xC0, 0xC0,       // End Collection x2
]);
hid.add_characteristic(map);

let report = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4D),
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
report.set_value([0, 0, 0, 0]); // [buttons, dx, dy, wheel]
report.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
let reference = android::BluetoothGattDescriptor(uuid::from_u16(0x2908),
    android::PERMISSION_READ);
reference.set_value([0x01, 0x01]);
report.add_descriptor(reference);
hid.add_characteristic(report);

let mode = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4E),
    android::PROPERTY_READ | android::PROPERTY_WRITE_NO_RESPONSE,
    android::PERMISSION_READ | android::PERMISSION_WRITE);
mode.set_value([0x01]);
hid.add_characteristic(mode);
server.add_service(hid);

// Walks the pointer around a square: four headings, 3 seconds each.
fn tick(server, t) {
    let leg = (t / 3.0).to_int() % 4;
    let dx = 0;
    let dy = 0;
    if leg == 0 { dx = 5; }
    if leg == 1 { dy = 5; }
    if leg == 2 { dx = 251; } // -5 as a signed byte
    if leg == 3 { dy = 251; }
    server.update_value(uuid::from_u16(0x2A4D), [0, dx, dy, 0]);
}
"#,
    ),
    (
        "gamepad",
        "HID over GATT game controller (1812): two analog axes + 8 buttons",
        r#"// HOGP game controller. Note most console pads (Xbox, DualSense)
// pair over CLASSIC HID, not this — but LE gamepads use exactly this
// profile, and the report map is what makes Android map the axes.
let server = android::BluetoothGattServer("SimGamepad");
let hid = android::BluetoothGattService(uuid::from_u16(0x1812), android::SERVICE_TYPE_PRIMARY);

let info = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4A),
    android::PROPERTY_READ, android::PERMISSION_READ);
info.set_value([0x11, 0x01, 0x00, 0x03]);
hid.add_characteristic(info);

let map = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4B),
    android::PROPERTY_READ, android::PERMISSION_READ);
map.set_value([
    0x05, 0x01,       // Usage Page (Generic Desktop)
    0x09, 0x05,       // Usage (Game Pad)
    0xA1, 0x01,       // Collection (Application)
    0x09, 0x01,       //   Usage (Pointer)
    0xA1, 0x00,       //   Collection (Physical)
    0x09, 0x30, 0x09, 0x31, //   Usage X, Y — the left stick
    0x15, 0x81, 0x25, 0x7F, //   Logical -127..127
    0x75, 0x08, 0x95, 0x02,
    0x81, 0x02,       //     Input (Data,Var,Abs) — absolute stick position
    0xC0,             //   End Collection
    0x05, 0x09,       //   Usage Page (Button)
    0x19, 0x01, 0x29, 0x08, // Buttons 1..8
    0x15, 0x00, 0x25, 0x01,
    0x75, 0x01, 0x95, 0x08,
    0x81, 0x02,       //   Input (Data,Var,Abs) — 8 button bits
    0xC0,             // End Collection
]);
hid.add_characteristic(map);

let report = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4D),
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
report.set_value([0, 0, 0]); // [x, y, buttons]
report.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
let reference = android::BluetoothGattDescriptor(uuid::from_u16(0x2908),
    android::PERMISSION_READ);
reference.set_value([0x01, 0x01]);
report.add_descriptor(reference);
hid.add_characteristic(report);

let mode = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4E),
    android::PROPERTY_READ | android::PROPERTY_WRITE_NO_RESPONSE,
    android::PERMISSION_READ | android::PERMISSION_WRITE);
mode.set_value([0x01]);
hid.add_characteristic(mode);
server.add_service(hid);

// Sweeps the stick and cycles one button at a time.
fn tick(server, t) {
    let step = t.to_int();
    let x = (step * 16) % 127;
    let y = (step * 8) % 127;
    let buttons = 1 << (step % 8);
    server.update_value(uuid::from_u16(0x2A4D), [x, y, buttons]);
}
"#,
    ),
    (
        "cycling",
        "Cycling Speed and Cadence (1816): cumulative counters a phone differentiates into speed",
        r#"// CSCS sensor. The measurement carries CUMULATIVE revolution counts
// plus the time of the last event (1/1024 s units) — the phone computes
// speed and cadence by differentiating between notifications, which is
// why a counter that only ever increases is the right model.
let server = android::BluetoothGattServer("CadenceSensor");
let cscs = android::BluetoothGattService(uuid::from_u16(0x1816), android::SERVICE_TYPE_PRIMARY);

// Flags bit0 = wheel data present, bit1 = crank data present.
let measurement = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A5B),
    android::PROPERTY_NOTIFY, android::PERMISSION_READ);
measurement.set_value([0x03, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
measurement.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
cscs.add_characteristic(measurement);

// CSC Feature: wheel + crank revolution data supported.
let feature = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A5C),
    android::PROPERTY_READ, android::PERMISSION_READ);
feature.set_value([0x03, 0x00]);
cscs.add_characteristic(feature);

// Sensor Location: 5 = left crank.
let location = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A5D),
    android::PROPERTY_READ, android::PERMISSION_READ);
location.set_value([0x05]);
cscs.add_characteristic(location);

// SC Control Point — this is where a phone resets the odometer.
let control = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A55),
    android::PROPERTY_WRITE | android::PROPERTY_INDICATE,
    android::PERMISSION_WRITE);
// CSCS 1.1, 3.3: the SC Control Point indicates its result, so it needs a
// CCCD for the client to enable those indications.
control.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
cscs.add_characteristic(control);
server.add_service(cscs);

// ~1 wheel rev/s (roughly 8 km/h on a 700c wheel) and 80 rpm cranks.
fn tick(server, t) {
    let seconds = t.to_int();
    let wheel = seconds;
    let crank = (seconds * 4) / 3;
    let event = (seconds * 1024) % 65536;
    let w0 = wheel & 0xFF;
    let w1 = (wheel >> 8) & 0xFF;
    let c0 = crank & 0xFF;
    let c1 = (crank >> 8) & 0xFF;
    let e0 = event & 0xFF;
    let e1 = (event >> 8) & 0xFF;
    server.update_value(uuid::from_u16(0x2A5B),
        [0x03, w0, w1, 0, 0, e0, e1, c0, c1, e0, e1]);
}
"#,
    ),
    (
        "pulse_oximeter",
        "Pulse Oximeter (1822): SpO2 + pulse rate as IEEE-11073 SFLOATs",
        r#"// PLXS continuous measurement. Values are SFLOATs: 16 bits split as a
// 4-bit signed exponent and a 12-bit signed mantissa. With exponent 0 the
// mantissa is just the integer, so 98% SpO2 is 0x0062 little-endian —
// which is why these look like plain numbers below.
let server = android::BluetoothGattServer("PulseOx");
let plxs = android::BluetoothGattService(uuid::from_u16(0x1822), android::SERVICE_TYPE_PRIMARY);

// Continuous Measurement: flags 0x00 (no extra fields) + SpO2 + pulse rate.
let continuous = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A5F),
    android::PROPERTY_NOTIFY, android::PERMISSION_READ);
continuous.set_value([0x00, 0x62, 0x00, 0x3E, 0x00]);
continuous.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
plxs.add_characteristic(continuous);

// Spot-check Measurement is indicated, not notified: it is a single
// reading the collector must acknowledge.
let spot = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A5E),
    android::PROPERTY_INDICATE, android::PERMISSION_READ);
spot.set_value([0x00, 0x62, 0x00, 0x3E, 0x00]);
spot.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
plxs.add_characteristic(spot);

let features = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A60),
    android::PROPERTY_READ, android::PERMISSION_READ);
features.set_value([0x00, 0x00]);
plxs.add_characteristic(features);
server.add_service(plxs);

// SpO2 drifts 96-99%, pulse 58-70 bpm.
fn tick(server, t) {
    let step = t.to_int();
    let spo2 = 96 + (step % 4);
    let pulse = 58 + (step % 13);
    server.update_value(uuid::from_u16(0x2A5F), [0x00, spo2, 0x00, pulse, 0x00]);
}
"#,
    ),
    (
        "weight_scale",
        "Smart scale: Weight Scale (181D) + Body Composition (181B) measurements",
        r#"// A smart scale exposes two services: Weight Scale for the raw mass
// and Body Composition for the derived numbers. Both measurements are
// INDICATED rather than notified — a weigh-in is a record the phone must
// acknowledge, not a stream it can miss.
let server = android::BluetoothGattServer("SmartScale");

let wss = android::BluetoothGattService(uuid::from_u16(0x181D), android::SERVICE_TYPE_PRIMARY);
// Flags 0x00 = SI units; weight is uint16 in 5 g steps, so 74.5 kg = 14900.
let weight = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A9D),
    android::PROPERTY_INDICATE, android::PERMISSION_READ);
weight.set_value([0x00, 0x34, 0x3A]);
weight.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
wss.add_characteristic(weight);

let wss_feature = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A9E),
    android::PROPERTY_READ, android::PERMISSION_READ);
wss_feature.set_value([0x00, 0x00, 0x00, 0x0C]); // 5 g mass resolution
wss.add_characteristic(wss_feature);
server.add_service(wss);

let bcs = android::BluetoothGattService(uuid::from_u16(0x181B), android::SERVICE_TYPE_PRIMARY);
// Flags 0x00 (SI) + body fat percentage in 0.1% steps: 182 = 18.2%.
let composition = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A9C),
    android::PROPERTY_INDICATE, android::PERMISSION_READ);
composition.set_value([0x00, 0x00, 0xB6, 0x00]);
composition.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
bcs.add_characteristic(composition);

let bcs_feature = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A9B),
    android::PROPERTY_READ, android::PERMISSION_READ);
bcs_feature.set_value([0x02, 0x00, 0x00, 0x00]); // body fat supported
bcs.add_characteristic(bcs_feature);
server.add_service(bcs);

// A step-on wobble that settles: real scales average a noisy load cell.
fn tick(server, t) {
    let wobble = (t * 3.0).to_int() % 7;
    let grams = 14900 + wobble * 5;
    let lo = grams & 0xFF;
    let hi = (grams >> 8) & 0xFF;
    server.update_value(uuid::from_u16(0x2A9D), [0x00, lo, hi]);
}
"#,
    ),
    (
        "smart_lock",
        "Smart lock: a custom control point that locks/unlocks, with state notifications",
        r#"// A BLE smart lock. No SIG profile covers locks, so real products use
// a vendor service — the shape is always the control-point idiom: the
// phone WRITES a command, the lock applies it and notifies the new state.
// Nothing here trusts the writer; a real lock authenticates first (see
// the pairing/bonding path) before honouring a command.
let server = android::BluetoothGattServer("SmartLock");
let svc = android::BluetoothGattService(
    uuid::of("d3a70001-1f8a-4b2c-9a11-000000000001"), android::SERVICE_TYPE_PRIMARY);

// 0 = unlocked, 1 = locked, 2 = jammed.
let state = android::BluetoothGattCharacteristic(
    uuid::of("d3a70002-1f8a-4b2c-9a11-000000000001"),
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
state.set_value([0x01]);
state.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
svc.add_characteristic(state);

// Commands: 0x01 lock, 0x02 unlock. 0xFF means "no command pending".
let control = android::BluetoothGattCharacteristic(
    uuid::of("d3a70003-1f8a-4b2c-9a11-000000000001"),
    android::PROPERTY_WRITE, android::PERMISSION_WRITE);
control.set_value([0xFF]);
svc.add_characteristic(control);

// Battery, because every lock's real failure mode is a dead battery.
let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
let level = android::BluetoothGattCharacteristic(uuid::BATTERY_LEVEL,
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
level.set_value([72]);
bas.add_characteristic(level);
server.add_service(bas);
server.add_service(svc);

fn tick(server, t) {
    let command = server.value(uuid::of("d3a70003-1f8a-4b2c-9a11-000000000001"));
    if command.len() < 1 || command[0] == 0xFF { return; }
    let op = command[0];
    if op == 0x01 {
        server.update_value(uuid::of("d3a70002-1f8a-4b2c-9a11-000000000001"), [0x01]);
    }
    if op == 0x02 {
        server.update_value(uuid::of("d3a70002-1f8a-4b2c-9a11-000000000001"), [0x00]);
    }
    // Consume the command so the next write is seen as new.
    server.update_value(uuid::of("d3a70003-1f8a-4b2c-9a11-000000000001"), [0xFF]);
}
"#,
    ),
    (
        "fitness_tracker",
        "Smartwatch / band: several services on one device (heart rate, battery, steps)",
        r#"// A wearable is not one profile — it is a handful of services on one
// GATT server, which is what makes it a useful shape to copy: standard
// services where they exist (Heart Rate, Battery, Device Information)
// and a vendor service for everything the SIG never standardised (steps).
let server = android::BluetoothGattServer("FitBand");

let hrs = android::BluetoothGattService(uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY);
let hr = android::BluetoothGattCharacteristic(uuid::HEART_RATE_MEASUREMENT,
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
hr.set_value([0x00, 64]);
hr.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
hrs.add_characteristic(hr);
let location = android::BluetoothGattCharacteristic(uuid::BODY_SENSOR_LOCATION,
    android::PROPERTY_READ, android::PERMISSION_READ);
location.set_value([0x02]); // wrist
hrs.add_characteristic(location);
server.add_service(hrs);

let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
let level = android::BluetoothGattCharacteristic(uuid::BATTERY_LEVEL,
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
level.set_value([84]);
bas.add_characteristic(level);
server.add_service(bas);

// Device Information — how a phone labels the device in its UI.
let dis = android::BluetoothGattService(uuid::from_u16(0x180A), android::SERVICE_TYPE_PRIMARY);
let manufacturer = android::BluetoothGattCharacteristic(uuid::MANUFACTURER_NAME,
    android::PROPERTY_READ, android::PERMISSION_READ);
manufacturer.set_value([0x53, 0x69, 0x6D, 0x42, 0x4C, 0x45]); // "SimBLE"
dis.add_characteristic(manufacturer);
let model = android::BluetoothGattCharacteristic(uuid::MODEL_NUMBER,
    android::PROPERTY_READ, android::PERMISSION_READ);
model.set_value([0x42, 0x61, 0x6E, 0x64, 0x20, 0x31]); // "Band 1"
dis.add_characteristic(model);
server.add_service(dis);

// Vendor step counter: a 32-bit cumulative count, like the cycling sensor.
let steps_svc = android::BluetoothGattService(
    uuid::of("f1e20001-8c3d-4a5b-9e6f-000000000001"), android::SERVICE_TYPE_PRIMARY);
let steps = android::BluetoothGattCharacteristic(
    uuid::of("f1e20002-8c3d-4a5b-9e6f-000000000001"),
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
steps.set_value([0, 0, 0, 0]);
steps.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
steps_svc.add_characteristic(steps);
server.add_service(steps_svc);

// Heart rate wanders with activity; steps accumulate about 2 per second.
fn tick(server, t) {
    let seconds = t.to_int();
    let bpm = 64 + (seconds * 3) % 40;
    server.update_value(uuid::HEART_RATE_MEASUREMENT, [0x00, bpm]);
    let count = seconds * 2;
    let b0 = count & 0xFF;
    let b1 = (count >> 8) & 0xFF;
    let b2 = (count >> 16) & 0xFF;
    server.update_value(uuid::of("f1e20002-8c3d-4a5b-9e6f-000000000001"), [b0, b1, b2, 0]);
}
"#,
    ),
    (
        "eddystone",
        "Eddystone-UID beacon (FEAA): Google's open beacon format, broadcast-only",
        r#"// Eddystone-UID: service data on 0xFEAA carrying a frame type, a
// calibrated TX power, and a 16-byte ID split into a 10-byte namespace
// (the operator) and a 6-byte instance (which beacon). Compare the
// fast_pair example — same advertising mechanism, different payload.
let server = android::BluetoothGattServer("Eddystone");
let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
let level = android::BluetoothGattCharacteristic(uuid::BATTERY_LEVEL,
    android::PROPERTY_READ, android::PERMISSION_READ);
level.set_value([95]);
bas.add_characteristic(level);
server.add_service(bas);

server.advertise_service_data(0xFEAA, [
    0x00,             // frame type: UID
    0xEB,             // ranging data: RSSI at 0 m, -21 dBm
    0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, // namespace
    0x00, 0x00, 0x00, 0x00, 0x00, 0x01, // instance
    0x00, 0x00,       // reserved
]);
server.advertise_connectable(false); // beacons broadcast, they do not connect
"#,
    ),
    (
        "ranging",
        "Channel Sounding responder (185B): distance estimates over the Ranging Service",
        r#"// Channel Sounding responder — a Bluetooth 6.0 ranging tag, the
// thing a phone measures its distance to (finder tags, car keys, "where
// did I leave it" trackers).
//
// The distance measurement itself is a CONTROLLER procedure: the two
// radios exchange tones and phase, and the host never sees the RF. What a
// phone talks to is this GATT service, which publishes the results — so
// this device models the reachable half, with `tick` standing in for the
// procedure's output.
let server = android::BluetoothGattServer("Ranger");
server.add_ras(); // Ranging Features, Real-Time Data, Control Point
server.advertise_service_uuid(0x185B);

// Real-Time Ranging Data is [f32 distance_metres, f32 confidence], little
// endian — the encoding RangingService::encode_ranging_data produces.
fn tick(server, t) {
    // A tag drifting slowly between 1 and 5 metres.
    let phase = (t / 4.0) % 2.0;
    let metres = if phase < 1.0 { 1.0 + phase * 4.0 } else { 5.0 - (phase - 1.0) * 4.0 };
    server.update_value(uuid::RANGING_REALTIME_DATA, f32_le(metres) + f32_le(0.87));
}
"#,
    ),
    (
        "ranging_tag",
        "Channel Sounding finder tag: ranging + battery, non-connectable until found",
        r#"// A finder tag (car key, luggage tag, "where are my keys"): the
// device a phone ranges to. Same Ranging Service as `ranging`, plus the
// battery every real tag exposes and a name a phone will show.
//
// Pair this with `ranging` to have two ranging devices on the air at once
// — a phone measuring distance to several tags is the actual use case.
let server = android::BluetoothGattServer("FinderTag");
server.add_ras();

let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
let level = android::BluetoothGattCharacteristic(uuid::BATTERY_LEVEL,
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
level.set_value([92]);
level.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
bas.add_characteristic(level);
server.add_service(bas);

server.advertise_service_uuid(0x185B);

fn tick(server, t) {
    // Held still, so the estimate jitters around 2.4 m the way a real
    // phase-based measurement does rather than sitting perfectly still.
    let jitter = ((t * 3.0).to_int() % 7).to_float() / 100.0;
    server.update_value(uuid::RANGING_REALTIME_DATA, f32_le(2.4 + jitter) + f32_le(0.91));
}
"#,
    ),
    (
        "fast_pair",
        "Fast Pair beacon (FE2C): custom advertising payload — service data + manufacturer data",
        r#"// Fast Pair beacon: what makes Android pop the pairing sheet. The
// identity lives in the ADVERTISEMENT, not the GATT — service data on
// the Fast Pair UUID (FE2C) carrying a 3-byte Model ID. The same
// advertise_* calls build any beacon (Eddystone, Quick Share nudge).
let server = android::BluetoothGattServer("FastPairBeacon");
let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
let level = android::BluetoothGattCharacteristic(uuid::BATTERY_LEVEL,
    android::PROPERTY_READ, android::PERMISSION_READ);
level.set_value([88]); // percent
bas.add_characteristic(level);
server.add_service(bas);

server.advertise_service_data(0xFE2C, [0x00, 0x11, 0x22]); // Model ID
server.advertise_manufacturer_data(0x00E0, [0x01]); // 0x00E0 = Google
server.advertise_connectable(false); // a real beacon is broadcast-only
"#,
    ),
    (
        "thermostat",
        "Settable device: custom 128-bit writable setpoint + convergence physics",
        r#"// Thermostat: the SIG has no thermostat service, so like real BLE
// thermostats this pairs standard Environmental Sensing temperature
// (read/notify) with a custom 128-bit writable setpoint. Set it from a
// central with the write tool; the room then drifts toward the target.
let server = android::BluetoothGattServer("Thermostat");

let ess = android::BluetoothGattService(uuid::from_u16(0x181A), android::SERVICE_TYPE_PRIMARY);
let temp = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A6E),
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
temp.set_value([0x00, 18]); // [flags, degrees C]
temp.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
ess.add_characteristic(temp);
server.add_service(ess);

let ctl = android::BluetoothGattService(uuid::of("5e7b0001-c0de-4a11-b1e5-0000c0ffee01"),
    android::SERVICE_TYPE_PRIMARY);
let setpoint = android::BluetoothGattCharacteristic(uuid::of("5e7b0002-c0de-4a11-b1e5-0000c0ffee01"),
    android::PROPERTY_READ | android::PROPERTY_WRITE,
    android::PERMISSION_READ | android::PERMISSION_WRITE);
setpoint.set_value([21]); // target degrees C — a central write replaces this
ctl.add_characteristic(setpoint);
server.add_service(ctl);

// fn tick keeps no variables between calls — the GATT database is the
// device's state. server.value(uuid) reads it back, central writes included.
fn tick(server, t) {
    let target = server.value(uuid::of("5e7b0002-c0de-4a11-b1e5-0000c0ffee01"))[0];
    let current = server.value(uuid::from_u16(0x2A6E))[1];
    if current < target { current += 1; }
    if current > target { current -= 1; }
    server.update_value(uuid::from_u16(0x2A6E), [0x00, current]);
}
"#,
    ),
];

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
                dongle; not wired yet). Resets the scene.",
            "inputSchema": {
                "type": "object",
                "properties": { "target": { "type": "string", "enum": ["self", "netsim", "usb"] } },
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
                connected central, so the peripheral's fn tick value changes push to it. Call \
                connect first.",
            "inputSchema": {
                "type": "object",
                "properties": { "uuid": { "type": "string" } },
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
            Some((kind, name)) => {
                tool_text(id, &format!("0x{uuid16:04X} {kind} — {name}"), false)
            }
            None => tool_text(
                id,
                &format!("0x{uuid16:04X} has no SIG-assigned service/characteristic/descriptor name"),
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
                    .map(|u| u.as_str().and_then(name_for).map_or(Value::Null, |n| json!(n)))
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
    let names = || {
        EXAMPLES
            .iter()
            .map(|(n, _, _)| *n)
            .collect::<Vec<_>>()
            .join(", ")
    };
    match name {
        None | Some("") => {
            let listing: String = EXAMPLES
                .iter()
                .map(|(n, d, _)| format!("{n} — {d}\n"))
                .collect();
            tool_text(
                id,
                &format!("{listing}Call example with a name to get its script."),
                false,
            )
        }
        Some(query) => match EXAMPLES.iter().find(|(n, _, _)| *n == query) {
            Some((_, _, script)) => tool_text(id, script, false),
            None => tool_text(
                id,
                &format!("unknown example {query:?} (have: {})", names()),
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
        for (name, _, _) in EXAMPLES {
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
        assert!(text.contains("0x1809 service — Health Thermometer"), "{text}");

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
        for (name, _, script) in EXAMPLES {
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
        let script = EXAMPLES
            .iter()
            .find(|(n, _, _)| *n == name)
            .unwrap_or_else(|| panic!("no example named {name}"))
            .2;
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
        let locked = call(&mut s, "assert", json!({"uuid": STATE, "op": "==", "value": 1, "byte": 0}));
        assert_eq!(locked["result"]["isError"], false, "{locked}");

        // 0x02 = unlock.
        call(&mut s, "write", json!({"uuid": CONTROL, "value": [0x02]}));
        call(&mut s, "tick", json!({"seconds": 0.2}));
        let unlocked = call(&mut s, "assert", json!({"uuid": STATE, "op": "==", "value": 0, "byte": 0}));
        assert_eq!(unlocked["result"]["isError"], false, "{unlocked}");

        // The command is consumed, so the state holds until the next write.
        call(&mut s, "tick", json!({"seconds": 0.4}));
        let still = call(&mut s, "assert", json!({"uuid": STATE, "op": "==", "value": 0, "byte": 0}));
        assert_eq!(still["result"]["isError"], false, "{still}");

        // 0x01 = lock again.
        call(&mut s, "write", json!({"uuid": CONTROL, "value": [0x01]}));
        call(&mut s, "tick", json!({"seconds": 0.2}));
        let relocked = call(&mut s, "assert", json!({"uuid": STATE, "op": "==", "value": 1, "byte": 0}));
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
        assert!(
            keys_seen.contains(&0),
            "and released again: {keys_seen:?}"
        );

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
            let script = EXAMPLES.iter().find(|(n, _, _)| *n == name).unwrap().2;
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
            let script = EXAMPLES.iter().find(|(n, _, _)| *n == name).unwrap().2;
            let mut s = Server::default();
            let added = call(&mut s, "add_peripheral", json!({"script": script}));
            assert_eq!(added["result"]["isError"], false, "{name}: {added}");
            assert_eq!(call(&mut s, "connect", json!({}))["result"]["isError"], false);

            // Real-Time Ranging Data is [f32 metres, f32 confidence] LE.
            call(&mut s, "tick", json!({"seconds": 1.0}));
            let read = call(&mut s, "read", json!({"uuid": "2B70"}));
            assert_eq!(read["result"]["isError"], false, "{name}: {read}");
            let text = read["result"]["content"][0]["text"].as_str().unwrap();
            let value = text
                .split("\"2B70\"")
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
        let script = EXAMPLES.iter().find(|(n, _, _)| *n == "volume").unwrap().2;
        let mut s = Server::default();
        call(&mut s, "add_peripheral", json!({"script": script}));
        assert_eq!(call(&mut s, "connect", json!({}))["result"]["isError"], false);

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
        call(&mut s, "write", json!({"uuid": "2B7E", "value": [0x00, 0x01]}));
        call(&mut s, "tick", json!({"seconds": 0.2}));
        let stepped = call(
            &mut s,
            "assert",
            json!({"uuid": "2B7D", "op": "==", "value": 184, "byte": 0}),
        );
        assert_eq!(stepped["result"]["isError"], false, "{stepped}");

        // Mute (0x06) sets the mute byte without touching the volume.
        call(&mut s, "write", json!({"uuid": "2B7E", "value": [0x06, 0x02]}));
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
        let script = EXAMPLES
            .iter()
            .find(|(n, _, _)| *n == "thermostat")
            .unwrap()
            .2;
        let mut s = Server::default();
        call(&mut s, "add_peripheral", json!({"script": script}));
        assert_eq!(call(&mut s, "connect", json!({}))["result"]["isError"], false);

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

        let usb = call(&mut s, "run_on", json!({"target": "usb"}));
        assert_eq!(usb["result"]["isError"], true);
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
}
