// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Unit tests for the bulk-transfer benchmark state machine.
//!
//! The clock is fake throughout — a step counter turned into milliseconds —
//! which is the whole reason the runner takes `now_ms` from its caller. That
//! makes every timing assertion here exact rather than flaky: a segment that
//! spans three steps is 3 ms, always, on any machine.
//!
//! What these cover: the four segments are stamped in order and sum to the
//! total; the transfer ends at *arrival* rather than at the central's last
//! queued write; the peripheral's byte count is what the report quotes;
//! a peer that is not a benchmark sink fails rather than hangs; a run that
//! never finds its peer is recorded as a failed measurement with the segment
//! named; and the chunk size follows the negotiated MTU.

use super::*;

/// A millisecond per step: coarse enough to read, fine enough that every
/// segment boundary lands on a distinct value.
fn clock(step: usize) -> f64 {
    step as f64
}

const SINK: Address = Address::new([0x0B, 0x00, 0x00, 0x57, 0x1E, 0xCC]);
const CENTRAL: Address = Address::new([0x0C, 0x00, 0x00, 0x57, 0x1E, 0xCC]);

/// A scene with a small transfer, so the tests stay quick.
fn scene(total_bytes: usize) -> ThroughputScene {
    ThroughputScene::new(
        SINK,
        CENTRAL,
        BulkOptions {
            total_bytes,
            ..BulkOptions::default()
        },
    )
}

#[test]
fn a_whole_run_delivers_every_byte() {
    let mut scene = scene(64 * 1024);
    assert!(
        scene.run(20_000, clock),
        "the run did not finish: {:?}",
        scene.report()
    );
    let report = scene.report();
    assert_eq!(report.phase, "complete", "{report:?}");
    assert_eq!(report.failure, None);
    assert_eq!(report.bytes_sent, 64 * 1024);
    assert_eq!(report.bytes_received, Some(64 * 1024));
    assert_eq!(report.chunks_received, Some(report.chunks_sent));
}

#[test]
fn the_four_segments_are_stamped_in_order_and_sum_to_the_total() {
    let mut scene = scene(16 * 1024);
    assert!(scene.run(20_000, clock));
    let report = scene.report();

    let discover = report.discover_ms.expect("discover was stamped");
    let connect = report.connect_ms.expect("connect was stamped");
    let negotiate = report.negotiate_ms.expect("negotiate was stamped");
    let transfer = report.transfer_ms.expect("transfer was stamped");
    let total = report.total_ms.expect("the total was stamped");

    for (name, value) in [
        ("discover", discover),
        ("connect", connect),
        ("negotiate", negotiate),
        ("transfer", transfer),
    ] {
        assert!(value >= 0.0, "{name} ran backwards: {value}");
    }
    // The four segments are contiguous by construction, so they must add up
    // exactly. A drift here means a boundary was stamped twice or not at all.
    assert!(
        (discover + connect + negotiate + transfer - total).abs() < 1e-9,
        "{discover} + {connect} + {negotiate} + {transfer} != {total}"
    );
    assert!(total > 0.0, "the whole run took no time at all");
}

#[test]
fn the_transfer_ends_at_arrival_not_at_the_last_queued_write() {
    // Write commands are unacknowledged, so the central's queue empties
    // before the link has carried the bytes. The reported end must be the
    // sink's stamp — which is what `server-stamped` claims.
    let mut scene = scene(32 * 1024);
    assert!(scene.run(20_000, clock));
    let report = scene.report();
    assert_eq!(report.confirmation, "server-stamped");

    let arrival = scene
        .sink()
        .counters()
        .last_byte_ms
        .expect("the sink stamped an arrival");
    let start = report.total_ms.expect("a total") - report.transfer_ms.expect("a transfer");
    // total_ms is measured from the run's start (0.0 on this clock), so the
    // arrival stamp and the reported end are the same instant.
    assert!(
        (start + report.transfer_ms.unwrap() - arrival).abs() < 1e-9,
        "the report ended the transfer at {} but the last byte landed at {arrival}",
        start + report.transfer_ms.unwrap()
    );
}

#[test]
fn throughput_is_scoped_to_the_transfer_segment() {
    let mut scene = scene(32 * 1024);
    assert!(scene.run(20_000, clock));
    let report = scene.report();
    let landed = report.bytes_received.expect("a received count") as f64;
    let expected = landed / 1024.0 / (report.transfer_ms.unwrap() / 1000.0);
    let actual = report.throughput_kb_s.expect("a throughput");
    assert!(
        (actual - expected).abs() < 1e-6,
        "throughput {actual} is not the transfer segment's rate {expected}"
    );
    // Scoping matters: over the whole run the figure would be lower, and
    // the difference is exactly the setup latency the waterfall shows.
    let over_whole_run = landed / 1024.0 / (report.total_ms.unwrap() / 1000.0);
    assert!(actual >= over_whole_run);
}

#[test]
fn the_chunk_size_follows_the_negotiated_mtu() {
    let mut scene = scene(8 * 1024);
    assert!(scene.run(20_000, clock));
    let report = scene.report();
    // The server offers 512 and the client asks for 517, so 512 is the
    // meeting point; a chunk is that less the ATT write header.
    assert_eq!(report.mtu, 512);
    assert_eq!(report.chunk_bytes, 509);
    assert_eq!(report.chunks_sent, (8 * 1024_u32).div_ceil(509));
}

#[test]
fn an_acknowledged_run_also_delivers_every_byte() {
    let mut scene = ThroughputScene::new(
        SINK,
        CENTRAL,
        BulkOptions {
            total_bytes: 8 * 1024,
            with_response: true,
            ..BulkOptions::default()
        },
    );
    assert!(
        scene.run(20_000, clock),
        "the acknowledged run did not finish: {:?}",
        scene.report()
    );
    let report = scene.report();
    assert!(report.with_response);
    assert_eq!(report.bytes_received, Some(8 * 1024));
    // One request at a time, so an acknowledged run takes at least a link
    // turn per chunk — the reason the two modes are worth comparing.
    assert!(
        report.transfer_ms.unwrap() >= f64::from(report.chunks_sent),
        "{report:?}"
    );
}

#[test]
fn the_sink_counts_what_arrives_and_stamps_when() {
    let mut scene = scene(4 * 1024);
    assert!(scene.run(20_000, clock));
    let counters = scene.sink().counters();
    assert_eq!(counters.bytes, 4 * 1024);
    assert_eq!(counters.expected, 4 * 1024);
    assert!(counters.first_byte_ms.is_some());
    assert!(counters.last_byte_ms >= counters.first_byte_ms);
}

#[test]
fn a_peer_that_never_advertises_is_recorded_as_a_failed_measurement() {
    // No sink on the medium at all: the central scans, hears nothing, and
    // the watchdog names the segment it died in. An empty chart and a chart
    // of three failures must not look the same.
    let mut runner = BulkCentral::new(
        SINK,
        BulkOptions {
            total_bytes: 1024,
            timeout_ms: 500.0,
            ..BulkOptions::default()
        },
    );
    let mut link = Link::new();
    let channel = link.add_device(CENTRAL);
    for packet in runner.start(0.0) {
        let _ = channel.inject_host_packet(packet);
    }
    for step in 1..2_000 {
        let now = clock(step);
        for packet in runner.step(now) {
            let _ = channel.inject_host_packet(packet);
        }
        link.tick();
        while let Some(packet) = channel.poll_controller_packet() {
            for out in runner.on_packet(&packet, now) {
                let _ = channel.inject_host_packet(out);
            }
        }
        if runner.is_finished() {
            break;
        }
    }
    let report = runner.report();
    assert_eq!(report.phase, "failed");
    let failure = report.failure.expect("a reason");
    assert!(failure.contains("discover"), "{failure}");
    assert_eq!(report.bytes_sent, 0);
    assert_eq!(report.bytes_received, None);
}

#[test]
fn a_peer_without_the_bulk_service_fails_rather_than_hanging() {
    // A peripheral that is a perfectly good GATT server but not a benchmark
    // sink. The run must say so, not sit in "negotiate" until the watchdog.
    use crate::device::host::LeHost;

    let mut link = Link::new();
    let peer_channel = link.add_device(SINK);
    let central_channel = link.add_device(CENTRAL);

    let mut device = VirtualDevice::new("not-a-sink", SINK, AddressType::Public);
    device.gatt_db.add_service(0x180Fu16, true);
    device.gatt_db.add_characteristic(
        0x2A19u16,
        CharacteristicProperties(CharacteristicProperties::READ),
        vec![0x64],
        AttributePermissions::read_only(),
    );
    let mut host = LeHost::new();
    for packet in host
        .start_advertising(&device, &[0x180F])
        .expect("bring-up")
    {
        let _ = peer_channel.inject_host_packet(packet);
    }

    let mut runner = BulkCentral::new(
        SINK,
        BulkOptions {
            total_bytes: 1024,
            timeout_ms: 5_000.0,
            ..BulkOptions::default()
        },
    );
    for packet in runner.start(0.0) {
        let _ = central_channel.inject_host_packet(packet);
    }
    for step in 1..5_000 {
        let now = clock(step);
        for packet in runner.step(now) {
            let _ = central_channel.inject_host_packet(packet);
        }
        link.tick();
        while let Some(packet) = peer_channel.poll_controller_packet() {
            if let Ok(replies) = host.handle_packet(&mut device, &packet) {
                for reply in replies {
                    let _ = peer_channel.inject_host_packet(reply);
                }
            }
        }
        while let Some(packet) = central_channel.poll_controller_packet() {
            for out in runner.on_packet(&packet, now) {
                let _ = central_channel.inject_host_packet(out);
            }
        }
        if runner.is_finished() {
            break;
        }
    }
    let report = runner.report();
    assert_eq!(report.phase, "failed");
    let failure = report.failure.expect("a reason");
    assert!(
        failure.contains("no Bulk Transfer data characteristic"),
        "{failure}"
    );
    // The setup segments were still measured — a failure is a data point,
    // not a hole.
    assert!(report.discover_ms.is_some());
    assert!(report.connect_ms.is_some());
}

#[test]
fn options_come_from_json_and_are_clamped() {
    let parsed = BulkOptions::from_json(r#"{"total_bytes":1024,"with_response":true}"#);
    assert_eq!(parsed.total_bytes, 1024);
    assert!(parsed.with_response);
    assert_eq!(parsed.window_chunks, DEFAULT_WINDOW_CHUNKS);

    // Nonsense is the defaults, not an error: a misspelled setting must not
    // stop a benchmark from running.
    assert_eq!(BulkOptions::from_json("not json"), BulkOptions::default());

    let clamped = BulkOptions {
        total_bytes: 0,
        window_chunks: 0,
        timeout_ms: -1.0,
        ..BulkOptions::default()
    }
    .sane();
    assert_eq!(clamped.total_bytes, 1);
    assert_eq!(clamped.window_chunks, 1);
    assert_eq!(clamped.timeout_ms, 100.0);
}

#[test]
fn the_granular_link_knobs_default_to_the_old_fast_bundle() {
    // The defaults must reproduce, byte for byte, the three commands the fast
    // path used to send unconditionally — a change here would silently move
    // every existing benchmark's baseline.
    let o = BulkOptions::default();
    let cmds = fast_link_commands(0x0040, &o);
    assert_eq!(cmds.len(), 3, "PHY, then DLE, then connection update");
    // command() prefixes H4 type 0x01 and a 1-byte param length; assert on the
    // opcode + parameters that follow.
    assert_eq!(&cmds[0][1..3], &LE_SET_PHY);
    assert_eq!(&cmds[0][4..], &[0x40, 0x00, 0x00, 0x07, 0x07, 0x00, 0x00]);
    assert_eq!(&cmds[1][1..3], &LE_SET_DATA_LENGTH);
    assert_eq!(&cmds[1][4..], &[0x40, 0x00, 0xFB, 0x00, 0x48, 0x08]);
    assert_eq!(&cmds[2][1..3], &LE_CONNECTION_UPDATE);
    assert_eq!(
        &cmds[2][4..],
        &[
            0x40, 0x00, 0x06, 0x00, 0x0C, 0x00, 0x00, 0x00, 0x90, 0x01, 0x00, 0x00, 0x00, 0x00
        ]
    );
}

#[test]
fn each_granular_knob_can_be_set_or_switched_off_independently() {
    // 2M-only PHY, no DLE, a custom 30–40 ms interval: exactly one PHY command
    // and one connection update, no data-length command.
    let o = BulkOptions {
        phy_mask: 0x02,
        tx_octets: 0,
        conn_interval_min: 24,
        conn_interval_max: 32,
        ..BulkOptions::default()
    }
    .sane();
    let cmds = fast_link_commands(0x0040, &o);
    assert_eq!(cmds.len(), 2, "PHY and interval only — DLE is off");
    assert_eq!(&cmds[0][1..3], &LE_SET_PHY);
    assert_eq!(&cmds[0][4..], &[0x40, 0x00, 0x00, 0x02, 0x02, 0x00, 0x00]);
    assert_eq!(&cmds[1][1..3], &LE_CONNECTION_UPDATE);
    assert_eq!(&cmds[1][4..8], &[0x40, 0x00, 0x18, 0x00]); // min 24 = 0x0018
    assert_eq!(&cmds[1][8..10], &[0x20, 0x00]); // max 32 = 0x0020

    // Everything off: the fast path sends nothing, same as a bare link.
    let none = BulkOptions {
        phy_mask: 0,
        tx_octets: 0,
        conn_interval_max: 0,
        ..BulkOptions::default()
    }
    .sane();
    assert!(fast_link_commands(0x0040, &none).is_empty());
}

#[test]
fn the_granular_knobs_come_from_json_and_are_range_checked() {
    let parsed = BulkOptions::from_json(
        r#"{"phy_mask":2,"tx_octets":100,"conn_interval_min":10,"conn_interval_max":20}"#,
    );
    assert_eq!(parsed.phy_mask, 0x02);
    assert_eq!(parsed.tx_octets, 100);
    assert_eq!(parsed.conn_interval_max, 20);

    // A stray PHY bit is masked off; an out-of-range octet count and interval
    // are clamped; a min above the max is pulled down to it.
    let clamped = BulkOptions {
        phy_mask: 0xF2,
        tx_octets: 9000,
        conn_interval_min: 5000,
        conn_interval_max: 40,
        ..BulkOptions::default()
    }
    .sane();
    assert_eq!(clamped.phy_mask, 0x02, "only the three PHY bits survive");
    assert_eq!(clamped.tx_octets, 251, "octets clamped to the DLE ceiling");
    assert_eq!(clamped.conn_interval_max, 40);
    assert_eq!(clamped.conn_interval_min, 40, "min pulled down to the max");
}

#[test]
fn the_report_serialises_with_every_field_a_page_renders() {
    let mut scene = scene(4 * 1024);
    assert!(scene.run(20_000, clock));
    let json: serde_json::Value =
        serde_json::from_str(&scene.report_json()).expect("the report is JSON");
    for key in [
        "phase",
        "complete",
        "failure",
        "peer",
        "requested_bytes",
        "bytes_sent",
        "bytes_received",
        "chunk_bytes",
        "mtu",
        "discover_ms",
        "connect_ms",
        "negotiate_ms",
        "transfer_ms",
        "total_ms",
        "throughput_kb_s",
        "confirmation",
        "with_response",
    ] {
        assert!(json.get(key).is_some(), "the report has no {key}");
    }
}

#[test]
fn a_second_run_against_the_same_sink_starts_from_zero() {
    // The page runs the benchmark ten or thirty times in a row. Each run
    // builds a fresh scene, but the BEGIN write is what guarantees a reused
    // sink would not carry the previous run's count into the next.
    let mut sink = BulkSink::new("sink", SINK);
    let shared = sink.shared.clone();
    {
        let mut state = lock(&shared);
        state.bytes = 999;
        state.chunks = 7;
    }
    let mut begin = vec![control_op::BEGIN];
    begin.extend_from_slice(&512u32.to_le_bytes());
    let mut handler = ControlHandler(shared.clone());
    handler
        .on_write(&mut sink.device.gatt_db, &begin)
        .expect("begin is accepted");
    let counters = sink.counters();
    assert_eq!(counters.bytes, 0);
    assert_eq!(counters.chunks, 0);
    assert_eq!(counters.expected, 512);
}

/// Every ACL packet the runner hands out is charged against the controller's
/// buffers exactly once.
///
/// This is the invariant that a simulated run cannot check on its own. The
/// scene's link has no buffer pool and never sends `Number Of Completed
/// Packets`, so a packet charged twice costs nothing here — it cost the whole
/// transfer on real silicon. `step` charges its own output and `on_packet`
/// charges its own; when `on_packet` also charged what `step` returned, every
/// streamed fragment was billed twice, `acl_outstanding` climbed past the
/// pool and the budget sat at zero forever. The run reported "stalled in
/// transfer — 0 of 16384 bytes" while the link was perfectly healthy.
///
/// Counting the packets as they leave is the only way to see it: the runner's
/// own tally is the thing under test, so it cannot be the reference.
#[test]
fn each_acl_packet_is_charged_exactly_once() {
    let mut scene = scene(4096);
    let mut handed_out = 0usize;
    let mut step = 0usize;
    // Charging is a no-op until the controller reports its pool, and the
    // simulated link never does. A pool far larger than the run needs turns
    // the accounting on without ever letting the budget bind, so the run
    // still finishes and the tally is purely a count of what went out.
    scene.runner.acl_total = Some(u16::MAX);

    // `tick`, unrolled, so the ACL packets can be counted on their way past.
    while step < 4000 && !scene.runner.is_finished() {
        let now = clock(step);
        if !scene.started {
            for packet in scene.sink.start_commands() {
                let _ = scene.sink_channel.inject_host_packet(packet);
            }
            for packet in scene.runner.start(now) {
                handed_out += acl_count(&packet);
                let _ = scene.central_channel.inject_host_packet(packet);
            }
            scene.started = true;
        }
        for packet in scene.sink.poll() {
            let _ = scene.sink_channel.inject_host_packet(packet);
        }
        for packet in scene.runner.step(now) {
            handed_out += acl_count(&packet);
            let _ = scene.central_channel.inject_host_packet(packet);
        }

        scene.link.tick();

        while let Some(packet) = scene.sink_channel.poll_controller_packet() {
            for out in scene.sink.on_packet(&packet, now) {
                let _ = scene.sink_channel.inject_host_packet(out);
            }
        }
        while let Some(packet) = scene.central_channel.poll_controller_packet() {
            for out in scene.runner.on_packet(&packet, now) {
                handed_out += acl_count(&out);
                let _ = scene.central_channel.inject_host_packet(out);
            }
        }
        scene.runner.note_server(scene.sink.counters());
        step += 1;
    }

    assert!(
        scene.runner.is_finished(),
        "the run should finish so there is something to have charged"
    );
    assert!(handed_out > 0, "the run should have sent ACL packets");
    assert_eq!(
        scene.runner.acl_total,
        Some(u16::MAX),
        "the simulated controller should not have reported a pool of its own"
    );
    // Nothing credits them back in simulation, so the tally is the count.
    assert_eq!(
        scene.runner.acl_outstanding as usize, handed_out,
        "charged {} for {handed_out} ACL packets actually handed to the controller",
        scene.runner.acl_outstanding
    );
}

/// One if this is an ACL packet, zero otherwise.
fn acl_count(packet: &[u8]) -> usize {
    usize::from(packet.first() == Some(&crate::transport::h4_type::HCI_ACL_DATA))
}
