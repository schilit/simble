---
name: phone-throughput-bench
description: Benchmark BLE bulk throughput against real phones running SimBLE Android, with a CSR8510 USB dongle as the central. Use when asked to run the phone speed test, measure real-radio throughput to a phone, compare phones, or drive examples/phone_bulk. Encodes the traps that make this flaky (macOS Local Network permission, wireless-adb dropping under BLE load, expiring pair codes, one-advertiser-at-a-time, off-link stats).
---

# Phone throughput benchmark (dongle → phone, real RF)

Measure how fast a **real phone** receives a BLE bulk transfer. simble is the
central over a **real USB dongle**; the phone runs **SimBLE Android** (`android/app/`)
as the GATT sink and counts what lands. This is the run that puts Android's real
host stack and a real phone controller on the receiving end — where the bugs that
only silicon shows have always been.

## Topology — non-negotiable

- **Dongle = central (does the writing). Phone = sink.** A phone can **never** be
  simble's central: simble drives a controller over HCI, and stock Android does
  not expose its controller as an HCI radio over adb. So there is **no
  phone-to-phone path**. "Across N phones" means benchmark N phones **as sinks**,
  one at a time, each driven by the dongle, then compare.
- **Stats come back over HTTP, never over the BT link.** A `FINISH`/`REPORT` on the
  link costs air time on the very thing being measured, and its arrival is what
  ends the transfer — every figure would then include a round trip of the thing
  under test, and a broken link could not deliver its result at all. The run sets
  `use_control_point: false`; the link carries payload and nothing else.

## The traps (read before touching anything)

1. **macOS Local Network permission.** On macOS 15+ (26/Tahoe here) LAN access is a
   per-app permission, inherited by CLI tools from the app that launched them
   (iTerm/Terminal). If it's off, the fingerprint is exact and misleading:
   **internet works, mDNS discovery works, ARP resolves, but every LAN unicast
   fails with "No route to host"** — the phones *and* the gateway. It silently
   resets on OS point updates ("worked yesterday"). Fix: System Settings → Privacy
   & Security → **Local Network** → enable the terminal app (toggle off/on if
   already listed). This is a system setting **you cannot change for the user** —
   direct them to it.

2. **Wireless adb dies under BLE load — prefer USB.** BLE and 2.4 GHz WiFi coexist
   badly; a concurrent BLE transfer will wedge a phone's wireless-adb link (shell
   commands hang forever, then `device offline`). Observed on two Pixels in one
   session. **Put the sink phone on USB** for the stable control + stats-forward
   channel; leave only the BLE transfer wireless. If the phones are wireless-only,
   expect instability — run each phone in **one clean pass** and don't be surprised
   when the second one wedges. Always wrap adb calls in a hard timeout (see below)
   so a hung call can't stall for 2 minutes.

3. **The dongle.** CSR8510 A10 → selector **`0a12:0001`** (VID 0x0A12 Cambridge
   Silicon Radio, PID 0x0001). Verify presence with **`ioreg -p IOUSB`**, not
   `system_profiler SPUSBDataType` (it caches and will show 0 devices right after a
   plug-in). The CSR8510 is the **throughput ceiling**: BT 4.0, 1M PHY, no Data
   Length Extension, and it shares 10 ACL buffers (no separate LE pool). ~4 KB/s to
   any phone is the dongle, not the phone.

4. **adb is not on PATH.** It lives at
   `~/Library/Android/sdk/platform-tools/adb` (or `$ANDROID_HOME/platform-tools`).

5. **Wireless pairing codes expire in seconds and ports rotate.** Get the pairing
   port from mDNS `_adb-tls-pairing._tcp` and run `adb pair` in the *same breath*
   as the human reads the 6-digit code. A `protocol fault (couldn't read status
   message)` almost always means the code already expired — ask for a fresh one
   and re-fetch the port. Two devices can share the hostname `Android.local`; map
   the connect port to the right IP by probing which IP actually listens.

6. **One advertiser at a time.** If two phones run SimBLE Android, both advertise
   `f0bb0001` and the dongle grabs whichever it sees — corrupting the run.
   `am force-stop com.simble` on every phone except the one under test.

7. **Read stats off-link over HTTP.** `SIMBLE_SINK_HTTP=host:8099`. The sink's
   `new ServerSocket(PORT)` binds `0.0.0.0`, so **direct WiFi (`<phone-ip>:8099`)
   works and is preferred** — it keeps adb out of the run entirely (adb is only
   for launch/force-stop, before the transfer). `adb forward tcp:8099 tcp:8099`
   then `127.0.0.1:8099` is the fallback (and the only path when the phone is on
   USB with no routable IP). If direct WiFi returns nothing, it's the **phone's
   WiFi that has wedged** (check: adb to it will be hanging/offline too), not the
   socket. Quote the phone's **`duration_ms`** — measured on the phone's own clock,
   and a *duration* needs no agreement about epochs.

8. **The run self-verifies.** `phone_bulk` resets the sink counter to `expected`
   before the run and reads it after; a clean **0 → N** proves the dongle hit
   *this* phone and not a stray advertiser. If the sink says 0 while the host says
   N sent, the dongle connected to the wrong phone.

## Procedure

Use the helper for the mechanical parts; do discovery/pairing by hand (needs a
human to read pair codes).

```bash
ADB=~/Library/Android/sdk/platform-tools/adb

# 0. Dongle present? (ioreg, not system_profiler)
ioreg -p IOUSB -l -w 0 | grep -i 'USB Product Name'   # expect "CSR8510 A10"

# 1. Discover phones advertising wireless debugging
#    (skip if on USB: `$ADB devices` shows them directly)
dns-sd -B _adb-tls-connect._tcp local.                # instance names → serials
dns-sd -Z _adb-tls-connect._tcp local.                # SRV → host:port

# 2. Pair each (wireless only; USB just needs "Allow USB debugging" on the phone).
#    Human opens: Settings → Developer options → Wireless debugging →
#    tap "Wireless debugging" (the words) → Pair device with pairing code.
#    Get the pairing port live, pair in the same breath as the code:
dns-sd -Z _adb-tls-pairing._tcp local.                # SRV → pairing port for that serial
$ADB pair <phone-ip>:<pairing-port> <6-digit-code>
$ADB connect <phone-ip>:<connect-port>

# 3. Install + permission the sink (APK is prebuilt)
$ADB -s <serial> install -r android/app/build/simble-android.apk
$ADB -s <serial> shell pm grant com.simble android.permission.BLUETOOTH_ADVERTISE
$ADB -s <serial> shell pm grant com.simble android.permission.BLUETOOTH_CONNECT

# 4. Benchmark one phone (see scripts/bench-one.sh — it force-stops the others,
#    launches the sink, verifies /stats, runs phone_bulk, reads the phone-clock number)
.claude/skills/phone-throughput-bench/scripts/bench-one.sh <serial> [bytes]
```

Repeat step 4 per phone, then tabulate.

## Interpreting the numbers

`phone_bulk` prints two figures:
- **Host JSON** `throughput_kb_s` with `"confirmation":"unconfirmed"` — the dongle's
  view (`transfer_ms`). Useful, but the dongle's clock.
- **`sink says` `duration_ms`** — the phone's own receive time. This is the
  quotable number (`bytes / duration_ms`). Confirmation is `http-reported`: both
  ends' figures, stats fetched off the link.

They should agree closely (a session saw host 4.15 KB/s vs phone 4.17 KB/s over
15.3 s). If the sink shows fewer bytes than sent, that's real loss — investigate
ACL fragment sizing (take the LE buffer *count* but never a Classic length).

## Known-good baseline

Pixel 8 Pro (Core 5.4), 65536 bytes over CSR8510: **15.346 s, ≈4.17 KB/s, 0 loss,
MTU 517, 236-byte chunks**. Any phone lands near this because the dongle is the cap.

## What this is NOT

- Not Channel Sounding (needs BT 6.0 silicon — Pixel 9/10, not the 6/6 Pro/8 Pro).
- Not an MCP feature: the simble MCP is the portable *simulation* surface; real-lab
  orchestration (adb, a physical CSR8510, this LAN, flaky wireless) is
  environment-specific and non-deterministic and does not belong behind an MCP tool.
