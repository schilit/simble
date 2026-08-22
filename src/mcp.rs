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

use crate::devices::catalog::{self, EXAMPLES};
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
    match name {
        None | Some("") => {
            let listing: String = EXAMPLES
                .iter()
                .map(|e| format!("{} — {}\n", e.name, e.summary))
                .collect();
            tool_text(
                id,
                &format!("{listing}Call example with a name to get its script."),
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
        let script = catalog::script("volume").unwrap();
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
        let script = catalog::script("thermostat").unwrap();
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
