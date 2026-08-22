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
//! Tools:
//! - `lint` / `run_test` — stateless; compile or run a script (same functions
//!   the CLI and browser Testing page use, so the surfaces can't diverge).
//! - `run_on` — choose which controller the scene runs on: `self` (in-process,
//!   deterministic), later `netsim` / `usb`. "self" is wired here.
//! - `add_peripheral` / `tick` / `status` — build and drive the live scene.
//!
//! A *scene* is the set of devices the agent has added; the controller is where
//! they run. `run_on` re-targets the controller; the devices are the agent's,
//! hosted by this process (peers on netsim / in a browser are not).

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

        // Self-only: nothing to pump between requests yet. (Live backends will
        // pump their sockets and push notifications here.) Idle briefly so an
        // otherwise-quiet loop doesn't spin a core.
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
            "assert" => self.tool_assert(id, args),
            "subscribe" => match args.and_then(|a| a.get("uuid")).and_then(Value::as_str) {
                Some(uuid) => self.tool_subscribe(id, uuid),
                None => tool_text(id, "subscribe needs a uuid argument", true),
            },
            "assert_over" => self.tool_assert_over(id, args),
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
            "netsim" | "usb" => tool_text(
                id,
                &format!("run_on \"{target}\" is not wired yet — only \"self\" for now"),
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
        tool_text(id, &format!("connected central #{index}\n{status}"), false)
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
        let body = json!({ "controller": "self", "devices": devices });
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
        tool_text(id, &reports, false)
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
                deterministic, no setup), \"netsim\" (shares the scene with the Android emulator), \
                or \"usb\" (a real dongle). Resets the scene. Only \"self\" is wired so far.",
            "inputSchema": {
                "type": "object",
                "properties": { "target": { "type": "string", "enum": ["self", "netsim", "usb"] } },
                "required": ["target"],
            },
        },
        {
            "name": "add_peripheral",
            "description": "Add a scripted peripheral to the live scene. The script must create an \
                android::BluetoothGattServer. Returns the device index; call tick then status to \
                run it. A bad script is rejected with its error.",
            "inputSchema": script_schema("A Rhai peripheral script (creates a BluetoothGattServer)."),
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
    fn test_run_on_netsim_not_wired_yet() {
        let mut s = Server::default();
        let resp = call(&mut s, "run_on", json!({"target": "netsim"}));
        assert_eq!(resp["result"]["isError"], true);
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
