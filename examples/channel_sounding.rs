// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Standalone example: Bluetooth 6.0 Channel Sounding Phase-Based Ranging (PBR).

use simble::cs::compute_pbr_distance;

fn main() {
    println!("=== Simble Bluetooth 6.0 Channel Sounding ===");

    // Phase slope across 40 MHz channel hop: 1.05 radians
    let freq_delta_hz = 40_000_000.0; // 40 MHz
    let phase_delta_rad = 1.047; // ~60 degrees

    let distance_meters = compute_pbr_distance(freq_delta_hz, phase_delta_rad);
    println!("Frequency Delta: {:.1} MHz", freq_delta_hz / 1_000_000.0);
    println!("Phase Delta: {phase_delta_rad:.3} rad");
    println!("Estimated Distance: {distance_meters:.3} meters");
}
