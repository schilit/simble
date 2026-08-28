---
name: phone-throughput-bench
description: Benchmark BLE bulk throughput against real phones running SimBLE Android — either dongle-to-phone (a CSR8510/nRF USB dongle as central) or phone-to-phone (one phone's own radio as central, no dongle). Use when asked to run the phone speed test, measure real-radio throughput to a phone, compare phones, run a phone-to-phone transfer, or drive examples/phone_bulk. Encodes the traps that make this flaky (macOS Local Network permission, wireless-adb dropping under BLE load, expiring pair codes, one-advertiser-at-a-time, off-link stats, the 512-byte attribute cap).
---

# Phone throughput benchmark (dongle → phone, real RF)

Measure how fast a **real phone** receives a BLE bulk transfer. simble is the
central over a **real USB dongle**; the phone runs **SimBLE Android** (`android/app/`)
as the GATT sink and counts what lands. This is the run that puts Android's real
host stack and a real phone controller on the receiving end — where the bugs that
only silicon shows have always been.

## Two topologies

- **Dongle → phone (real RF, simble as central).** simble drives a USB controller
  over HCI; the phone runs SimBLE Android as the **sink**. This is the bulk of
  this skill. simble can *only* be the central this way — stock Android does not
  expose its controller as an HCI radio over adb, so **simble-over-HCI has no
  phone-to-phone path**. "Across N phones" means benchmark N phones as sinks, one
  at a time, each driven by the dongle, then compare. Stats come back **over HTTP,
  never over the BT link**: a `REPORT` on the link costs air time on the very
  thing being measured and its arrival ends the transfer, so every figure would
  include a round trip of the thing under test. The run sets
  `use_control_point: false`; the link carries payload and nothing else.
- **Phone → phone (no dongle, the app as central).** The SimBLE app itself has a
  **source role** (`android/app/.../BulkSource.java`) that drives the transfer
  from one phone into another using Android's own `BluetoothGatt` *client* — not
  simble's HCI stack, which is why this is possible where simble-as-central is
  not. No dongle, no laptop in the data path. Here the count *does* come back on
  the link as a `REPORT` (and the sink times its own receive span), because there
  is no laptop to read HTTP and the phone's wifi doze makes `/stats` flaky anyway.
  See **Phone-to-phone** below. Either topology is one advertiser (the sink) and
  one central.

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

2. **A phone that shows "not running" or no HTTP — just relaunch the app. Don't
   diagnose.** This is the first move, every time. A killed app looks *identical*
   to a wedged link (no HTTP on :8099, absent from `/phones`), but it is far more
   common and it is a one-liner to fix:

   ```bash
   adb -s <serial> shell monkey -p com.simble -c android.intent.category.LAUNCHER 1
   ```

   Only bother diagnosing if `adb -s <serial> shell getprop ro.serialno` *itself*
   hangs — that, and only that, means the wireless-adb link is actually wedged.
   If getprop answers, the phone is fine and the app was simply killed; relaunch
   and move on. Do not sweep mDNS, reconnect transports, or theorize about
   coexistence before trying the relaunch.

3. **Wireless adb can die under BLE load — prefer USB.** BLE and 2.4 GHz WiFi
   coexist badly; a concurrent BLE transfer can wedge a phone's wireless-adb link
   for real (shell commands hang, then `device offline`). **Put the sink phone on
   USB** for the stable control channel; leave only the BLE transfer wireless. If
   the phones are wireless-only, expect the occasional genuine wedge — but reach
   for trap 2 (relaunch) *first*, since a killed app is the usual cause and a
   wedged link is the rare one. Always wrap adb calls in a hard timeout (see
   below) so a hung call can't stall for 2 minutes.

4. **The dongle.** CSR8510 A10 → selector **`0a12:0001`** (VID 0x0A12 Cambridge
   Silicon Radio, PID 0x0001). Verify presence with **`ioreg -p IOUSB`**, not
   `system_profiler SPUSBDataType` (it caches and will show 0 devices right after a
   plug-in). The CSR8510 is the **throughput ceiling**: BT 4.0, 1M PHY, no Data
   Length Extension, and it shares 10 ACL buffers (no separate LE pool). ~4 KB/s to
   any phone is the dongle, not the phone.

5. **adb is not on PATH.** It lives at
   `~/Library/Android/sdk/platform-tools/adb` (or `$ANDROID_HOME/platform-tools`).

6. **Wireless pairing codes expire in seconds and ports rotate.** Get the pairing
   port from mDNS `_adb-tls-pairing._tcp` and run `adb pair` in the *same breath*
   as the human reads the 6-digit code. A `protocol fault (couldn't read status
   message)` almost always means the code already expired — ask for a fresh one
   and re-fetch the port. Two devices can share the hostname `Android.local`; map
   the connect port to the right IP by probing which IP actually listens.

7. **One advertiser at a time.** If two phones run SimBLE Android, both advertise
   `f0bb0001` and the dongle grabs whichever it sees — corrupting the run.
   `am force-stop com.simble` on every phone except the one under test.

8. **Read stats off-link over HTTP.** `SIMBLE_SINK_HTTP=host:8099`. The sink's
   `new ServerSocket(PORT)` binds `0.0.0.0`, so **direct WiFi (`<phone-ip>:8099`)
   works and is preferred** — it keeps adb out of the run entirely (adb is only
   for launch/force-stop, before the transfer). `adb forward tcp:8099 tcp:8099`
   then `127.0.0.1:8099` is the fallback (and the only path when the phone is on
   USB with no routable IP). If direct WiFi returns nothing, it's the **phone's
   WiFi that has wedged** (check: adb to it will be hanging/offline too), not the
   socket. Quote the phone's **`duration_ms`** — measured on the phone's own clock,
   and a *duration* needs no agreement about epochs.

9. **The run self-verifies.** `phone_bulk` resets the sink counter to `expected`
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

## Phone-to-phone (no dongle)

One phone drives the transfer into another over its own radio. The sink is the
same app as always; the source is the app launched with a **role**:

```bash
# the whole pair, scripted: force-stops other advertisers, launches the sink,
# drives the source, reads both phones' clocks off the REPORT (no HTTP)
.claude/skills/phone-throughput-bench/scripts/bench-pair.sh \
  <source-serial> <sink-serial> [bytes]

# or by hand — the source is one intent (mind the quoting: the remote shell
# re-splits on spaces, so single-quote a spaced sink name):
adb -s <source> shell "am start -n com.simble/.SimbleActivity \
  --es role source --es target 'Pixel 8 Pro' --ei bytes 65536"
```

Traps specific to this path:

- **Cap the chunk at 512 bytes, not MTU-3.** MTU-3 works out to 514, but the max
  BLE *attribute value* is 512; a 514-byte write is malformed and the peer drops
  it silently — no error, no callback, just a transfer that stalls after one
  chunk and looks like a dead stack. `BulkSource` caps at 512; if you see exactly
  one chunk land and then nothing, this is why.
- **Write Without Response is fine — and 3.5× faster than confirmed.** The write
  callback *does* fire for no-response writes (once the size is valid), so the
  pump chains one deep. Confirmed writes also work but cost a round trip per
  chunk (~14 vs ~48 KB/s). Don't "fix" a stalled no-response pump by switching to
  confirmed writes; check the 512 cap first — that was the actual bug.
- **Discovery is slow: 30 s+ before the transfer starts.** The source scan can
  take half a minute to surface one named advertiser, then the transfer itself is
  ~1 s. `bench-pair.sh` waits up to ~50 s; if driving by hand, don't call it dead
  early. `BulkSource`'s own run timeout is 40 s.
- **One advertiser still applies.** The source filters on the service UUID and
  then the name; force-stop the app on every phone except the sink so the source
  can't grab a stray. Two same-model phones share a name — fine, because only the
  sink advertises the service.
- **Wireless adb wedges under BLE load here too (trap 3).** The transfer can
  succeed while the source phone's adb link drops, so the *result read* comes back
  empty even though the run worked — check the phone's screen or its logcat once
  adb recovers before concluding it failed. Prefer USB for the source.

Grant the source `BLUETOOTH_SCAN` (the sink never scans, so it doesn't need it);
`build.sh install` and `bench-pair.sh` both grant it.

### Known-good baseline (phone-to-phone)

- Pixel 6 Pro → Pixel 8 Pro, 64 KB: **~48 KB/s** (47.4 / 47.4 / 49.1 — tight).
- Pixel 8 Pro → Pixel 8 Pro, 64 KB: **~41–56 KB/s** (more spread, still 0 loss).

Every connect succeeds and every byte lands — no `0x3E`, unlike the nRF dongle.
Matches the patched nRF for speed and beats it for reliability, with no dongle.

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
