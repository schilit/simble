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
}

impl Default for Server {
    fn default() -> Self {
        Self {
            scene: None,
            next_addr: 1,
            elapsed: 0.0,
            scanner: None,
        }
    }
}

/// Runs the server: one JSON-RPC message per line in, one response line per
/// request out, until stdin reaches EOF. Notifications (no `id`) get no reply.
pub fn serve_stdio() -> std::io::Result<()> {
    let mut server = Server::default();
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(()); // client closed the pipe
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(trimmed) {
            Ok(request) => server.handle(&request),
            Err(e) => Some(error_response(None, -32700, &format!("parse error: {e}"))),
        };
        if let Some(response) = response {
            serde_json::to_writer(&mut out, &response)?;
            out.write_all(b"\n")?;
            out.flush()?;
        }
    }
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
        let scene = self.scene.get_or_insert_with(SceneEngine::new);
        match scene.add_peripheral(address, script) {
            Ok(index) => {
                let status = scene
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
    ]})
}

// --- JSON-RPC / MCP response envelopes -------------------------------------

fn require_script(args: Option<&Value>) -> Result<&str, &'static str> {
    args.and_then(|a| a.get("script"))
        .and_then(Value::as_str)
        .ok_or("missing required argument: script")
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
