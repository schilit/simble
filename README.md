# Simble

[![CI](https://github.com/schilit/simble/actions/workflows/ci.yml/badge.svg)](https://github.com/schilit/simble/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

**Simble** is a lightweight, zero-copy, memory-safe virtual Bluetooth Low Energy (BLE) Host Stack and Device Simulation Engine written in pure Rust.

Designed as an alternative to Python-based Bumble for simulation environments, Simble enables declarative modeling of virtual BLE peripherals, GATT servers and clients, L2CAP connection-oriented channels, Security Manager (SMP) pairing, LE Audio profiles, and Bluetooth 6.0 Channel Sounding.

---

## Key Features

- **Zero-Copy Serialization (`zerocopy`)**: Native in-place parsing and slicing of ATT, L2CAP, SMP, and HCI packets with zero heap reallocations.
- **Pure Rust Cryptographic Engine**: AES-128, AES-CMAC (RFC 4493), Resolvable Private Address resolution (`ah`), confirm generators (`c1`, `s1`, `f4`, `g2`), and Bumble-compatible `JsonKeyStore`.
- **Bluetooth 6.0 Channel Sounding (CS)**: High-accuracy Phase-Based Ranging (PBR) distance estimation ($\Delta d = \frac{c \cdot \Delta \phi}{4\pi \cdot \Delta f}$) and Ranging Service (`0x185B`).
- **Complete Profile Ecosystem**:
  - **Health & Device**: Heart Rate (`0x180D`), Battery (`0x180F`), Device Information (`0x180A`), Generic Attribute (`0x1801` with database hash).
  - **LE Audio**: Coordinated Set Identification (CSIP `0x1846`), Published Audio Capabilities (PACS `0x184E`), Volume Control (VCP `0x1844`).
  - **HID over GATT (HOGP)**: Virtual Keyboards and Mice with automated ASCII-to-HID report conversion.
- **REST & Web Management**: Built-in HTTP router for declarative multi-device provisioning and real-time attribute mutation.
- **Zero External Dependencies**: Compiles in milliseconds with standard `cargo build` and `cargo test` across Linux, macOS, and Windows.

---

## Architecture

```text
┌─────────────────────────────────────────────────────────────┐
│                      Simble Virtual Device                  │
│  - Declarative GATT Database & Hash Engine (0x1801)         │
│  - SMP Security Manager & Persistent JSON KeyStore          │
│  - Profiles: HRS, BAS, DIS, CSIP, PACS, VCP, RAS, HID       │
│  - Bluetooth 6.0 Channel Sounding PBR Distance Estimator    │
└──────────────────────────────┬──────────────────────────────┘
                               │
            ┌──────────────────┴──────────────────┐
            │   L2CAP Reassembler & CoC Manager   │
            │   - Fixed Channels: ATT (0x04), SMP │
            │   - Dynamic CoC / EATT (0x0040-0x7F)│
            │   - Credit-Based Flow Control       │
            └──────────────────┬──────────────────┘
                               │
            ┌──────────────────┴──────────────────┐
            │   In-Memory HciChannel Transport    │
            │   (H4: 0x01 Cmd, 0x02 ACL, 0x04 Evt)│
            └─────────────────────────────────────┘
```

---

## Quick Start

### 1. Creating a Virtual Heart Rate Monitor

```rust
use simble::devices::HeartRateMonitor;
use simble::types::{Address, AddressType};

fn main() {
    let addr = Address::from_be_bytes([0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6]);
    let mut hrm = HeartRateMonitor::new("MyHeartRateMonitor", addr);

    // Update heart rate to 78 bpm and emit a notification
    let notification_pdu = hrm.send_heart_rate(78);
    println!("Emitted notification PDU: {notification_pdu:02X?}");
}
```

### 2. Performing Bluetooth 6.0 Phase-Based Ranging

```rust
use simble::cs::{compute_pbr_distance, CsStepResult};

fn main() {
    let steps = vec![
        CsStepResult { channel: 10, phase_rad: 0.15 },
        CsStepResult { channel: 20, phase_rad: 0.75 },
        CsStepResult { channel: 30, phase_rad: 1.35 },
    ];

    let estimate = compute_pbr_distance(&steps);
    println!("Estimated distance: {:.2} meters", estimate.estimated_distance_meters);
}
```

---

## Running Tests

Simble contains 75+ unit and integration tests ported from Bumble:

```bash
cargo test --all-targets --all-features
```

---

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
