# A scriptable phone: SimBLE scripts on real Android

> **Superseded by [`phone-as-backend.md`](phone-as-backend.md).** The
> recommendation below — per-call RPC first, an interpreter later — was
> reversed: the script runs on the device. Two arguments made here are simply
> wrong and are corrected there: that two Rhai engines would drift (it is one
> engine cross-compiled), and that the calls are not latency-sensitive (device
> *callbacks* are). Section 2's boundary analysis stands and is why this file
> is kept.
>
> **Design record, 2026-08-25.** Point-in-time by design. Nothing here is
> implemented. It records a proposal, the evidence for it, and — more usefully
> — the boundary of what an Android app can and cannot do, so nobody starts
> the parts that cannot work.

**The problem it solves.** Every hardware test in this project needs a pair of
hands. Pairing a phone, pressing a dongle's button, tapping through an audio
picker: each is a person in the loop, and a test with a person in the loop
runs once, not thirty times. The Data benchmark makes this acute — an average
over twenty transfers against a real Bluetooth 5.3 radio is exactly the
measurement worth having, and exactly the one nobody will sit through by hand.

**The proposal.** An Android app that accepts scripts over an RPC channel and
runs them, so a phone becomes a remotely-driven SimBLE device. The MCP server
(`simble mcp`) is the model: an agent-facing surface that takes a device
description and stands the device up.

## 1. Why this fits: the vocabulary already matches

The scripting surface was deliberately shaped like Android's API (see
[`scripting-profile-apis.md`](scripting-profile-apis.md)). That decision was
argued on ergonomics — a developer who knows Android GATT should not have to
learn a second vocabulary. It has a second payoff nobody claimed at the time:
**the same script text can drive a real Android device.**

Measured against the tree rather than asserted. The classes the scripting
layer mirrors:

| SimBLE script binding | Android class |
|---|---|
| `android::BluetoothGattServer` | `BluetoothGattServer` |
| `android::BluetoothGattService` | `BluetoothGattService` |
| `android::BluetoothGattCharacteristic` | `BluetoothGattCharacteristic` |
| `android::BluetoothGattDescriptor` | `BluetoothGattDescriptor` |
| `android::BluetoothDevice` | `BluetoothDevice` |
| server/client callbacks | `BluetoothGattServerCallback`, `BluetoothGattCallback` |

And the constants are Android's own names with Android's own meanings — every
`PROPERTY_*` (`READ`, `WRITE`, `WRITE_NO_RESPONSE`, `NOTIFY`, `INDICATE`,
`BROADCAST`, `SIGNED_WRITE`, `EXTENDED_PROPS`) and every `PERMISSION_*`
including the encrypted and MITM variants.

So for a GATT server the mapping is close to mechanical: `add_service`,
`add_characteristic`, `add_descriptor`, `set_value` and
`notify_characteristic_changed` each have a direct counterpart.

**The larger prize is not remote control.** A script that runs in both places
makes the phone a *foreign implementation of our own device definitions*: run
`catalog/devices/hrm.rhai` in SimBLE and on Android, diff what appears on the
air, and the scripting layer finally has an oracle. Today nothing tests it
except SimBLE's own parser, which shares SimBLE's misunderstandings. It would
also turn the Android-shape claim in `scripting-profile-apis.md` from an
argument into a measurement.

## 2. The boundary, stated before anyone starts

**SimBLE is a host stack and needs a controller. Android does not give an app
one.** An app gets the framework's Bluetooth API; HCI is not reachable without
root. So the app cannot *run* SimBLE — it must **interpret the script and call
the Android API**. Everything below GATT is therefore out of reach, and no
amount of effort changes that:

| Reachable from an Android app | Not reachable |
|---|---|
| GATT server: services, characteristics, descriptors, values, notifications | Raw HCI of any kind |
| GATT client: connect, discover, read, write, subscribe | PHY selection (1M / 2M / Coded) |
| Advertising via `AdvertiseData` (service UUIDs, service data, manufacturer data) | Connection interval, latency, supervision timeout |
| Connection as a peer, MTU request | Data Length Extension control |
| | Arbitrary AD structures — `AdvertiseData` is a builder, not a byte array |
| | LE Audio / BIS — restricted or absent for normal apps |
| | The `tick()` model: no equivalent; the app must drive device physics on its own timer |

**What the phone therefore is:** a scriptable GATT peer with a real Bluetooth
5.3 radio — 2M PHY and Data Length Extension in practice, negotiated by the
framework rather than chosen by us. Excellent for device-level interop and for
the Data benchmark. Useless for controller-level work, which stays with
dongles.

## 3. Permissions

Runtime grants on Android 12+, prompted once:

- `BLUETOOTH_ADVERTISE` — to advertise
- `BLUETOOTH_CONNECT` — to run a GATT server or connect as a client
- `BLUETOOTH_SCAN` — to scan, declared `neverForLocation` so the app does not
  drag the location permission in behind it

These are a setup step, not an obstacle. Worth stating plainly because
`scripting-profile-apis.md` argues at length that *script-visible* permissions
are irrelevant to SimBLE — that argument is about GATT attribute permissions
and does not apply here. App permissions are real and must be granted.

## 4. Staging

**v1 — RPC, no interpreter.** A WebSocket the host drives, with a small verb
set: stand up this GATT server (a JSON description, not a script), advertise
this payload, connect to this address, write N bytes, report timings and byte
counts. No Rhai on the phone at all.

This is deliberately the unglamorous half, because it is the half that
unblocks work: it gives the Data benchmark a real 5.3 peer that is
**repeatable and hands-free**, and it is the two-sided measurement the
benchmark needs (the phone reports what it *received*, so a transfer time is
arrival rather than "sent, not confirmed delivered").

**v2 — Rhai on the phone.** The interpreter compiled for `aarch64-linux-android`
with JNI glue, and the `android::*` bindings implemented against the real
framework classes. Catalog scripts then run unmodified, and the diff-oracle in
§1 becomes possible.

The staging matters: v1 is worth building even if v2 never happens, and v2 is
much harder to justify before v1 has shown the RPC path carries its weight.

## 5. What would kill it

Honest failure modes, so they are recognised early rather than discovered late:

- **`AdvertiseData` is not a byte array.** Scripts that stage exact advertising
  bytes (beacons, Fast Pair, the Eddystone and CSIP idioms in `catalog/`)
  cannot be reproduced faithfully. The mapping is good for GATT and lossy for
  advertising.
- **No `tick()`.** A SimBLE device's values animate from its own clock. On
  Android that becomes app-side scheduling, and the two will drift in ways that
  make a byte-level diff between the two runs noisy rather than decisive.
- **Framework opinions.** Android caches services, imposes its own MTU
  behaviour and connection parameters, and will occasionally do something the
  script did not ask for. That is *why* it is a useful oracle — and also why a
  diff will need judgement rather than equality.
