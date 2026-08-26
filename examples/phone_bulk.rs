// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! The bulk-transfer benchmark against a real phone.
//!
//! A dongle is the central and does the writing; the phone runs SimBLE Android
//! (`android/app/`) and counts what lands. That direction is the interesting
//! one: it puts Android's real host stack and a real phone controller on the
//! receiving end, which is where the bugs that only real silicon shows have
//! always been.
//!
//! # Why the counters come back over HTTP
//!
//! They must not come back over the link. A `FINISH`/`REPORT` exchange on the
//! peer's control point costs air time on the link being measured, and the
//! report's *arrival* is what ends the measured transfer — so every figure
//! would include a round trip of the thing under test. A run whose whole
//! point is a broken link could not deliver its result over it at all.
//!
//! So the run sets `use_control_point: false` and the link carries payload
//! and nothing else. SimBLE Android serves its counters on port 8099, and this
//! reads them before and after. The phone's `duration_ms` is measured
//! entirely on the phone's own clock; a *duration* needs no agreement about
//! epochs, which is what makes it quotable here.
//!
//! Reach it however the phone is reachable — a plain address, or a forward:
//!
//!     adb forward tcp:8099 tcp:8099
//!
//! # Why this scans first
//!
//! Android advertises from a resolvable private address that rotates, and it
//! does not tell its own app what that address is — so there is no address to
//! write down in advance. The peer has to be found by the service it
//! advertises. That is the only reason this is a separate example rather than
//! the existing dongle-to-dongle path with a different argument.
//!
//!     cargo run --example phone_bulk -- [dongle-selector] [bytes]
//!     cargo run --example phone_bulk -- 02.3.1 65536

use std::time::{Duration, Instant};

use simble::device::throughput::{BulkCentral, BulkOptions, bulk_uuid};
use simble::transport::HciChannel;
use simble::transport::usb::{UsbSelector, UsbTransport};
use simble::transport::wasm_ws::{parse_scan_reports, queue_scanner_start};
use simble::types::Address;

use std::io::{Read, Write};
use std::net::TcpStream;

/// LE Set Scan Enable — off, no duplicate filtering.
const SCAN_OFF: [u8; 5] = [0x0C, 0x20, 0x02, 0x00, 0x00];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let selector = args.first().map(String::as_str).unwrap_or("02.3.1");
    let total_bytes: usize = args
        .get(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(65536);

    // Where SimBLE Android is serving. `adb forward tcp:8099 tcp:8099` makes the
    // default work over USB or wifi adb without knowing the phone's address.
    let stats_host = std::env::var("SIMBLE_SINK_HTTP")
        .unwrap_or_else(|_| "127.0.0.1:8099".to_string());

    let sel = UsbSelector::parse(selector).expect("selector");
    let mut usb = UsbTransport::open_selected(&sel).expect("open dongle");
    let channel = HciChannel::new();
    let clock = Instant::now();
    let now = |c: &Instant| c.elapsed().as_secs_f64() * 1000.0;

    let wanted = bulk_uuid::SERVICE.to_string();
    println!("scanning on {selector} for {wanted}");
    queue_scanner_start(&channel).expect("scanner bring-up");

    let scan_began = Instant::now();
    let Some((address, rssi, name)) = find_sink(&mut usb, &channel, &wanted, Duration::from_secs(20))
    else {
        eprintln!(
            "no peer advertising the bulk service.\n\
             Is SimBLE Android running and in the foreground?\n\
             adb shell am start -n com.simble/.SimbleActivity"
        );
        std::process::exit(1);
    };

    println!(
        "found {address} at {rssi} dBm{} in {:.1} ms",
        name.map(|n| format!(" — \"{n}\"")).unwrap_or_default(),
        scan_began.elapsed().as_secs_f64() * 1000.0
    );

    channel.send_command(&SCAN_OFF).expect("scan off");
    settle(&mut usb, &channel, Duration::from_millis(300));

    // Zero the peer's counters out of band, so the link carries no setup
    // either. This is the HTTP twin of a `BEGIN` on the control point.
    match http_get(&stats_host, &format!("/reset?total={total_bytes}")) {
        Ok(body) => println!("sink reset: {body}"),
        Err(e) => {
            eprintln!(
                "could not reach SimBLE Android at {stats_host}: {e}\n\
                 try: adb forward tcp:8099 tcp:8099"
            );
            std::process::exit(1);
        }
    }

    let target: Address = address.parse().expect("address");
    let mut run = BulkCentral::new(
        target,
        BulkOptions {
            total_bytes,
            // The link carries payload only; the count comes over HTTP.
            use_control_point: false,
            ..BulkOptions::default()
        },
    );

    println!("running {total_bytes} bytes");
    for packet in run.start(now(&clock)) {
        let _ = channel.inject_host_packet(packet);
    }

    // The phone is a stranger: it may never answer, so the loop is bounded by
    // the wall clock as well as by the run's own watchdog.
    let deadline = Instant::now() + Duration::from_secs(180);
    while !run.is_finished() && Instant::now() < deadline {
        usb.pump(&channel).expect("usb pump");
        while let Some(packet) = channel.poll_controller_packet() {
            for out in run.on_packet(&packet, now(&clock)) {
                let _ = channel.inject_host_packet(out);
            }
        }
        for out in run.step(now(&clock)) {
            let _ = channel.inject_host_packet(out);
        }
        for line in run.take_log() {
            println!("  {line}");
        }
        std::thread::sleep(Duration::from_millis(1));
    }

    for line in run.take_log() {
        println!("  {line}");
    }
    println!("{}", run.report_json());

    // The measurement proper: what the phone says it received, on a path the
    // run did not touch.
    match http_get(&stats_host, "/stats") {
        Ok(body) => println!("sink says: {body}"),
        Err(e) => eprintln!("could not read the sink's counters: {e}"),
    }

    if !run.is_finished() {
        eprintln!("gave up after 180 s");
        std::process::exit(1);
    }
}

/// Scans until something advertises `wanted`, or the deadline passes.
fn find_sink(
    usb: &mut UsbTransport,
    channel: &HciChannel,
    wanted: &str,
    within: Duration,
) -> Option<(String, i8, Option<String>)> {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        usb.pump(channel).ok()?;
        while let Some(packet) = channel.poll_controller_packet() {
            for report in parse_scan_reports(&packet) {
                if report
                    .service_uuids
                    .iter()
                    .any(|u| u.eq_ignore_ascii_case(wanted))
                {
                    return Some((report.address, report.rssi, report.name));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    None
}

/// One HTTP GET, hand-rolled because this crate carries no HTTP client and
/// this speaks to exactly one server that answers in one packet.
fn http_get(host: &str, path: &str) -> std::io::Result<String> {
    let mut stream = TcpStream::connect(host)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    // The body is whatever follows the blank line.
    Ok(response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.trim().to_string())
        .unwrap_or(response))
}

/// Drains the controller for a while, so a mode change lands before the next.
fn settle(usb: &mut UsbTransport, channel: &HciChannel, how_long: Duration) {
    let until = Instant::now() + how_long;
    while Instant::now() < until {
        let _ = usb.pump(channel);
        while channel.poll_controller_packet().is_some() {}
        std::thread::sleep(Duration::from_millis(2));
    }
}
