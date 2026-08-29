# SimBLE

[![CI](https://github.com/schilit/simble/actions/workflows/ci.yml/badge.svg)](https://github.com/schilit/simble/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

> **Preview.** An early preview release — the public API is unstable and may
> change between preview versions. On crates.io the crate is **`simble-stack`**
> (the bare `simble` name was taken in 2019 by an unrelated crate); the library
> itself keeps the name `simble`, so you depend on `simble-stack` but write
> `use simble::…`.
>
> ```toml
> [dependencies]
> simble-stack = "0.1.0-preview.1"
> ```

**SimBLE creates simulated Bluetooth scenes for testing scenarios.** A scene can contain *one device* or
*several interacting devices*. No hardware to charge, pair, or lose: every device is defined in
code, behaves the same way every run, and can misbehave on command when that's what your test
needs.

Inspired by [Bumble](https://github.com/google/bumble) and
[NimBLE](https://github.com/apache/mynewt-nimble), SimBLE supports scripting with
[Rhai](https://rhai.rs) for device definitions and tests.
SimBLE is written in pure Rust and works with
[netsim](https://android.googlesource.com/platform/tools/netsim), the Android emulator's
Bluetooth radio simulator.

### Choose a surface

| Surface | Use it when | Start here |
|---|---|---|
| **Web** | You want no-install interactive examples and device showcases | **[Start with the web demos](https://schilit.github.io/simble/)** |
| **MCP** | An AI agent is creating, running, and testing devices | [Quick start 1](#quick-start-1-drive-it-from-an-ai-agent-mcp) |
| **Native** | You need Rust integration, CI fixtures, netsim, or a USB dongle | [Quick start 4](#quick-start-4-use-it-as-a-library) |

All three use the same host stack with different transports.

---

## What can I do with it?

**Web** — interactively build a device in the [Playground](https://schilit.github.io/simble/playground/),
inspect it, and try it in a browser.

**MCP** — in a chat with SimBLE available, say:

> *“Create a simulated heart-rate monitor, connect to it, and check that its rate stays below 200 bpm for five seconds.”*

**Native** — run reproducible device tests locally or in CI, integrate SimBLE into Rust tests,
or connect a scene to netsim or a USB dongle.

Whichever surface you choose, you can also:

- **Test Android apps against Bluetooth accessories that don't exist yet** — or that you
  don't want a drawer full of. Your app in the Android emulator, SimBLE as the accessory,
  netsim as the radio between them.
- **Reproduce the unreproducible.** A peripheral that drops the connection mid-notification,
  sends a stale pairing value, or advertises malformed data — real accessories won't
  misbehave on cue; simulated ones will, identically, every run.
- **Exercise LE protocol layers.** SimBLE implements HCI, L2CAP, ATT/GATT, and SMP pairing
  (Legacy and Secure Connections). Classic (BR/EDR) has a simulated controller too — inquiry,
  paging and ACL — so two hosts meet over SDP and RFCOMM in a scene; the profiles above that
  (HFP, A2DP/AVDTP, AVRCP, HID) are still library-only, and there is no pairing or SCO audio
  yet. See [what each peripheral type needs](docs/peripheral-support.md) for
  what is *scriptable* versus *library-only*.
- **Reach real hardware when you want it.** With a USB Bluetooth dongle, a SimBLE device
  advertises over real RF and your actual phone can scan, connect, and pair with it.

## Choose a controller

A **surface** is how you use SimBLE (MCP, Web, or Native). A **scene** is the collection of
simulated devices and interactions. A **controller** is where that scene runs; choose it
independently of the surface.

<table width="100%">
  <colgroup>
    <col width="16%">
    <col width="28%">
    <col width="28%">
    <col width="28%">
  </colgroup>
  <thead>
    <tr>
      <th width="16%">Controller</th>
      <th width="28%">MCP</th>
      <th width="28%">Web</th>
      <th width="28%">Native</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <th>In-process</th>
      <td>Default</td>
      <td>Default</td>
      <td>Tests and CI</td>
    </tr>
    <tr>
      <th>netsim</th>
      <td><code>run_on("netsim")</code></td>
      <td>netsim over WebSocket</td>
      <td>netsim examples</td>
    </tr>
    <tr>
      <th>USB dongle</th>
      <td>Not available yet</td>
      <td>Not available</td>
      <td><code>usb_hrm</code> example</td>
    </tr>
  </tbody>
</table>

The in-process controller needs no external setup and is deterministic. netsim requires local
`netsimd` with its WebSocket endpoint: MCP adds peripherals and the Android emulator is the
central; Web connects to netsim over WebSocket. The
[controller ladder](https://schilit.github.io/simble/controllers/) explains the fidelity and
setup trade-offs in more detail.

## SimBLE MCP — the agent-first surface

SimBLE ships an MCP server for stateful device construction and testing in an agent
conversation. Once configured, the client works in a live scene: a session-scoped set of peripherals, plus a
central and scanner to exercise them. The scene persists across calls, so an agent can build it,
drive interactions, and inspect the result. `run_on` chooses its controller — the in-process
simulator, `netsim`, or `usb` with a `vid:pid` dongle selector; changing controllers starts a
fresh live scene.

It speaks stdio by default, or `simble mcp --ws-server [PORT]` (default 7682) to serve the same
protocol over WebSocket, one client at a time with a fresh scene per connection. A `subscribe`
carrying `op` and `value` arms a watch that pushes an unsolicited `notifications/message` the
first time the condition breaks, rather than waiting to be asked.

| Tools | What they do |
|---|---|
| `example`, `lookup` | Learn the API and the assigned numbers without leaving the session |
| `lint`, `run_test` | Compile a script, or run it and check every `assert(...)` |
| `run_on`, `add_peripheral`, `add_central`, `tick`, `status`, `scan` | Choose the controller, build the scene (devices *and* the clients that drive them), drive the clock, see it as a whole or as a scanner hears it |
| `connect`, `read`, `write`, `assert` | Drive a central against a peripheral, one call at a time |
| `subscribe`, `assert_over` | Monitor a value across a time window |

`example` serves 26 ready-to-run device scripts, `lookup` resolves SIG assigned numbers, and
`assert_over` subscribes, advances the clock, and fails on the first violating sample. Tool
output annotates 16-bit UUIDs with their SIG names; failures use `isError` for clients to detect.

A whole flow — *"build a heart-rate monitor and check HR stays under 200"* — is four calls:

```jsonc
example    {"name": "hrm"}        // → the Rhai script, ready to paste
add_peripheral {"script": "..."}  // → "added peripheral #0", its GATT as JSON
connect    {}                     // → the discovered services
assert_over {"uuid": "2A37", "op": "<", "value": 200, "seconds": 5}
// → "PASS — 2A37 byte 1 held < 200 across 30 samples over 5.0s (extreme 76)"
```

`add_central` is the other half: instead of scripting each interaction as a tool call, hand it a
client script and it connects, discovers, subscribes and asserts on its own.

```jsonc
add_central {"script": "let c = android::BluetoothGatt(\"Probe\"); c.connect(\"AA:BB:CC:00:00:01\");
             fn on_services_discovered(client) { client.subscribe(uuid::HEART_RATE_MEASUREMENT); }
             fn on_characteristic_changed(client, uuid, value) { assert(value[1] < 200, \"plausible\"); }"}
```

The scripts are the same artifact throughout: `simble FILE.rhai` runs one headless for CI
(exit 0 / 1), and `simble --no-run FILE.rhai` lints without running.

## Self-contained device scripts and tests

A short [Rhai](https://rhai.rs) script contains a complete device definition: its GATT services,
initial values, and behavior. Edit and re-run it without rebuilding Rust. Its API is
Android-shaped: `BluetoothGattServer`, `BluetoothGattService`, characteristics, and
notifications correspond to familiar `android.bluetooth` concepts.

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

Add `assert(...)` and that same self-contained file becomes a device test. Execution is bounded
and has no filesystem or network access; the script can run identically in local tests, netsim
fixtures, and CI.

The client half is scriptable too, and mirrors `android.bluetooth` the same way —
`BluetoothGatt` plus the `BluetoothGattCallback` methods as plain functions:

```rhai
let client = android::BluetoothGatt("HRM Client");
client.connect("AA:BB:CC:00:00:01");

fn on_services_discovered(client) {
    client.subscribe(uuid::HEART_RATE_MEASUREMENT);
}
fn on_characteristic_changed(client, uuid, value) {
    assert(value[1] > 30 && value[1] < 220, "a plausible heart rate");   // this is the test
}
```

Both halves run anywhere the library does: an in-page scene, a JSON scene file, the MCP scene,
or over netsim against a real stack.

## What devices come built in?

**Twenty-six ready-to-run device scripts** are served by the MCP `example` tool and can also be
used from the web pages or CLI. They include heart-rate, thermometer, thermostat,
environmental, battery, HID, cycling, pulse-oximeter, weight-scale, smart-lock, fitness,
volume-control, beacon, Fast Pair, Auracast, coordinated-set earbuds, and Channel Sounding
examples. Call `example` with no name to list them. A script can also load one by name with
`catalog::device("hrm")` — what comes back is the peripheral itself, so a test can build on a
shipped device instead of restating it.

Underneath sits the profile catalog to build your own: Battery, Device Information,
LE Audio (BAP, ASCS, PACS, volume/input control, broadcast scan, media control, hearing
access, and the coordinator profiles), Apple ANCS/AMS, and Bluetooth 6.0 **Channel
Sounding** distance ranging with **AoA/AoD** direction finding.

Two honest caveats. **LE Audio streaming works but has only been driven from simble and
Bumble** — the control plane (PACS, ASCS, volume control), CIS establishment and LC3 are all
implemented, and the [Audio page](https://schilit.github.io/simble/audio/) streams a file
over a real CIS end to end; a phone as the source is untested. **Classic (BR/EDR)** now runs in
a scene: the simulated controller does inquiry, paging and ACL routing, two hosts find each
other and exchange SDP and RFCOMM, and the [Car page](https://schilit.github.io/simble/car/)
carries HFP over a real simulated link. The initiator is checked against Bumble over netsim.
Still missing: **pairing and encryption** (no SSP, no link keys), **SCO/eSCO** — so HFP has
signalling but no audio — and A2DP/AVRCP/HFP/HID are not yet `ClassicHost` handlers, so no
scene can host a classic speaker or keyboard.
[`docs/peripheral-support.md`](docs/peripheral-support.md) tracks exactly what is
scriptable versus library-only.

---

## Quick start 1: drive it from an AI agent (MCP)

Start from a source checkout:

```bash
git clone https://github.com/schilit/simble.git
cd simble
```

Build and register the server:

```bash
cargo build --release --bin simble
claude mcp add simble -- "$PWD/target/release/simble" mcp
```

Then ask the agent, for example:

> **Create and test a device**
>
> *“Add a heart-rate monitor and check its rate stays under 200 for five seconds.”*

> **Explore a built-in device**
>
> *“Add the smart-lock example, connect to it, and show me its services and characteristics.”*

> **Build a scene**
>
> *“Add a heart-rate monitor and a scanner to a scene, advance the clock for five seconds, and show me what the scanner sees.”*

> **Use the Android emulator**
>
> *“Run this scene on netsim, add a thermometer, and tell me when my Android emulator can discover it.”*

For the first prompt, the agent calls `example` → `add_peripheral` → `connect` → `assert_over`
and reports PASS or FAIL. The in-process run is deterministic. See
[SimBLE MCP](#simble-mcp--the-agent-first-surface) for the full tool list.

## Quick start 2: talk to the Android emulator (netsim)

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
- **[Audio](https://schilit.github.io/simble/audio/)** — both halves of an LE Audio stream:
  drop in an audio file, watch the ASE handshake and CIS establishment, and hear the LC3
  frames the sink actually received, at the volume its GATT says

Open the Playground and the Scanner side by side for the full loop: rename the device in the
Playground script and see the new name appear in the scanner.

Any device you create this way appears in netsim alongside emulator instances — name and
address come straight from the connection URL:

```
ws://localhost:7681/v1/websocket/bt?name=my-hrm&address=11:22:33:44:55:01
```

## Quick start 3: talk to a real phone (USB dongle)

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

## Quick start 4: use it as a library

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
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo doc --no-deps --all-features
```

CI also builds the release CLI and runs the Rhai fixtures used by the web testing page. The
suite contains hundreds of tests across the protocol layers, including ports of Bumble tests
and spec-derived coverage.

---

## Acknowledgments

SimBLE is inspired by, and ports test coverage from,
[Bumble](https://github.com/google/bumble), Google's Python Bluetooth stack. Where a SimBLE
test suite is a direct port of a Bumble test file, that provenance is noted here rather than
repeated per-file in the source.

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
