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
//! simble --usb [SELECTOR] [--ws-port N]  # SELECTOR: 0a12:0001 | #0 | 02/4 | 02.3.4
//!                                    # instead, bridge a USB dongle onto a
//!                                    # WebSocket so a browser can drive real HW
//! simble mcp [--ws-server [PORT]]    # the MCP server for agents, on stdio or
//!                                    # on a WebSocket
//! ```
//!
//! `--usb` is the `usb-ble-ws` bridge: it owns a physical Bluetooth dongle and
//! re-exposes its HCI over the netsim-style WebSocket (default port 7681), the
//! same transport pages use for `netsim` and `rootcanal-ws`.

use simble::scene::runner::{RunOptions, RunReport};
use simble::scene::{Controller, Scene};
use simble::transport::usb::{UsbSelector, list_bluetooth_dongles};
use simble::transport::wasm_ws::{lint_script, run_test_script};
use simble::transport::{HciChannel, Inbound, UsbTransport, accept_inbound};
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
  simble --usb [SELECTOR] [--ws N]     bridge a USB dongle onto ws://127.0.0.1:N/
                                       SELECTOR: 0a12:0001, #0, 02/4, or 02.3.4
                                       (`simble --usb-list` names every dongle)
  simble mcp                           run the MCP server (stdio) for agents
  simble mcp --ws-server [PORT]        …serve MCP over WebSocket instead (7682)

scene options:
  --controller self|netsim|usb         override the scene file's controller
  --seconds N                          how long to run each scene (default 2)
  --tick-ms N                          scene clock step in ms (default 100)

Running scripts exits 0 if every assert(...) holds, 1 if any fails.
Running a scene exits 0 if every device came up and none reported an error.
--usb serves every dongle at once, one session per client; point a page's backend at it.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if args.first().map(String::as_str) == Some("mcp") {
        // `--ws-server [PORT]` swaps the transport under the same server:
        // JSON-RPC over RFC 6455 text messages instead of stdio lines, so a
        // browser page (or any WebSocket client) can drive the same scene an
        // agent would. The port is optional and defaults to MCP_WS_PORT.
        let ws_port = match ws_server_port(&args[1..]) {
            Ok(port) => port,
            Err(message) => {
                eprintln!("simble mcp: {message}\n\n{USAGE}");
                return ExitCode::from(2);
            }
        };
        let served = match ws_port {
            Some(port) => simble::mcp::serve_ws(port),
            None => simble::mcp::serve_stdio(),
        };
        return match served {
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

/// The MCP server's default WebSocket port, one above the netsim/bridge port
/// so `--ws-server` and `--usb` can run side by side with no flags.
const MCP_WS_PORT: u16 = 7682;

/// Reads `--ws-server [PORT]` out of the `mcp` subcommand's arguments.
/// `Ok(None)` means stdio (the flag is absent); `Ok(Some(port))` means serve
/// WebSocket there. The port is optional, so the token after the flag is only
/// consumed when it parses as one — the same shape `--usb [VID:PID]` uses.
fn ws_server_port(args: &[String]) -> Result<Option<u16>, String> {
    let Some(at) = args.iter().position(|a| a == "--ws-server") else {
        // Catch a stray flag rather than silently serving stdio: "simble mcp
        // --wsserver 9000" would otherwise look like it worked.
        return match args.iter().find(|a| a.starts_with('-')) {
            Some(unknown) => Err(format!("unknown option {unknown:?}")),
            None => Ok(None),
        };
    };
    match args.get(at + 1) {
        None => Ok(Some(MCP_WS_PORT)),
        Some(next) => match next.parse::<u16>() {
            Ok(0) => Err("--ws-server needs a port between 1 and 65535, got 0".to_string()),
            Ok(port) => Ok(Some(port)),
            // Not a port: either another flag, or a typo worth reporting.
            Err(_) if next.starts_with('-') => Err(format!("unknown option {next:?}")),
            Err(_) => Err(format!("--ws-server needs a port number, got {next:?}")),
        },
    }
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
    // `--usb` may be followed by any selector form -- `0a12:0001`, `#0`,
    // `02/4`, `02.3.4`. If the next token is not one (it is another flag, or
    // absent), fall back to the first dongle. Two dongles of one model share a
    // vid:pid, so `#0`/`bus.port` are the forms that can actually name one;
    // `usb_list` prints every name each dongle answers to.
    let selector = args
        .windows(2)
        .find(|w| w[0] == "--usb")
        .and_then(|w| UsbSelector::parse(&w[1]).ok())
        .unwrap_or(UsbSelector::First);
    // `--ws` is accepted as well, because that is what the flag reads like.
    let ws_port = args
        .windows(2)
        .find(|w| w[0] == "--ws-port" || w[0] == "--ws")
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

    // A thread per client, because one bridge now serves *every* dongle: two
    // pages driving two radios need two live sessions, and a sequential accept
    // loop would let the first block the second forever.
    for incoming in listener.incoming() {
        match incoming {
            Ok(stream) => {
                let fallback = selector.clone();
                std::thread::spawn(move || {
                    if let Err(e) = serve(stream, &fallback) {
                        // A client disconnect surfaces as an error too; it is
                        // the clean end of a session, so log it and move on.
                        eprintln!("  session ended: {e}");
                    }
                });
            }
            Err(e) => eprintln!("usb-ws: accept failed: {e}"),
        }
    }
    ExitCode::SUCCESS
}

/// The dongle list a page reads before choosing one, as JSON.
///
/// Hand-rolled rather than pulled through serde: this is the only JSON the
/// bridge emits, and the fields are a handful of integers and strings.
fn devices_json() -> String {
    let dongles = list_bluetooth_dongles().unwrap_or_default();
    let mut out = String::from("{\"devices\":[");
    for (i, d) in dongles.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let product = d.product.clone().unwrap_or_default().replace('"', "'");
        out.push_str(&format!(
            "{{\"index\":{},\"selector\":\"{}\",\"bus\":\"{}\",\"address\":{},\
              \"vid\":\"{:04x}\",\"pid\":\"{:04x}\",\"product\":\"{product}\"}}",
            d.index,
            d.port_selector(),
            d.bus_id,
            d.device_address,
            d.vendor_id,
            d.product_id
        ));
    }
    out.push_str("]}");
    out
}

/// The phones adb can see, each with a local port forwarded to SimBLE Android.
///
/// The page cannot run adb and cannot reach a phone directly — an access
/// point that isolates clients refuses new connections while adb's own
/// established one survives — so the bridge sets up a forward per phone and
/// hands back the ports. That is the same reason it serves `/devices`: a page
/// cannot discover a port, but it can read a list.
///
/// Probing each forward is what separates "a phone is plugged in" from "a
/// phone is running the sink", and only the second is selectable.
fn phones_json() -> String {
    let adb = adb_path();
    let listed = std::process::Command::new(&adb).arg("devices").arg("-l").output();
    let Ok(listed) = listed else {
        return "{\"phones\":[],\"error\":\"adb not found — put it on PATH or set ANDROID_HOME\"}"
            .to_string();
    };
    let text = String::from_utf8_lossy(&listed.stdout);

    let mut out = String::from("{\"phones\":[");
    let mut first = true;
    let mut seen: Vec<String> = Vec::new();
    let mut index = 0usize;
    for line in text
        .lines()
        .skip(1)
        .filter(|l| l.contains("\tdevice") || l.contains(" device "))
    {
        let Some(serial) = line.split_whitespace().next() else {
            continue;
        };
        // One phone can appear twice — a wifi transport and an mdns one are
        // two adb entries for one radio. Offering both would let a caller
        // choose "two phones" that are one, and then measure a scan against
        // the phone it was not fetching counters from.
        let identity = std::process::Command::new(&adb)
            .args(["-s", serial, "shell", "getprop", "ro.serialno"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        if !identity.is_empty() {
            if seen.contains(&identity) {
                continue;
            }
            seen.push(identity);
        }
        // One port per phone, so two phones are never the same endpoint —
        // which is the bug this whole list exists to prevent.
        let port = 8099 + index as u16;
        index += 1;
        let _ = std::process::Command::new(&adb)
            .args(["-s", serial, "forward", &format!("tcp:{port}"), "tcp:8099"])
            .output();

        let probe = probe_sink(port);
        if !first {
            out.push(',');
        }
        first = false;
        let model = line
            .split_whitespace()
            .find_map(|f| f.strip_prefix("model:"))
            .unwrap_or("phone");
        out.push_str(&format!(
            "{{\"serial\":\"{serial}\",\"model\":\"{model}\",\"port\":{port},\"name\":\"{}\",\"running\":{}}}",
            probe.as_deref().unwrap_or(""),
            probe.is_some()
        ));
    }
    out.push_str("]}");
    out
}

/// Where adb is. A bridge started from a launcher or a login shell without
/// the SDK on PATH still has to find it, so the usual install location is
/// tried before giving up.
fn adb_path() -> String {
    if std::process::Command::new("adb").arg("version").output().is_ok() {
        return "adb".to_string();
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let candidates = [
        std::env::var("ANDROID_HOME").ok().map(|h| format!("{h}/platform-tools/adb")),
        Some(format!("{home}/Library/Android/sdk/platform-tools/adb")),
        Some(format!("{home}/Android/Sdk/platform-tools/adb")),
    ];
    for candidate in candidates.into_iter().flatten() {
        if std::path::Path::new(&candidate).exists() {
            return candidate;
        }
    }
    "adb".to_string()
}

/// Asks the sink on `port` what it advertises as. `None` if nothing answers.
fn probe_sink(port: u16) -> Option<String> {
    use std::io::{Read as _, Write as _};
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_millis(700)))
        .ok()?;
    write!(stream, "GET /stats HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").ok()?;
    let mut body = String::new();
    stream.read_to_string(&mut body).ok()?;
    // One field out of a flat object, without taking a JSON dependency into
    // a binary that has none.
    let at = body.find("\"name\":\"")? + 8;
    let rest = &body[at..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Serves one WebSocket client end-to-end: handshake, open the dongle, then
/// shuttle HCI both ways through one shared channel until either side closes.
fn serve(mut stream: TcpStream, fallback: &UsbSelector) -> Result<(), String> {
    let inbound = match accept_inbound(stream.try_clone().map_err(|e| e.to_string())?) {
        Ok(i) => i,
        Err(e) => return Err(e.to_string()),
    };
    let (mut ws, query) = match inbound {
        Inbound::WebSocket(ws, query) => (ws, query),
        // A plain GET: the page is asking what it can connect *to*. This is
        // why the bridge serves every dongle from one port rather than one
        // port each -- a page cannot discover a port, but it can read a list.
        Inbound::Request { method, target } => {
            let known = target.starts_with("/devices") || target.starts_with("/phones");
            let body = if target.starts_with("/devices") {
                devices_json()
            } else if target.starts_with("/phones") {
                phones_json()
            } else {
                format!(
                    "{{\"error\":\"unknown path\",\"method\":{method:?},\"target\":{target:?},\
                      \"try\":\"/devices or /phones\"}}"
                )
            };
            let status = if known { "200 OK" } else { "404 Not Found" };
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                 Access-Control-Allow-Origin: *\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            use std::io::Write as _;
            stream
                .write_all(response.as_bytes())
                .map_err(|e| e.to_string())?;
            return Ok(());
        }
    };
    // `?device=<selector>` names which dongle this client wants, the way
    // netsim's `?name=&address=` names which device a connection carries.
    let wanted = query
        .split('&')
        .find_map(|kv| kv.strip_prefix("device="))
        .map(|v| v.replace("%23", "#"));
    let selector = match &wanted {
        Some(spec) => UsbSelector::parse(spec).map_err(|e| e.to_string())?,
        None => fallback.clone(),
    };
    eprintln!("  client connected ({query:?}); opening {selector:?}…");
    let mut dongle = UsbTransport::open_selected(&selector).map_err(|e| e.to_string())?;
    eprintln!("  bridging — the dongle is now this client's controller");

    let channel = HciChannel::new();
    let session: Result<(), String> = loop {
        if let Err(e) = ws.pump(&channel) {
            break Err(e.to_string()); // WebSocket host <-> channel
        }
        if let Err(e) = dongle.pump(&channel) {
            break Err(e.to_string()); // channel <-> dongle (controller)
        }
        std::thread::sleep(POLL_INTERVAL);
    };
    // The session is over, however it ended — silence the dongle. A
    // controller outlives its host: without this, a dead session leaves the
    // dongle discoverable as a device nobody is behind, and the next phone
    // to try it gets an unexplained pairing failure.
    let quiet = HciChannel::new();
    if quiet.send_command(&[0x03, 0x0C, 0x00]).is_ok() {
        for _ in 0..200 {
            if dongle.pump(&quiet).is_err() {
                break;
            }
            if quiet.poll_controller_packet().is_some() {
                eprintln!("  session over — dongle reset");
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
    session
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn test_ws_server_port_defaults_and_overrides() {
        // No flag: stdio, as `simble mcp` has always meant.
        assert_eq!(ws_server_port(&args(&[])), Ok(None));
        // Flag with no port: the default.
        assert_eq!(
            ws_server_port(&args(&["--ws-server"])),
            Ok(Some(MCP_WS_PORT))
        );
        assert_eq!(
            ws_server_port(&args(&["--ws-server", "9001"])),
            Ok(Some(9001))
        );
    }

    #[test]
    fn test_ws_server_port_rejects_what_cannot_be_a_port() {
        // A typo must be reported, not silently ignored into serving stdio —
        // "it printed nothing and hung" is the failure mode that costs an
        // afternoon.
        for bad in [
            args(&["--ws-server", "0"]),
            args(&["--ws-server", "70000"]),
            args(&["--ws-server", "http://localhost:9001"]),
            args(&["--wsserver", "9001"]),
            args(&["--ws-port", "9001"]),
        ] {
            assert!(ws_server_port(&bad).is_err(), "{bad:?} should be rejected");
        }
        // A flag after the bare form is not mistaken for its port.
        assert!(ws_server_port(&args(&["--ws-server", "--verbose"])).is_err());
    }
}
