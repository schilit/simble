// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! The bulk-transfer benchmark against a real phone.
//!
//! A dongle is the central and does the writing; the phone runs SimBLE Sink
//! (`android/app/`) and counts what lands. That direction is the interesting
//! one: it puts Android's real host stack and a real phone controller on the
//! receiving end, which is where the bugs that only real silicon shows have
//! always been.
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

/// LE Set Scan Enable — off, no duplicate filtering.
const SCAN_OFF: [u8; 5] = [0x0C, 0x20, 0x02, 0x00, 0x00];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let selector = args.first().map(String::as_str).unwrap_or("02.3.1");
    let total_bytes: usize = args
        .get(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(65536);

    let sel = UsbSelector::parse(selector).expect("selector");
    let mut usb = UsbTransport::open_selected(&sel).expect("open dongle");
    let channel = HciChannel::new();
    let clock = Instant::now();
    let now = |c: &Instant| c.elapsed().as_secs_f64() * 1000.0;

    let wanted = bulk_uuid::SERVICE.to_string();
    println!("scanning on {selector} for {wanted}");
    queue_scanner_start(&channel).expect("scanner bring-up");

    let Some((address, rssi, name)) = find_sink(&mut usb, &channel, &wanted, Duration::from_secs(20))
    else {
        eprintln!(
            "no peer advertising the bulk service.\n\
             Is SimBLE Sink running and in the foreground?\n\
             adb shell am start -n com.simble.sink/.SinkActivity"
        );
        std::process::exit(1);
    };

    println!(
        "found {address} at {rssi} dBm{}",
        name.map(|n| format!(" — \"{n}\"")).unwrap_or_default()
    );

    channel.send_command(&SCAN_OFF).expect("scan off");
    settle(&mut usb, &channel, Duration::from_millis(300));

    let target: Address = address.parse().expect("address");
    let mut run = BulkCentral::new(
        target,
        BulkOptions {
            total_bytes,
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

/// Drains the controller for a while, so a mode change lands before the next.
fn settle(usb: &mut UsbTransport, channel: &HciChannel, how_long: Duration) {
    let until = Instant::now() + how_long;
    while Instant::now() < until {
        let _ = usb.pump(channel);
        while channel.poll_controller_packet().is_some() {}
        std::thread::sleep(Duration::from_millis(2));
    }
}
