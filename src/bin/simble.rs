// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! `simble` — the SimBLE developer CLI.
//!
//! A SimBLE device is a `.rhai` script, so the default action is simply to run
//! one (or several) as a test: each `assert(...)` is checked in a fresh
//! in-process engine — deterministic, no radio — and the exit code is 0 if
//! every assertion in every file holds, 1 otherwise. It's the same evaluation
//! the browser Testing page does, so the file that passes here is the file CI
//! runs.
//!
//! ```text
//! simble device.rhai                 # run one device/test
//! simble tests/*.rhai                # run many; nonzero exit if any fail
//! simble < device.rhai               # or read the script from stdin
//! simble --usb [VID:PID] [--ws-port N]
//!                                    # instead, bridge a USB dongle onto a
//!                                    # WebSocket so a browser can drive real HW
//! ```
//!
//! `--usb` is the `usb-ble-ws` bridge: it owns a physical Bluetooth dongle and
//! re-exposes its HCI over the netsim-style WebSocket (default port 7681), the
//! same transport pages use for `netsim` and `rootcanal-ws`.

use simble::transport::usb::parse_vid_pid;
use simble::transport::wasm_ws::run_test_script;
use simble::transport::{HciChannel, UsbTransport, WsServerConn};
use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::process::ExitCode;
use std::time::Duration;

const USAGE: &str = "\
simble — SimBLE developer CLI

usage:
  simble FILE.rhai [FILE.rhai ...]     run device script(s) as tests (stdin if none)
  simble --usb [VID:PID] [--ws-port N] bridge a USB dongle onto ws://127.0.0.1:N/

Running scripts exits 0 if every assert(...) holds, 1 if any fails.
--usb serves one WebSocket client at a time; point a page's backend at it.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "--usb") {
        return run_bridge(&args);
    }
    run_tests(&args)
}

// --- default: run .rhai device scripts as tests ----------------------------

fn run_tests(args: &[String]) -> ExitCode {
    // Positional args are files; reject stray flags so a typo isn't silently
    // treated as "no files, read stdin".
    let mut files = Vec::new();
    for arg in args {
        if arg.starts_with('-') {
            eprintln!("simble: unknown option {arg:?}\n\n{USAGE}");
            return ExitCode::from(2);
        }
        files.push(arg.clone());
    }

    if files.is_empty() {
        let mut script = String::new();
        if let Err(e) = std::io::stdin().read_to_string(&mut script) {
            eprintln!("simble: cannot read stdin: {e}");
            return ExitCode::from(2);
        }
        return report("<stdin>", &script, false);
    }

    let show_names = files.len() > 1;
    let mut any_failed = false;
    for file in &files {
        let script = match std::fs::read_to_string(file) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("simble: cannot read {file}: {e}");
                any_failed = true;
                continue;
            }
        };
        if report(file, &script, show_names) == ExitCode::FAILURE {
            any_failed = true;
        }
    }
    if any_failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Runs one script and prints its result. `with_name` prefixes each line with
/// the file, so a multi-file run reads like a test report.
fn report(name: &str, script: &str, with_name: bool) -> ExitCode {
    let tag = if with_name {
        format!("{name}: ")
    } else {
        String::new()
    };
    match run_test_script(script) {
        Ok(()) => {
            println!("{tag}PASS — all assertions held");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("{tag}FAIL — {message}");
            ExitCode::FAILURE
        }
    }
}

// --- `--usb`: the usb-ble-ws bridge ----------------------------------------

/// Idle spin between non-blocking pump passes — small enough to stay
/// responsive, large enough not to peg a core. There is no async runtime.
const POLL_INTERVAL: Duration = Duration::from_millis(1);

fn run_bridge(args: &[String]) -> ExitCode {
    // `--usb` may be followed by a VID:PID; if that token doesn't parse as one
    // (e.g. it's `--ws-port`), fall back to auto-detecting the first dongle.
    let device = args
        .windows(2)
        .find(|w| w[0] == "--usb")
        .and_then(|w| parse_vid_pid(&w[1]).ok());
    let ws_port = args
        .windows(2)
        .find(|w| w[0] == "--ws-port")
        .and_then(|w| w[1].parse::<u16>().ok())
        .unwrap_or(7681);

    let listener = match TcpListener::bind(("127.0.0.1", ws_port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("simble: cannot bind 127.0.0.1:{ws_port}: {e}");
            return ExitCode::from(2);
        }
    };
    eprintln!(
        "usb-ws: listening on ws://127.0.0.1:{ws_port}/  (point a page's WebSocket backend here)"
    );

    for incoming in listener.incoming() {
        match incoming {
            Ok(stream) => {
                if let Err(e) = serve(stream, device) {
                    // A client disconnect surfaces as an error too; it's the
                    // clean end of a session, so log and await the next one.
                    eprintln!("  session ended: {e}");
                }
                eprintln!("usb-ws: waiting for the next client…");
            }
            Err(e) => eprintln!("usb-ws: accept failed: {e}"),
        }
    }
    ExitCode::SUCCESS
}

/// Serves one WebSocket client end-to-end: handshake, open the dongle, then
/// shuttle HCI both ways through one shared channel until either side closes.
fn serve(stream: TcpStream, device: Option<(u16, u16)>) -> Result<(), String> {
    let (mut ws, query) = WsServerConn::accept(stream).map_err(|e| e.to_string())?;
    eprintln!("  client connected ({query:?}); opening dongle…");
    let mut dongle = match device {
        Some((vid, pid)) => UsbTransport::open(vid, pid),
        None => UsbTransport::open_first(),
    }
    .map_err(|e| e.to_string())?;
    eprintln!("  bridging — the dongle is now this client's controller");

    let channel = HciChannel::new();
    loop {
        ws.pump(&channel).map_err(|e| e.to_string())?; // WebSocket host <-> channel
        dongle.pump(&channel).map_err(|e| e.to_string())?; // channel <-> dongle (controller)
        std::thread::sleep(POLL_INTERVAL);
    }
}
