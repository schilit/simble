# android/

The phone as a SimBLE backend: a headless app that runs SimBLE scripts on a
real Android device, so the same script text that defines a simulated device
defines a real one.

**Nothing here is built yet.** This directory is scaffolding — the crate
skeleton, and the CI job that keeps it from rotting. The design it implements
is [`docs/phone-as-backend.md`](../docs/phone-as-backend.md); read that first,
particularly §2, which says what a phone measurement is and is not.

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
rust/     the Rhai runner and the real `android::` implementation over JNI
app/      the Gradle project: manifest, the Kotlin shim, no Activity
```

The seam between them is one trait. Rust holds the runner and the logic;
Kotlin holds the few dozen lines that make Android's Bluetooth API idiomatic
and forward its callbacks back through JNI. Zero-Kotlin is possible and is not
worth it — see §5 of the design doc.

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
