// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! The bulk benchmark across two dongles, with both ends ours.
//!
//! The companion to `phone_bulk`: same measurement, but the sink is a
//! [`BulkSink`] whose byte count we can trust absolutely. When a run against
//! somebody else's peripheral reports loss, this says whether the sender or
//! the receiver put it there.
//!
//!     cargo run --example two_dongle_bulk -- 02.3.1 02.3.4 65536

use std::time::{Duration, Instant};

use simble::device::throughput::{BulkCentral, BulkOptions, BulkSink};
use simble::transport::HciChannel;
use simble::transport::usb::{UsbSelector, UsbTransport};
use simble::types::Address;

const SINK_ADDR: Address = Address::new([0x0B, 0x00, 0x00, 0x57, 0x1E, 0xCC]);

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let central_sel = a.first().map(String::as_str).unwrap_or("02.3.1");
    let sink_sel = a.get(1).map(String::as_str).unwrap_or("02.3.4");
    let total: usize = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(65536);

    let mut central_usb =
        UsbTransport::open_selected(&UsbSelector::parse(central_sel).unwrap()).unwrap();
    let mut sink_usb =
        UsbTransport::open_selected(&UsbSelector::parse(sink_sel).unwrap()).unwrap();
    let central_ch = HciChannel::new();
    let sink_ch = HciChannel::new();
    let clock = Instant::now();
    let now = || clock.elapsed().as_secs_f64() * 1000.0;

    let mut sink = BulkSink::new("bulk-sink", SINK_ADDR);
    let mut run = BulkCentral::new(
        SINK_ADDR,
        BulkOptions {
            total_bytes: total,
            ..BulkOptions::default()
        },
    );

    for p in sink.start_commands() {
        let _ = sink_ch.inject_host_packet(p);
    }
    for p in run.start(now()) {
        let _ = central_ch.inject_host_packet(p);
    }

    let deadline = Instant::now() + Duration::from_secs(180);
    while !run.is_finished() && Instant::now() < deadline {
        let _ = sink_usb.pump(&sink_ch);
        let _ = central_usb.pump(&central_ch);
        for p in sink.poll() {
            let _ = sink_ch.inject_host_packet(p);
        }
        while let Some(p) = sink_ch.poll_controller_packet() {
            for out in sink.on_packet(&p, now()) {
                let _ = sink_ch.inject_host_packet(out);
            }
        }
        while let Some(p) = central_ch.poll_controller_packet() {
            for out in run.on_packet(&p, now()) {
                let _ = central_ch.inject_host_packet(out);
            }
        }
        for p in run.step(now()) {
            let _ = central_ch.inject_host_packet(p);
        }
        for line in run.take_log() {
            println!("  {line}");
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    for line in run.take_log() {
        println!("  {line}");
    }
    let counters = sink.counters();
    println!("sink saw {} bytes in {} chunks", counters.bytes, counters.chunks);
    println!("{}", run.report_json());
}
