# SimBLE Android

The peripheral half of the bulk-transfer benchmark, on a real phone. A dongle
central writes; this counts what lands and reports the count back over the
control point.

That report is the point. The benchmark writes without response, so "the
central finished writing" happens well before "the phone received the last
byte" — a client-only stopwatch is wrong in the flattering direction and
blind to loss. The number this app sends back is the one worth quoting.

## Build and install

Needs a JDK and the Android SDK build tools. **No Gradle, no Kotlin, no
network** — the app is one Java file, and Gradle would add a wrapper
download, a daemon and a dependency graph to a build that is four commands.

```sh
./build.sh            # -> build/simble-android.apk
./build.sh install    # also installs, grants permissions, launches
```

`install` grants `BLUETOOTH_ADVERTISE` and `BLUETOOTH_CONNECT` with
`adb shell pm grant`, because without them the app launches and immediately
says it cannot advertise.

## Measure against it

```sh
cargo run --example phone_bulk -- 02.3.1 65536
```

The example scans for the service rather than taking an address, because
Android advertises from a rotating resolvable private address and does not
tell its own app what that address is.

## Why an Activity

`android/README.md` describes a headless service. This is deliberately not
that: the full design runs Rhai on the device and needs JNI, the NDK and a
Gradle build, none of which is required to put a real Android host stack and
a real controller on the receiving end of a transfer. A visible counter is
also the fastest way to see a run stall.

## Measured, 2026-08-25 — Pixel 9 Pro, Android 17

64 KB, dongle `02.3.1` (CSR8510) central, write-without-response:

| | |
|---|---|
| sent / received | 65536 / 65536 — no loss |
| chunks | 3277 / 3277 |
| **MTU** | **23 — not raised** |
| discover / connect / negotiate | 197 ms / 389 ms / 2852 ms |
| transfer | 13.6 s, 4.7 kB/s |
| confirmation | `peer-reported` |

**The MTU is the finding.** The same central negotiates 512 against a dongle
sink and got 23 here, so the transfer ran in 20-byte chunks — 3277 writes for
64 KB. Raising it would cut the chunk count by more than twenty times, and it
is the first thing to chase. Setup is the other half of the story: 3.4 s
before a byte moves, most of it walking the phone's twelve services at a
23-byte MTU.
