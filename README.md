# SimBLE

[![CI](https://github.com/schilit/simble/actions/workflows/ci.yml/badge.svg)](https://github.com/schilit/simble/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

**SimBLE creates virtual Bluetooth devices for testing.** Spin up a simulated heart-rate
monitor, keyboard, LE Audio earbud, hands-free car kit, or media remote — and connect to it
from the Android emulator, from test code, or (with a USB dongle) from a real phone. No
hardware to charge, pair, or lose: every device is defined in code, behaves the same way
every run, and can misbehave on command when that's what your test needs.

Inspired by [Bumble](https://github.com/google/bumble) and
[NimBLE](https://github.com/apache/mynewt-nimble), SimBLE exposes the
[Rhai](https://rhai.rs) scripting language to make device creation and testing a snap.
SimBLE is written in pure Rust and is a native companion to
[netsim](https://android.googlesource.com/platform/tools/netsim), the Android emulator's
network simulator.

**▶ Try it now in your browser: the [SimBLE Playground](https://schilit.github.io/simble/playground/)** —
write a device in Rhai and run it live (compiled to WebAssembly, no install).

---

## What can I do with it?

- **Test Android apps against Bluetooth accessories that don't exist yet** — or that you
  don't want a drawer full of. Your app in the Android emulator, SimBLE as the accessory,
  netsim as the radio between them.
- **Reproduce the unreproducible.** A peripheral that drops the connection mid-notification,
  sends a stale pairing value, or advertises malformed data — real accessories won't
  misbehave on cue; simulated ones will, identically, every run.
- **Exercise the whole stack, not a mock.** SimBLE speaks real HCI, L2CAP, ATT/GATT, SMP
  pairing (Legacy and Secure Connections), SDP, RFCOMM, HFP, A2DP/AVDTP, AVRCP, and HID —
  what connects to it is talking to a real protocol implementation, packet by packet.
- **Reach real hardware when you want it.** With a USB Bluetooth dongle, a SimBLE device
  advertises over real RF and your actual phone can scan, connect, and pair with it.

## Devices are scripts

A SimBLE device doesn't have to be Rust you compile — it can be a short
[Rhai](https://rhai.rs) script you edit and re-run in seconds. Rhai is a small, Rust-flavored
scripting language embedded in SimBLE, and it exposes an API that **mirrors the platform
Bluetooth framework you already know** — not an invented one. Today that's the
**Android-shaped** surface (`BluetoothGattServer`, `BluetoothGattService`, characteristics,
`notify…`) straight out of `android.bluetooth`; a **CoreBluetooth-shaped** surface
(`CBPeripheralManager` for macOS/iOS developers) is the planned sibling, built on the same
internal hook. Pick the vocabulary that matches the app you're testing against.

```rhai
// A heart-rate monitor, defined entirely in a text file:
let server = android::BluetoothGattServer("My HRM");
let hrs = android::BluetoothGattService(uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY);
let hr = android::BluetoothGattCharacteristic(
    uuid::HEART_RATE_MEASUREMENT,
    android::PROPERTY_READ | android::PROPERTY_NOTIFY,
    android::PERMISSION_READ
);
hr.set_value([0x00, 72]);            // flags + 72 bpm
hrs.add_characteristic(hr);
server.add_service(hrs);
```

The same script defines a **device** to run, or a **test** to check — add `assert(...)` and
you've written a test instead of a peripheral. No rebuild, sandboxed by construction (bounded
execution, no filesystem or network), and identical every run. The behavior lives in the
script; SimBLE's real protocol stack does the work underneath.

## AI-first testing

Because a device is a small script against an API that's already in every LLM's training data,
the natural way to make one is to ask:

> *"Write a test where a phone connects to a heart-rate monitor, the monitor drops the
> connection mid-notification, and the phone's re-read after reconnect gets the latest value."*

An LLM emits the Rhai, SimBLE runs it **deterministically** — so a generation mistake shows up
immediately and reproducibly, and the generate-check-fix loop actually converges instead of
chasing flaky failures. The validated script is itself the artifact: the same file ships into
netsim as a CI fixture, unchanged. No hand-translation between "the test the AI wrote" and "the
test CI runs."

This is the direction SimBLE is built for: describe the Bluetooth scenario you want in plain
language, get back a runnable, checkable, shippable test — and lean on the cases real hardware
can't stage on demand (the peripheral that misbehaves at exactly the wrong moment, reproducibly,
every time).

## What devices come built in?

Ready-made simulated devices: **heart-rate monitor**, **keyboard**, **mouse**,
**Eddystone and iBeacon beacons** — plus the full profile catalog to build your own:
Battery, Device Information, LE Audio (BAP, ASCS, PACS, volume/input control, broadcast
scan, media control, hearing access, and the coordinator profiles), Apple ANCS/AMS,
Classic audio and telephony (A2DP, AVRCP, HFP), Classic HID, and Bluetooth 6.0
**Channel Sounding** distance ranging with **AoA/AoD** direction finding.

---

## Quick start 1: talk to the Android emulator (netsim)

SimBLE connects to netsim over a WebSocket, naming its device right in the URL. This needs
the **canary-channel emulator** (37.2.5+) — the stable emulator's netsim doesn't have the
WebSocket endpoint yet:

```bash
# One-time: install the canary-channel emulator package
~/Library/Android/sdk/cmdline-tools/latest/bin/sdkmanager --channel=3 emulator

# Start netsim with the WebSocket endpoint on
# (--no-shutdown keeps it alive while no devices are connected)
~/Library/Android/sdk/emulator/netsimd --logtostderr --no-shutdown --ws-port 7681

# Prove the pipe works: one SimBLE device, HCI round trip
cargo run --example netsim_smoke

# Two SimBLE devices discovering each other through the simulated radio
cargo run --example netsim_two_devices

# See who's on the air
~/Library/Android/sdk/emulator/netsim devices
```

**Try it in your browser** — with `netsimd` running locally as above, SimBLE itself runs
in the page (compiled to WebAssembly) and joins the simulation. Start at the
[demo index](https://schilit.github.io/simble/), or jump straight in:

- **[Playground](https://schilit.github.io/simble/playground/)** — a free-form Rhai editor
  where the script *is* the device; Run it, watch the live GATT viewer, generate a device
  with AI, and Share a link that encodes your script in the URL
- **[API Explorer](https://schilit.github.io/simble/explorer/)** — fill in an `android::*`
  call, press Execute, and it emits one line of Rhai against a live session, teaching the
  syntax as you build a device click by click
- **[Scanner](https://schilit.github.io/simble/scanner/)** — live scan of everything on the
  simulated air, with decoded advertisements
- **[Scripted heart-rate monitor](https://schilit.github.io/simble/hrm/)** — a running
  SimBLE whose device is an editable Rhai script; edit, hit Run, and watch it change in the
  scanner tab
- **[Color Bulb](https://schilit.github.io/simble/lightbulb/)** — a PLAYBULB-style light: a
  Rhai peripheral with a writable RGB characteristic and a glowing bulb that reacts

Open the Playground and the Scanner side by side for the full loop: rename the device in the
Playground script and see the new name appear in the scanner.

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

The full crate API — every module, profile, and packet type — is documented at
**https://schilit.github.io/simble/doc/** (generated by `cargo doc`; build it locally with
`cargo doc --open`).

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
// the android.bluetooth shapes, backed by SimBLE's real stack.
```

Scripts work too — SimBLE embeds the [Rhai](https://rhai.rs) scripting engine with the same
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
| `netsim_smoke` | One SimBLE device connected to netsim, HCI round trip |
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

SimBLE is inspired by, and ports test coverage from,
[Bumble](https://github.com/google/bumble), Google's Python Bluetooth stack. Where a SimBLE
test suite is a direct port of a Bumble test file, that provenance is noted here rather than
repeated per-file in the source.

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
