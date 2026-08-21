# Simble

[![CI](https://github.com/schilit/simble/actions/workflows/ci.yml/badge.svg)](https://github.com/schilit/simble/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

**Simble creates virtual Bluetooth devices for testing.** Spin up a simulated heart-rate
monitor, keyboard, LE Audio earbud, hands-free car kit, or media remote — and connect to it
from the Android emulator, from test code, or (with a USB dongle) from a real phone. No
hardware to charge, pair, or lose: every device is defined in code, behaves the same way
every run, and can misbehave on command when that's what your test needs.

Simble is written in pure Rust, runs everywhere `cargo` does, and is a native companion to
[netsim](https://android.googlesource.com/platform/tools/netsim), the Android emulator's
network simulator.

---

## What can I do with it?

- **Test Android apps against Bluetooth accessories that don't exist yet** — or that you
  don't want a drawer full of. Your app in the Android emulator, Simble as the accessory,
  netsim as the radio between them.
- **Reproduce the unreproducible.** A peripheral that drops the connection mid-notification,
  sends a stale pairing value, or advertises malformed data — real accessories won't
  misbehave on cue; simulated ones will, identically, every run.
- **Exercise the whole stack, not a mock.** Simble speaks real HCI, L2CAP, ATT/GATT, SMP
  pairing (Legacy and Secure Connections), SDP, RFCOMM, HFP, A2DP/AVDTP, AVRCP, and HID —
  what connects to it is talking to a real protocol implementation, packet by packet.
- **Reach real hardware when you want it.** With a USB Bluetooth dongle, a Simble device
  advertises over real RF and your actual phone can scan, connect, and pair with it.

## What devices come built in?

Ready-made simulated devices: **heart-rate monitor**, **keyboard**, **mouse**,
**Eddystone and iBeacon beacons** — plus the full profile catalog to build your own:
Battery, Device Information, LE Audio (BAP, ASCS, PACS, volume/input control, broadcast
scan, media control, hearing access, and the coordinator profiles), Apple ANCS/AMS,
Classic audio and telephony (A2DP, AVRCP, HFP), Classic HID, and Bluetooth 6.0
**Channel Sounding** distance ranging with **AoA/AoD** direction finding.

---

## Quick start 1: talk to the Android emulator (netsim)

Simble connects to netsim over a WebSocket, naming its device right in the URL. This needs
the **canary-channel emulator** (37.2.5+) — the stable emulator's netsim doesn't have the
WebSocket endpoint yet:

```bash
# One-time: install the canary-channel emulator package
~/Library/Android/sdk/cmdline-tools/latest/bin/sdkmanager --channel=3 emulator

# Start netsim with the WebSocket endpoint on
# (--no-shutdown keeps it alive while no devices are connected)
~/Library/Android/sdk/emulator/netsimd --logtostderr --no-shutdown --ws-port 7681

# Prove the pipe works: one Simble device, HCI round trip
cargo run --example netsim_smoke

# Two Simble devices discovering each other through the simulated radio
cargo run --example netsim_two_devices

# See who's on the air
~/Library/Android/sdk/emulator/netsim devices
```

**Try it in your browser** — with `netsimd` running locally as above, Simble itself runs
in the page (compiled to WebAssembly) and joins the simulation:

- **Beacon scanner**: https://schilit.github.io/simble/scanner/ — live scan of everything
  on the simulated air
- **Scripted heart-rate monitor**: https://schilit.github.io/simble/hrm/ — a running
  Simble whose device is defined by an editable Rhai script in the page; edit, hit Run,
  and watch it change in the scanner tab

Open both side by side for the full loop: rename the device in the HRM tab's script and
see the new name appear in the scanner. (Links go live once GitHub Pages is enabled for
this repo; until then, build locally and serve `web/` — instructions in that directory.)

Any device you create this way appears in netsim alongside emulator instances — name and
address come straight from the connection URL:

```
ws://localhost:7681/v1/websocket/bt?name=my-hrm&address=11:22:33:44:55:01
```

## Quick start 2: talk to a real phone (USB dongle)

Plug in a USB Bluetooth dongle (macOS's built-in radio is not accessible — a generic
CSR-style dongle works out of the box), then:

```bash
# Advertise as "Simble HRM"; scan and connect from your phone with nRF Connect
cargo run --example usb_hrm

# Or pick a specific dongle
cargo run --example usb_hrm -- 0a12:0001
```

If opening the dongle fails on macOS, check whether the OS claimed it; on Linux you'll need
device permissions (a udev rule, or `sudo`).

## Quick start 3: use it as a library

```rust
use simble::devices::HeartRateMonitor;
use simble::types::Address;

let addr: Address = "F1:F2:F3:F4:F5:F6".parse()?;
let mut hrm = HeartRateMonitor::new("MyHeartRateMonitor", addr);
let notification = hrm.send_heart_rate(78);   // a real ATT notification PDU
```

Or the Android-flavored API, if that's the vocabulary you know:

```rust
use simble::android::gatt_server::BluetoothGattServer;
use simble::android::gatt_service::{BluetoothGattService, BluetoothGattCharacteristic};
// BluetoothGattServer / addService / notifyCharacteristicChanged —
// the android.bluetooth shapes, backed by Simble's real stack.
```

Scripts work too — Simble embeds the [Rhai](https://rhai.rs) scripting engine with the same
Android-shaped API, so device behavior can live in a text file instead of a rebuild. This is
**Rhai** (a Rust-flavored scripting language), not Rust — no compiler involved:

```rhai
// heart_rate.rhai — Rhai script, evaluated at runtime
let server = android::BluetoothGattServer("Scripted HRM");
let hrs = android::BluetoothGattService(uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY);
let chr = android::BluetoothGattCharacteristic(
    uuid::HEART_RATE_MEASUREMENT,
    android::PROPERTY_READ | android::PROPERTY_NOTIFY,
    android::PERMISSION_READ
);
chr.set_value([0x00, 72]);
hrs.add_characteristic(chr);
server.add_service(hrs);
```

---

## Examples

| Example | What it shows |
|---|---|
| `netsim_smoke` | One Simble device connected to netsim, HCI round trip |
| `netsim_two_devices` | Two devices seeing each other through the simulated radio |
| `usb_hrm` | A heart-rate monitor on real RF via a USB dongle |
| `heart_rate_monitor`, `ble_keyboard` | Library-level virtual devices |
| `channel_sounding` | Bluetooth 6.0 distance-ranging math |

## Verifying a change

```bash
cargo test --all-targets --all-features
```

The suite is 850+ tests covering every protocol layer, largely ported from Bumble's test
suite (see Acknowledgments) plus spec-derived coverage of its gaps.

---

## Acknowledgments

Simble is inspired by, and ports test coverage from,
[Bumble](https://github.com/google/bumble), Google's Python Bluetooth stack. Where a Simble
test suite is a direct port of a Bumble test file, that provenance is noted here rather than
repeated per-file in the source.

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
