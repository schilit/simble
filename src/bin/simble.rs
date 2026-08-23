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
//! A `.json` file is a *scene*: several devices, how they are placed and how
//! they are wired (see `docs/scene-format.md`). It runs the same way — one
//! file in, exit 0 or 1 — so a whole topology is as committable and as
//! runnable as a single device.
//!
//! ```text
//! simble device.rhai                 # run one device/test
//! simble tests/*.rhai                # run many; nonzero exit if any fail
//! simble scene.json                  # instantiate a whole scene and run it
//! simble < device.rhai               # or read the script from stdin
//! simble --usb [VID:PID] [--ws-port N]
//!                                    # instead, bridge a USB dongle onto a
//!                                    # WebSocket so a browser can drive real HW
//! ```
//!
//! `--usb` is the `usb-ble-ws` bridge: it owns a physical Bluetooth dongle and
//! re-exposes its HCI over the netsim-style WebSocket (default port 7681), the
//! same transport pages use for `netsim` and `rootcanal-ws`.

use simble::scene::runner::{RunOptions, RunReport};
use simble::scene::{Controller, Scene};
use simble::transport::usb::parse_vid_pid;
use simble::transport::wasm_ws::{lint_script, run_test_script};
use simble::transport::{HciChannel, UsbTransport, WsServerConn};
use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::process::ExitCode;
use std::str::FromStr;
use std::time::Duration;

const USAGE: &str = "\
simble — SimBLE developer CLI

usage:
  simble FILE.rhai [FILE.rhai ...]     run device script(s) as tests (stdin if none)
  simble SCENE.json [SCENE.json ...]   instantiate a scene file and run it
  simble --no-run FILE ...             check only: compile scripts / validate scenes
  simble --usb [VID:PID] [--ws-port N] bridge a USB dongle onto ws://127.0.0.1:N/
  simble mcp                           run the MCP server (stdio) for agents

scene options:
  --controller self|netsim|usb         override the scene file's controller
  --seconds N                          how long to run each scene (default 2)
  --tick-ms N                          scene clock step in ms (default 100)

Running scripts exits 0 if every assert(...) holds, 1 if any fails.
Running a scene exits 0 if every device came up and none reported an error.
--usb serves one WebSocket client at a time; point a page's backend at it.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if args.first().map(String::as_str) == Some("mcp") {
        return match simble::mcp::serve_stdio() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("simble mcp: {e}");
                ExitCode::from(2)
            }
        };
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
    let mut lint_only = false;
    let mut scene_options = RunOptions::default();
    let mut controller_override = None;
    let mut scene_flags: Vec<String> = Vec::new();
    let mut expecting: Option<String> = None;
    for arg in args {
        if let Some(option) = expecting.take() {
            scene_flags.push(option.clone());
            match parse_scene_option(option, arg, &mut scene_options, &mut controller_override) {
                Ok(()) => continue,
                Err(message) => {
                    eprintln!("simble: {message}\n\n{USAGE}");
                    return ExitCode::from(2);
                }
            }
        }
        match arg.as_str() {
            "--no-run" => lint_only = true,
            "--controller" | "--seconds" | "--tick-ms" => expecting = Some(arg.clone()),
            _ if arg.starts_with('-') => {
                eprintln!("simble: unknown option {arg:?}\n\n{USAGE}");
                return ExitCode::from(2);
            }
            _ => files.push(arg.clone()),
        }
    }
    if let Some(option) = expecting {
        eprintln!("simble: {option} needs a value\n\n{USAGE}");
        return ExitCode::from(2);
    }

    // A scene file is a different kind of artifact, not a different kind of
    // script, so it gets its own path rather than a mode flag.
    let (scenes, scripts): (Vec<String>, Vec<String>) =
        files.iter().cloned().partition(|f| f.ends_with(".json"));
    if !scenes.is_empty() {
        if !scripts.is_empty() {
            eprintln!(
                "simble: cannot mix scene files with device scripts in one run \
                 (a scene already says which devices it holds)"
            );
            return ExitCode::from(2);
        }
        return run_scenes(&scenes, lint_only, controller_override, &scene_options);
    }
    // A scene option with no scene to apply it to is a mistake, not a no-op:
    // silently ignoring it is how someone concludes --controller doesn't work.
    if !scene_flags.is_empty() {
        eprintln!(
            "simble: {} only applies to a scene file, and none was given\n\n{USAGE}",
            scene_flags.join(", ")
        );
        return ExitCode::from(2);
    }

    if files.is_empty() {
        let mut script = String::new();
        if let Err(e) = std::io::stdin().read_to_string(&mut script) {
            eprintln!("simble: cannot read stdin: {e}");
            return ExitCode::from(2);
        }
        return report("<stdin>", &script, false, lint_only);
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
        if report(file, &script, show_names, lint_only) == ExitCode::FAILURE {
            any_failed = true;
        }
    }
    if any_failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Runs (or, with `lint_only`, just compiles) one script and prints its result.
/// `with_name` prefixes each line with the file, so a multi-file run reads like
/// a test report.
fn report(name: &str, script: &str, with_name: bool, lint_only: bool) -> ExitCode {
    let tag = if with_name {
        format!("{name}: ")
    } else {
        String::new()
    };
    let outcome = if lint_only {
        lint_script(script).map(|()| "OK — compiles cleanly")
    } else {
        run_test_script(script).map(|()| "PASS — all assertions held")
    };
    match outcome {
        Ok(message) => {
            println!("{tag}{message}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            let label = if lint_only { "LINT" } else { "FAIL" };
            eprintln!("{tag}{label} — {message}");
            ExitCode::FAILURE
        }
    }
}

// --- scene files ------------------------------------------------------------

fn parse_scene_option(
    option: String,
    value: &str,
    options: &mut RunOptions,
    controller: &mut Option<Controller>,
) -> Result<(), String> {
    match option.as_str() {
        "--controller" => {
            *controller = Some(Controller::from_str(value)?);
            Ok(())
        }
        "--seconds" => {
            options.seconds = value
                .parse()
                .map_err(|_| format!("--seconds needs a number, got {value:?}"))?;
            Ok(())
        }
        "--tick-ms" => {
            options.tick_ms = value
                .parse()
                .map_err(|_| format!("--tick-ms needs a whole number, got {value:?}"))?;
            Ok(())
        }
        other => Err(format!("unknown option {other:?}")),
    }
}

/// Loads and runs each scene file, printing one block per scene. `check_only`
/// stops after validation — the scene equivalent of `--no-run`, and what CI
/// wants for a fixture whose controller isn't available on the runner.
fn run_scenes(
    files: &[String],
    check_only: bool,
    controller: Option<Controller>,
    options: &RunOptions,
) -> ExitCode {
    let mut any_failed = false;
    for file in files {
        if run_scene(file, check_only, controller, options) == ExitCode::FAILURE {
            any_failed = true;
        }
    }
    if any_failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn run_scene(
    file: &str,
    check_only: bool,
    controller: Option<Controller>,
    options: &RunOptions,
) -> ExitCode {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(e) => {
            eprintln!("simble: cannot read {file}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut scene = match Scene::from_json(&text) {
        Ok(scene) => scene,
        Err(e) => {
            eprintln!("{file}: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Some(controller) = controller {
        scene.controller = controller;
    }
    let resolved = match scene.resolve() {
        Ok(resolved) => resolved,
        Err(e) => {
            eprintln!("{file}: {e}");
            return ExitCode::FAILURE;
        }
    };

    let label = resolved.name.clone().unwrap_or_else(|| file.to_string());
    println!(
        "{file}: {label} — {} device(s) on {}",
        resolved.devices.len(),
        resolved.controller
    );
    for device in &resolved.devices {
        let target = device
            .target
            .as_ref()
            .map(|(id, _)| format!(" -> {id}"))
            .unwrap_or_default();
        println!(
            "  {:<12} {:<13} {}{target}",
            device.id,
            device.role.to_string(),
            device.address
        );
    }
    if check_only {
        println!(
            "{file}: OK — valid scene ({} device(s))",
            resolved.devices.len()
        );
        return ExitCode::SUCCESS;
    }

    match simble::scene::runner::run(&resolved, options) {
        Ok(report) => report_scene(file, &report),
        Err(e) => {
            eprintln!("{file}: {e}");
            ExitCode::FAILURE
        }
    }
}

fn report_scene(file: &str, report: &RunReport) -> ExitCode {
    for device in &report.devices {
        match (&device.error, &device.name) {
            (Some(error), _) => eprintln!("  {} — ERROR {error}", device.id),
            (None, Some(name)) => println!("  {} — up as {name:?}", device.id),
            (None, None) => println!("  {} — up", device.id),
        }
    }
    // Say it out loud rather than let a declared-but-unapplied bond look like
    // a working one: a scene can express more than this loader materializes.
    let pending = report.bonds_not_installed();
    if pending > 0 {
        eprintln!(
            "  note: {pending} declared bond record(s) were validated but not installed — \
             the loader cannot yet reach a scripted device's bond store \
             (see docs/scene-format.md)"
        );
    }
    if report.ok() {
        println!(
            "{file}: PASS — {} device(s) ran {:.1}s on {}",
            report.devices.len(),
            report.elapsed,
            report.controller
        );
        ExitCode::SUCCESS
    } else {
        eprintln!("{file}: FAIL — a device reported an error");
        ExitCode::FAILURE
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
