// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! A minimal Model Context Protocol server over stdio (`simble mcp`), exposing
//! SimBLE to agents as tools.
//!
//! MCP is **JSON-RPC 2.0**, newline-delimited over stdio — not gRPC — so this
//! needs only `serde_json` (already a dependency) and `std::io`; no tonic, no
//! protobuf. Unlike the one-shot CLI, an MCP server registers once and stays
//! alive, so it can (in later versions) hold a live scene across tool calls.
//!
//! v1 ships two stateless tools, both wrapping the exact functions the CLI and
//! browser Testing page use, so the three surfaces can't diverge:
//! - `lint` — compile a script without running it (fast, side-effect-free).
//! - `run_test` — run it and report whether every `assert(...)` held.
//!
//! The stateful scene tools (`new_scene` with a `built-in` / `websocket` / `usb`
//! controller, then `add_peripheral` / `connect` / `assert …`) build on this.

use crate::transport::wasm_ws::{lint_script, run_test_script};
use serde_json::{Value, json};
use std::io::{BufRead, Write};

/// The MCP revision this server implements (returned from `initialize`).
const PROTOCOL_VERSION: &str = "2024-11-05";

/// Runs the server: one JSON-RPC message per line in, one response line per
/// request out, until stdin reaches EOF. Notifications (no `id`) get no reply.
pub fn serve_stdio() -> std::io::Result<()> {
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
            Ok(request) => handle(&request),
            Err(e) => Some(error_response(None, -32700, &format!("parse error: {e}"))),
        };
        if let Some(response) = response {
            serde_json::to_writer(&mut out, &response)?;
            out.write_all(b"\n")?;
            out.flush()?;
        }
    }
}

/// Dispatches one JSON-RPC request. Returns `Some(response)` for requests and
/// `None` for notifications (a message with no `id` is never answered).
fn handle(request: &Value) -> Option<Value> {
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let id = request.get("id").cloned();
    id.as_ref()?; // notification: no id, no response

    Some(match method {
        "initialize" => result_response(id, initialize_result()),
        "tools/list" => result_response(id, tools_list()),
        "tools/call" => tools_call(id, request.get("params")),
        "ping" => result_response(id, json!({})),
        other => error_response(id, -32601, &format!("method not found: {other}")),
    })
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "simble", "version": env!("CARGO_PKG_VERSION") },
    })
}

fn tools_list() -> Value {
    json!({ "tools": [
        {
            "name": "lint",
            "description": "Compile a SimBLE Rhai device/test script WITHOUT running it. \
                Reports a syntax/parse error with its position, or that it compiles cleanly. \
                Side-effect-free — use it as a fast pre-flight before run_test.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "script": { "type": "string", "description": "The Rhai script source." }
                },
                "required": ["script"],
            },
        },
        {
            "name": "run_test",
            "description": "Run a SimBLE Rhai script in a fresh, deterministic in-process engine \
                (no radio, no netsim) and report whether every assert(...) held. A device is a \
                script; add assert(cond, \"message\") and the same script is a test.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "script": { "type": "string", "description": "The Rhai script source." }
                },
                "required": ["script"],
            },
        },
    ]})
}

fn tools_call(id: Option<Value>, params: Option<&Value>) -> Value {
    let Some(params) = params else {
        return error_response(id, -32602, "missing params");
    };
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let script = params
        .get("arguments")
        .and_then(|a| a.get("script"))
        .and_then(Value::as_str);
    let Some(script) = script else {
        return tool_text(id, "missing required argument: script", true);
    };

    let (text, is_error) = match name {
        "lint" => match lint_script(script) {
            Ok(()) => ("OK — compiles cleanly".to_string(), false),
            Err(e) => (format!("lint error: {e}"), true),
        },
        "run_test" => match run_test_script(script) {
            Ok(()) => ("PASS — all assertions held".to_string(), false),
            Err(e) => (format!("FAIL — {e}"), true),
        },
        other => return error_response(id, -32602, &format!("unknown tool: {other}")),
    };
    tool_text(id, &text, is_error)
}

// --- JSON-RPC / MCP response envelopes -------------------------------------

fn result_response(id: Option<Value>, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_response(id: Option<Value>, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// A `tools/call` result: a single text content block plus the `isError` flag
/// an agent uses to notice a failing test or a bad script.
fn tool_text(id: Option<Value>, text: &str, is_error: bool) -> Value {
    result_response(
        id,
        json!({ "content": [{ "type": "text", "text": text }], "isError": is_error }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, script: &str) -> Value {
        handle(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": { "script": script } },
        }))
        .unwrap()
    }

    #[test]
    fn test_initialize_advertises_server_and_tools() {
        let resp = handle(&json!({"jsonrpc":"2.0","id":1,"method":"initialize"})).unwrap();
        assert_eq!(resp["result"]["serverInfo"]["name"], "simble");
        assert_eq!(resp["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert!(resp["result"]["capabilities"]["tools"].is_object());
    }

    #[test]
    fn test_tools_list_has_lint_and_run_test() {
        let resp = handle(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})).unwrap();
        let names: Vec<&str> = resp["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"lint"));
        assert!(names.contains(&"run_test"));
    }

    #[test]
    fn test_run_test_pass_and_fail() {
        let pass = call(
            "run_test",
            r#"let s = android::BluetoothGattServer("t"); assert(s.name == "t", "name");"#,
        );
        assert_eq!(pass["result"]["isError"], false);
        assert!(
            pass["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("PASS")
        );

        let fail = call("run_test", r#"assert(1 == 2, "one is not two");"#);
        assert_eq!(fail["result"]["isError"], true);
        assert!(
            fail["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("FAIL")
        );
    }

    #[test]
    fn test_lint_catches_syntax_error_without_running() {
        let ok = call("lint", r#"let s = android::BluetoothGattServer("t");"#);
        assert_eq!(ok["result"]["isError"], false);

        let bad = call("lint", "let x = ;");
        assert_eq!(bad["result"]["isError"], true);
    }

    #[test]
    fn test_notification_gets_no_response() {
        // No "id" -> a notification -> no reply.
        assert!(handle(&json!({"jsonrpc":"2.0","method":"notifications/initialized"})).is_none());
    }

    #[test]
    fn test_unknown_method_is_json_rpc_error() {
        let resp = handle(&json!({"jsonrpc":"2.0","id":9,"method":"does/not/exist"})).unwrap();
        assert_eq!(resp["error"]["code"], -32601);
    }

    #[test]
    fn test_missing_script_argument_is_tool_error() {
        let resp = handle(&json!({
            "jsonrpc":"2.0","id":3,"method":"tools/call",
            "params": { "name": "lint", "arguments": {} },
        }))
        .unwrap();
        assert_eq!(resp["result"]["isError"], true);
    }
}
