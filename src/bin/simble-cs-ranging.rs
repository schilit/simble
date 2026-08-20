// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Standalone CLI Sample App: Bluetooth 6.0 Channel Sounding Ranging.

use simble::VirtualDevice;
use simble::cs::{CsConfig, CsMainMode, CsRole, compute_pbr_distance};
use simble::profiles::RangingService;
use simble::types::{Address, AddressType};

fn main() {
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║       Simble Bluetooth 6.0 Channel Sounding Ranging (CS)     ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    let addr = Address::from_be_bytes([0x00, 0x1A, 0x7D, 0xDA, 0x71, 0x13]);
    let mut dev = VirtualDevice::new("CS-Reflector", addr, AddressType::Random);

    // Register standard Ranging Service (0x185B)
    let ras = RangingService::register(&mut dev.gatt_db);
    println!("• Device Name       : {}", dev.name);
    println!("• Address           : {}", dev.address);
    println!("• RAS Features      : Real-Time Ranging Data (0x01)");
    println!(
        "• RAS Control Handle: 0x{:04X}\n",
        ras.control_point_value_handle
    );

    // Setup CS configuration
    let config = CsConfig {
        config_id: 1,
        role: CsRole::Initiator,
        main_mode: CsMainMode::RttAndPbr,
        min_steps: 4,
        max_steps: 16,
        channel_map: [0xFF; 10],
    };
    println!("CS Session Configuration:\n{config:#?}\n");

    // Execute Phase-Based Ranging (PBR) estimation at various simulated distance points
    println!("Simulating Phase-Based Ranging across 40 MHz channel hop:");
    let simulated_distances = [0.5, 1.2, 2.5, 5.0, 10.0]; // Target meters
    const SPEED_OF_LIGHT: f32 = 299_792_458.0;
    let freq_delta_hz = 40_000_000.0; // 40 MHz

    for d in simulated_distances {
        // Delta phi = (4 * pi * Delta f * d) / c
        let phase_delta_rad = (4.0 * std::f32::consts::PI * freq_delta_hz * d) / SPEED_OF_LIGHT;
        let estimated = compute_pbr_distance(freq_delta_hz, phase_delta_rad);
        println!(
            "  Actual: {d:4.1} m  |  Phase Delta: {phase_delta_rad:6.3} rad  |  PBR Estimated: {estimated:5.2} m"
        );
    }

    println!("\nSimulation complete.");
}
