# The phone as a SimBLE backend

> Supersedes an earlier remote-control ("RPC peer") design — the script runs on
> the device, not behind a per-call RPC. Nothing
> below is implemented; the transport half is measured and says so where it is.

**Decision:** *the script runs on the device, not a remote-control client.* The
same script that defines a simulated device defines a real one — so a phone
becomes a first-class SimBLE backend that measures without a network in the
loop, and can be diffed against the simulator to test the scripting layer
itself.

## 1. Why the script moves to the device, not an RPC client

The obvious design is an app exposing Android's Bluetooth API over RPC, with the
host holding the script and calling primitives — what Google's
[Mobly](https://github.com/google/mobly) does. Ours uses the same transport
(`adb forward` + JSON-RPC) but not the same division of labour, because SimBLE's
premise is that **a script is a device and a test**, and a Mobly "snippet" is
only ever a primitive somebody else calls — it cannot host a script. Moving the
script across buys three things a per-call client cannot:

- **Timing.** No network in the measurement loop; a per-call client would time
  WiFi and scheduler jitter alongside the radio.
- **Fidelity.** Callbacks and `tick()` run at device speed. A device whose every
  GATT callback round-trips over WiFi does not behave like a device.
- **The oracle.** Running *the same script text* in the simulator and on real
  Android makes the two comparable. A host interpreting and sending primitives
  would test the host's interpretation, not the device.

The third cannot be bought another way.

## 2. What a phone measurement is, and is not

**On the phone, SimBLE's host stack is not in the path.** The script drives
Android's `BluetoothGattServer` and Android's radio. So a phone row measures
**Android's Bluetooth stack on real RF**; a simulator row measures **our stack
over a simulated medium**. These are two different stacks, not one stack on two
media, and are **not comparable as a like-for-like benchmark**: a phone number
answers "how long does this take in the real world", a simulator number "how
does our code behave". Both are worth having, and every phone row carries that
caveat, as the real-RF and simulated rows already do in the Data category.

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

On the phone the bottom two layers are **Android's**, which makes it a foreign
implementation and therefore an oracle. The Rhai Runner is the only layer that
must exist in both worlds; the `android::` API is the seam, one trait rather
than a boundary scattered through the bindings.

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

So the master reaches a device-side server at `127.0.0.1:8099` over WiFi with no
cable, no IP configuration, and no discovery protocol of our own.

**Why adb rather than anything we build.** Android reshuffles the wireless
debugging port every time the setting toggles, so a typed address goes stale;
adb's own mDNS daemon solves that and auto-connects paired devices by service
name. A phone is enumerated by `adb devices` — no registration protocol, no
service advertisement — and feeds the same controller picker a dongle does. adb
is already a dependency (the Android SDK, for netsim). The picker must also
accept a typed `ip:port` for `adb connect`, since adb's mDNS can collide with
other Bonjour responders.

## 5. The app

Headless — no Activity. Three viable shapes:

1. **A foreground service in a minimal APK.** Standard; needs a declared service
   type and shows a notification. Permissions are not the obstacle they appear:
   `adb shell pm grant … BLUETOOTH_CONNECT` grants them with no UI.
2. **`app_process` with a pushed JAR** — scrcpy's trick. No APK, no install.
   Measured on the Pixel: the `shell` user already holds `BLUETOOTH_CONNECT`,
   `BLUETOOTH_ADVERTISE` and `BLUETOOTH_SCAN`, so this is viable for permissions.
3. An instrumentation APK (`am instrument`).

**Rust side.** `cargo-ndk` builds the `.so`; the `jni` crate carries the calls;
a thin Kotlin shim makes the Bluetooth API idiomatic and forwards callbacks
back. Zero-Kotlin (hand-written JNI for every framework call) is possible but
not worth it — `BluetoothGattServer` alone would be dozens of JNI calls, and
callbacks need a JVM-side class regardless. Those crates live in a separate
`simble-android` crate, so the core library keeps its near-zero-dependency rule.

## 6. The protocol already exists

"Send a script, get back a verdict and a log" is `run_test`. "Stand this device
up on that controller" is `run_on`. The MCP surface — `lint`, `run_test`,
`run_on`, `add_peripheral`, `add_central`, `lookup`, `example`, `tick`,
`status` — is already coarse-grained, script-shaped, and controller-agnostic;
`run_on` already takes a backend name. A phone is a third value, and the handler
forwards instead of executing. MCP already speaks JSON-RPC over stdio and
WebSocket (`simble mcp --ws-server`), so the device-side server is that handler
behind a third transport rather than a new interface.

## 7. The first milestone: `toast`

Before any radio, the first verb is `toast` — show a message on the phone's
screen. It has no Bluetooth in it, so it exercises the whole chain end to end —
`adb forward` → device-side server → JSON-RPC dispatch → JNI → Kotlin shim → an
Android UI call — and nothing else: a toast that appears means everything
structural is finished, and a failure is unambiguously in the transport or the
app. It also answers the question a shelf of identical phones raises: *which one
is running my script?*

Notes: `Toast` needs a `Context` and the main looper, so it belongs in the
Kotlin shim, not raw JNI; Android 11+ restricts *custom* toast views from the
background, while plain text toasts still show — which is all this needs.

**A version works today with no app at all**, worth keeping as a setup tool and
fallback. It runs as the `shell` user, so it works even when the app is wedged:

```sh
adb -s <serial> shell cmd notification post -S bigtext -t 'SimBLE' tag '<message>'
```

Verified against the Pixel 9 Pro. Use it to label phones as they join the fleet,
and as the smoke test that a newly paired device is reachable.

## 8. What it opens: a ladder of fidelity

One artifact — the script — climbs every rung, and each rung costs more and
proves more:

| Rung | Radio | Stack under test | Determinism |
|---|---|---|---|
| in-process | none | ours | total — same result every run |
| netsim | simulated | ours | high; multi-device, Android emulator peers |
| dongle | **real RF** | ours | none; timing and interference are real |
| **phone** | **real RF** | **Android's** | none; a foreign stack answers |

An agent can write a device through MCP, `lint` it, `run_test` it
deterministically in milliseconds, and then `run_on` the *same text* against
netsim, a dongle, and a phone — watching what survives contact with each. The
failures are the point: a script that passes in the simulator and fails on a
dongle has found a gap between our stack and the radio; one that passes on a
dongle but fails on a phone has found a gap between our reading of the spec and
Android's. The two costliest bugs to date — a peripheral advertising from the
wrong address, and an ISO stream with no flow control — were exactly this shape:
correct in simulation, wrong on silicon, each one rung of this ladder.

## 9. What is not settled

- **Whether the whole scripting surface can be implemented over the Android
  API.** GATT maps closely (the classes and every `PROPERTY_*` / `PERMISSION_*`
  constant already carry Android's names). Advertising is lossy —
  `AdvertiseData` is a builder, not a byte array, so scripts staging exact AD
  bytes cannot be reproduced faithfully. Anything below GATT is out of reach
  entirely. A count of what `catalog/devices/` actually needs would settle how
  much of the catalog is portable, and has not been done.
- **One radio, one device.** A phone is one controller, so a phone backend has
  the dongle's constraint: peripheral or central, not both. Two-ended tests need
  two backends — which a fleet of spare phones supplies more usefully than
  dongles do: two phones are two *independent Android stacks*, where two dongles
  are two radios under one host stack of ours.
- **What the fleet spans.** Old phones are more interesting than new ones: a
  device working across Bluetooth 4.0, 4.2, 5.0 and 5.3 is a compatibility claim
  rather than a data point. The floor is API 26 or so for a headless foreground
  service; the runtime permission model changed at 12, and LE Audio needs 13+.
  The versions in hand are not yet enumerated, so the matrix is unwritten.
