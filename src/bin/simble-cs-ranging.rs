// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Bluetooth 6.0 Channel Sounding, measured against a known truth.
//!
//! Runs a tag and a locator on the in-process simulated radio, moves the tag,
//! and prints what each of the two ranging methods makes of it.
//!
//! This used to print a table computed by taking a distance, turning it into a
//! phase with `Δφ = 4πΔf·d/c`, and then inverting that same equation — which
//! is a demonstration of arithmetic, not of ranging. Everything below comes
//! out of the stack: the RSSI from HCI Read RSSI, the tones from LE CS
//! Subevent Result events and from Ranging Data the tag notifies over the
//! Ranging Service. Neither host is told where the other is.

use simble::controller::propagation::Position;
use simble::device::RangingScene;
use simble::types::Address;

/// Ticks to let the pair connect, discover RAS, subscribe, and start ranging.
const SETUP_TICKS: usize = 40;

/// Ticks to settle at each new position. The RSSI estimate averages a window,
/// so it needs time to forget where the tag used to be.
const SETTLE_TICKS: usize = 40;

fn main() {
    let tag: Address = "CC:1E:57:00:00:0A".parse().expect("valid address");
    let locator: Address = "CC:1E:57:00:00:0B".parse().expect("valid address");
    let mut scene = RangingScene::new(tag, locator);

    println!("Simble — Bluetooth 6.0 Channel Sounding versus RSSI path loss\n");
    let room = scene.path_loss();
    println!(
        "room:     path-loss exponent {:.1}, shadowing σ {:.1} dB, tag TX {:.0} dBm",
        room.path_loss_exponent, room.shadowing_sigma_db, room.tx_power_dbm
    );
    let assumed = scene.rssi_assumptions();
    println!(
        "locator:  assumes exponent {:.1} and −{:.1} dBm at 1 m — it cannot know the room\n",
        assumed.path_loss_exponent,
        assumed.reference_rssi_dbm.abs()
    );

    for _ in 0..SETUP_TICKS {
        scene.tick();
    }

    println!("  truth      RSSI says   error        CS says   error    ± std err");
    println!("  ---------------------------------------------------------------");
    for truth in [0.5, 1.0, 2.5, 5.0, 10.0, 20.0, 30.0] {
        scene.set_tag_position(Position::new(truth, 0.0));
        for _ in 0..SETTLE_TICKS {
            scene.tick();
        }
        let status: serde_json::Value =
            serde_json::from_str(&scene.status_json()).expect("status is JSON");
        let rssi = status["rssi"]["distance_m"].as_f64();
        let cs = status["cs"]["distance_m"].as_f64();
        let std_error = status["cs"]["std_error_m"].as_f64();
        println!(
            "  {:5.1} m   {:>8}   {:>7}   {:>8}   {:>7}   {:>8}",
            truth,
            metres(rssi),
            error(rssi, truth),
            metres(cs),
            error(cs, truth),
            std_error.map_or("—".to_string(), |e| format!("±{:.2} m", e)),
        );
    }

    let combined = status_counter(&scene);
    println!(
        "\n{combined} procedures combined. Each needed the tag's own tones to cross \
         the link over the\nRanging Service first: neither controller can compute a \
         distance on its own."
    );
}

/// An optional distance, or a dash.
fn metres(value: Option<f64>) -> String {
    value.map_or("—".to_string(), |v| format!("{v:.2} m"))
}

/// The signed error against `truth`.
fn error(value: Option<f64>, truth: f64) -> String {
    value.map_or("—".to_string(), |v| format!("{:+.2} m", v - truth))
}

/// How many procedures produced a distance.
fn status_counter(scene: &RangingScene) -> u64 {
    serde_json::from_str::<serde_json::Value>(&scene.status_json())
        .ok()
        .and_then(|status| status["cs"]["procedures_combined"].as_u64())
        .unwrap_or(0)
}
