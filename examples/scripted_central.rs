// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Hosts a **scripted GATT client** on netsim, so it can be pointed at a peer
//! that is not simble's.
//!
//! In-process tests put a scripted central against a scripted peripheral,
//! which proves the scripting surface and nothing about the wire: two simble
//! endpoints agree with each other by construction. This binary is the other
//! half — it puts the same `ScriptedCentral` on netsim's ether, where a
//! Bumble peripheral or an Android emulator is the peer.
//!
//! ```bash
//! cargo run --example scripted_central -- hrm_client F0:F1:F2:F3:F4:D2 12
//! ```
//!
//! The first argument is a catalog client name or a path to a `.rhai` file,
//! the second the peer to connect to, the third how many seconds to run.
//! Exit status is the verdict: 0 if every `assert` in the script held and
//! discovery finished, 1 otherwise — so it can be driven from
//! `tests/interop/gatt_client.py` and believed.

use std::time::{Duration, Instant};

use simble::scripting::ScriptedCentral;
use simble::transport::{HciChannel, NETSIM_WS_URL, NetsimTransport};
use simble::types::Address;

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(source) = args.first() else {
        eprintln!(
            "usage: scripted_central <catalog-name|script.rhai> <peer-address> [seconds] [name]"
        );
        return std::process::ExitCode::from(2);
    };
    let script = match simble::devices::catalog::script(source) {
        Some(script) => script.to_string(),
        None => match std::fs::read_to_string(source) {
            Ok(text) => text,
            Err(e) => {
                eprintln!("{source}: not a catalog client and not a readable file ({e})");
                return std::process::ExitCode::from(2);
            }
        },
    };
    let target: Address = match args.get(1).map(|a| a.parse()) {
        Some(Ok(address)) => address,
        _ => {
            eprintln!("second argument must be the peer address, e.g. F0:F1:F2:F3:F4:D2");
            return std::process::ExitCode::from(2);
        }
    };
    let seconds: f64 = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(15.0);
    // The central's own identity on netsim. netsim reads the URL's address
    // parameter LSB-first, which `to_netsim_wire_string` accounts for.
    let own: Address = args
        .get(3)
        .and_then(|a| a.parse().ok())
        .unwrap_or_else(|| "F0:DE:C0:00:00:C1".parse().unwrap());

    let mut central = match ScriptedCentral::run_script(&script) {
        Ok(central) => central,
        Err(e) => {
            eprintln!("script rejected: {e}");
            return std::process::ExitCode::from(2);
        }
    };
    central.set_target(target);

    let url = format!(
        "{}/v1/websocket/bt?name=simble-client&address={}",
        NETSIM_WS_URL,
        own.to_netsim_wire_string()
    );
    let mut transport = match NetsimTransport::connect(&url) {
        Ok(transport) => transport,
        Err(e) => {
            eprintln!("cannot reach netsimd: {e}");
            return std::process::ExitCode::from(2);
        }
    };
    if let Ok(dir) = std::env::var("SIMBLE_BTSNOOP") {
        let path = std::path::Path::new(&dir).join("simble-client.btsnoop");
        match std::fs::File::create(&path) {
            Ok(file) => {
                let _ = transport.set_trace(file);
            }
            Err(e) => eprintln!("SIMBLE_BTSNOOP: cannot create {path:?}: {e}"),
        }
    }

    let channel = HciChannel::new();
    for packet in central.take_outbox() {
        let _ = channel.inject_host_packet(packet);
    }

    // Paced against the wall clock: the peer is a real process, and spinning
    // the loop flat out would burn a core without letting it answer.
    let step = Duration::from_millis(20);
    let deadline = Instant::now() + Duration::from_secs_f64(seconds);
    let mut t = 0.0;
    while Instant::now() < deadline {
        std::thread::sleep(step);
        if let Err(e) = transport.pump(&channel) {
            eprintln!("transport: {e}");
            return std::process::ExitCode::from(1);
        }
        while let Some(packet) = channel.poll_controller_packet() {
            for out in central.on_packet(&packet) {
                let _ = channel.inject_host_packet(out);
            }
        }
        t += step.as_secs_f64();
        for out in central.tick(t) {
            let _ = channel.inject_host_packet(out);
        }
        for message in central.take_emitted() {
            println!("emit {message}");
        }
    }

    println!("{}", central.status_json());
    let discovered = central
        .client()
        .with_central(|c| c.is_ready() && !c.services().is_empty());
    match central.failure() {
        Some(failure) => {
            eprintln!("FAIL — {failure}");
            std::process::ExitCode::from(1)
        }
        None if !discovered => {
            eprintln!("FAIL — never finished discovering the peer");
            std::process::ExitCode::from(1)
        }
        None => {
            println!("PASS — connected, discovered, and every assertion held");
            std::process::ExitCode::SUCCESS
        }
    }
}
