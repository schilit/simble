# The phone as a SimBLE backend

> **Design record, 2026-08-25.** Point-in-time by design. Supersedes
> `android-rpc-peer.md`, whose staging and recommendation this replaces —
> that doc's boundary analysis still holds and is not repeated here. Nothing
> below is implemented; the transport half is measured, and says so where it
> is.

**The thesis.** *The script runs on the device, not a remote-control client.*
The same script that defines a simulated device defines a real one — so a
phone becomes a first-class SimBLE backend that measures without a network in
the loop, and can be diffed against the simulator to test the scripting layer
itself.

## 1. Why not a remote-control client

The obvious design is an app exposing Android's Bluetooth API over RPC, with
the host holding the script and calling primitives. That is what Google's
[Mobly](https://github.com/google/mobly) does, and its architecture is worth
knowing because ours converged on the same transport independently:

| | Mobly | Here |
|---|---|---|
| Transport | `adb forward` + JSON-RPC | the same |
| Device side | "snippets" — Java methods called from the host | the script itself |
| Host side | `AndroidDevice` controller | a `run_on` backend |

The transport is identical and the division of labour is not, because SimBLE's
premise is that **a script is a device and a test**. A snippet can only ever be
a primitive somebody else calls; it cannot host a script. Three things follow
from moving the script across:

- **Timing.** No network in the measurement loop. A per-call client would time
  WiFi and scheduler jitter alongside the radio.
- **Fidelity.** Callbacks and `tick()` run at device speed. A device whose
  every GATT callback round-trips over WiFi does not behave like a device.
- **The oracle.** Running *the same script text* in the simulator and on real
  Android makes the two comparable. Had the host interpreted and sent
  primitives, the comparison would test the host's interpretation rather than
  the device's — worthless for the purpose.

The third is the one that cannot be bought another way, and it is why this is
not simply Mobly with different languages.

## 2. What a phone measurement is, and is not

**On the phone, SimBLE's host stack is not in the path.** The script drives
Android's `BluetoothGattServer` and Android's radio. So a phone row measures
**Android's Bluetooth stack on real RF**; a simulator row measures **our stack
over a simulated medium**.

Those are two different stacks, not one stack on two media, and they are **not
comparable as a like-for-like benchmark**. A phone number answers "how long
does this take in the real world"; a simulator number answers "how does our
code behave". Both are worth having and a chart that implies otherwise is
lying. Every phone row carries that caveat, exactly as the real-RF and
simulated rows already do in the Data category.

## 3. The layers

```
Rhai Runner       script -> device; portable, knows nothing about radios
     | trait
android:: API     two impls: virtual (ours, everywhere) | real (JNI, phone)
     |
Host stack        GATT/ATT/SMP - ours only; absent on the phone
     |
Controller        sim | netsim | dongle | the phone's own radio
```

On the phone the bottom two layers are **Android's**, which is precisely what
makes it a foreign implementation and therefore an oracle. The Rhai Runner is
the only layer that must exist in both worlds; the `android::` API is the seam,
and it is one trait rather than a boundary scattered through the bindings.

## 4. Transport: measured, not assumed

`adb` over WiFi, no cable. Every step below was run against a Pixel 9 Pro
(`caiman`, Android 17) from this machine:

```
$ adb mdns services
adb-45221FDAP005P3-xygy1G  _adb-tls-pairing._tcp  192.168.86.41:42021
adb-45221FDAP005P3-xygy1G  _adb-tls-connect._tcp  192.168.86.41:38927

$ adb pair 192.168.86.41:42021 <code>
Successfully paired to 192.168.86.41:42021 [guid=adb-45221FDAP005P3-xygy1G]

$ adb connect 192.168.86.41:38927
connected to 192.168.86.41:38927

$ adb devices -l
192.168.86.41:38927  device product:caiman model:Pixel_9_Pro
adb-45221FDAP005P3-xygy1G._adb-tls-connect._tcp  device  model:Pixel_9_Pro

$ adb shell getprop ro.build.version.release
17

$ adb forward tcp:8099 tcp:8099
8099
```

So the master reaches a device-side server at `127.0.0.1:8099` over WiFi with
no cable, no IP configuration, and no discovery protocol of our own.

**Why adb rather than anything we build.** Android reshuffles the wireless
debugging port every time the setting toggles, so an address typed once goes
stale. adb's own mDNS daemon solves that and auto-connects paired devices by
service name — the second `adb devices` row above is exactly that. We
considered publishing an mDNS service and writing a browse-only client; both
were deleted from this design because adb already does it, and the Android SDK
is a dependency this project already has for netsim.

**Discovery is `adb devices`.** No registration protocol, no service
advertisement, no stale-entry expiry. A phone is enumerated the way a dongle
is enumerated, and feeds the same controller picker.

**The fallback matters.** adb's mDNS can collide with other Bonjour responders.
Reading `ip:port` off the Wireless debugging screen and running `adb connect`
always works, so the picker must accept a typed address rather than trusting
discovery as the only path.

## 5. The app

Headless — no Activity. Three viable shapes, in the order they were considered:

1. **A foreground service in a minimal APK.** Standard; needs a declared
   service type and shows a notification. Runtime permissions are not the
   obstacle they first appear: `adb shell pm grant … BLUETOOTH_CONNECT` grants
   them with no UI at all.
2. **`app_process` with a pushed JAR** — scrcpy's trick. No APK, no install.
   Measured on the Pixel: the `shell` user already holds `BLUETOOTH_CONNECT`,
   `BLUETOOTH_ADVERTISE` and `BLUETOOTH_SCAN`, so this is viable for
   permissions.
3. An instrumentation APK (`am instrument`).

**Rust side.** `cargo-ndk` builds the `.so`; the `jni` crate carries the calls;
a thin Kotlin shim makes the Bluetooth API idiomatic and forwards callbacks
back. Zero-Kotlin is possible (`android-activity` + `ndk-context` + hand-written
JNI for every framework call) and is not worth it: `BluetoothGattServer` alone
would be dozens of JNI incantations, and callbacks need a JVM-side class
regardless.

Those crates live in a separate `simble-android` crate, so the core library
keeps its near-zero-dependency rule.

## 6. The protocol already exists

"Send a script, get back a verdict and a log" is `run_test`. "Stand this device
up on that controller" is `run_on`. The MCP surface — `lint`, `run_test`,
`run_on`, `add_peripheral`, `add_central`, `lookup`, `example`, `tick`,
`status` — is already coarse-grained, script-shaped, and controller-agnostic;
`run_on` already takes a backend name. A phone is a third value, and the
handler forwards instead of executing.

MCP already speaks JSON-RPC over stdio and WebSocket (`simble mcp
--ws-server`), so the device-side server is that handler behind a third
transport rather than a new interface.

## 7. What it opens: a ladder of fidelity

The phone completes something the other backends only gesture at. One
artifact — the script — climbs every rung, and each rung costs more and
proves more:

| Rung | Radio | Stack under test | Determinism |
|---|---|---|---|
| in-process | none | ours | total — same result every run |
| netsim | simulated | ours | high; multi-device, Android emulator peers |
| dongle | **real RF** | ours | none; timing and interference are real |
| **phone** | **real RF** | **Android's** | none; a foreign stack answers |

An agent can write a device through MCP, `lint` it, `run_test` it
deterministically in milliseconds, and then `run_on` the *same text* against
netsim, a dongle, and a phone — watching what survives contact with each.

That makes protocol exploration cheap in a way it has not been. A question
like "what does a real phone do with an advertisement shaped like *this*"
currently costs an afternoon of hand-driving; as a script it costs one
`run_on`. And the failures are the point: a script that passes in the
simulator and fails on a dongle has found a gap between our stack and the
radio, and one that passes on a dongle but fails on a phone has found a gap
between our reading of the spec and Android's. Both are findings, and both are
invisible today.

The two bugs that cost the most time this week — a peripheral advertising from
the wrong address, and an ISO stream with no flow control — were exactly this
shape: correct in simulation, wrong on silicon. They took days to find by
hand. They would each have been one rung of this ladder.

## 8. What is not settled

- **Whether the whole scripting surface can be implemented over the Android
  API.** GATT maps closely (the classes and every `PROPERTY_*` / `PERMISSION_*`
  constant already carry Android's names). Advertising is lossy —
  `AdvertiseData` is a builder, not a byte array, so scripts staging exact AD
  bytes cannot be reproduced faithfully. Anything below GATT is out of reach
  entirely. A count of what `catalog/devices/` actually needs would settle how
  much of the catalog is portable, and has not been done.
- **One radio, one device.** A phone is one controller, so a phone backend has
  the dongle's constraint: peripheral or central, not both. Two-ended tests
  need two backends.
