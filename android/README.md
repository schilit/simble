# android/

Two things live here, at different stages:

- **`app/` — a working standalone app** (`app/`), built today. Three Java files,
  no Gradle, no Kotlin, no Rust linkage: it puts a real Android host stack and a
  real phone controller on the receiving *or* sending end of a bulk BLE transfer
  — the phone-throughput and phone-to-phone benchmarks. It talks to the outside
  world only over BLE and HTTP, never by linking the crate. See
  [`docs/phone-to-phone.md`](../docs/phone-to-phone.md).
- **`rust/` — scaffolding for a future headless backend** that would run SimBLE
  Rhai scripts on the device over JNI, so the same script text that defines a
  simulated device defines a real one. **Not built yet.** Its design is
  [`docs/phone-as-backend.md`](../docs/phone-as-backend.md); read that first,
  particularly §2, on what a phone measurement is and is not. The rest of this
  README is about that planned backend; `app/` is described in its own header.

## Why it is not in the workspace

`rust/` is a standalone crate with its own lockfile, depending on `simble` by
path. It is deliberately **not** a Cargo workspace member:

- the root has no `[workspace]` and `simble` is one crate; adding a member
  would put `jni` and the NDK machinery in the main lockfile, against the
  near-zero-dependency rule in [`AGENTS.md`](../AGENTS.md)
- `cargo test --all-targets --all-features` would build it on every gate run,
  for a target it is not meant to run on

The cost of that choice is drift: a path dependency nobody builds by default
will rot silently. CI therefore `cargo check`s it for `aarch64-linux-android`
on every push, which catches a breaking change to `simble` the day it lands
rather than the day someone next opens this directory.

## Layout

```
app/      the shipping benchmark app: SimbleActivity + BulkSource + StatsServer
          (Java), AndroidManifest.xml, build.sh — no Gradle, no Kotlin, no NDK
rust/     scaffolding for the planned headless backend: the Rhai runner and the
          real `android::` implementation over JNI (not built yet)
```

`app/` builds with `app/build.sh` (plain `aapt2`/`javac`/`d8`/`apksigner`, no
Gradle) — see that script. The planned backend below is a separate design.

For the planned backend, the seam is one trait: Rust holds the runner and the
logic; a thin Kotlin (or Java) shim makes Android's Bluetooth API idiomatic and
forwards its callbacks back through JNI. Zero-shim is possible and is not worth
it — see §5 of the design doc.

## Building (once there is something to build)

```sh
rustup target add aarch64-linux-android
cargo install cargo-ndk
cd android/rust && cargo ndk -t arm64-v8a build --release
```

## Talking to a phone

Over WiFi, no cable. `adb`'s own mDNS finds the device and survives the port
Android reshuffles every time wireless debugging toggles:

```sh
adb mdns services                      # phone visible?
adb pair <ip>:<pair-port> <code>       # once, ever
adb connect <ip>:<connect-port>        # auto-connects thereafter
adb forward tcp:8099 tcp:8099          # host reaches the phone at 127.0.0.1:8099
```

If mDNS is unhelpful — it can collide with other Bonjour responders — read
`ip:port` off the phone's Wireless debugging screen and `adb connect` it
directly. That always works.
