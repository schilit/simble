# Phone-to-phone bulk transfer

> **Decision record, 2026-08-27.** Point-in-time by design. Records *why* the
> phone-to-phone path is shaped the way it is and what was rejected, plus the
> throughput it produced on the lab hardware on this date (representative, not
> fixed). Implemented on the `phone-to-phone` branch.

---

## 1. The central is the phone's own GATT client, not simble-over-HCI

**Context.** Every earlier simble throughput run put a USB dongle on the sending
end: simble drives a controller over HCI, and stock Android does not expose its
controller as an HCI radio over adb. So a phone cannot be simble's central over
HCI — which appears to rule out a phone-to-phone transfer entirely, the common
real-world shape no published Bluetooth number covers.

**Decision.** Give the Android app a *source role*
([`BulkSource`](../android/app/src/com/simble/BulkSource.java)) that drives the
transfer with Android's own `BluetoothGatt` **client**, not simble's HCI stack.
The claim "no phone-to-phone path" is true only for simble-over-HCI; the phone's
own client is a perfectly good central. Discovery, MTU/PHY negotiation, the
control point (`BEGIN`/`FINISH`/`REPORT`), and the byte-count report all stay on
GATT and ride the link, so no laptop is in the data path — it only launches the
two apps over adb.

**Rejected: an RPC/remote-control client** exposing Android's BLE API to the
host (the shape [`phone-as-backend.md`](phone-as-backend.md) analyses). It keeps
the host in the loop for every operation, which is exactly the network the
phone-to-phone measurement exists to remove.

**Consequences.**
- The bridge orchestrates a run over adb (`am start … --es role source`), so the
  browser — which cannot fire an intent — drives it through a `/pair-run`
  endpoint.
- Both ends must run SimBLE Android; a dongle central stays on the GATT path.
- The run reports tracing-style phase spans (discover / connect / negotiate /
  transfer), the same breakdown the dongle path gives.

## 2. Payload can leave GATT for an L2CAP socket; control stays on GATT

**Context.** GATT writes are metered by ATT at roughly one no-response write per
connection event. Even after filling the stack's write queue (several 512-byte
chunks per event, capped at the 512-byte max attribute value), a 256 KB run
tops out near 68 KB/s on a confirmed 2M / MTU-517 link — the per-event ceiling,
paid twice with a mobile on each end.

**Decision.** Add a second payload path — an **L2CAP Connection-Oriented
Channel** — selectable per run (`link=gatt|l2cap`). The sink opens an insecure
L2CAP server (Android 10+) and publishes its PSM in a read characteristic; the
source reads the PSM and streams the payload over the socket, where L2CAP's own
credit-based flow control packs the connection event. GATT is kept for
discovery, control, and reporting; only the payload moves. A source that finds
no PSM falls back to GATT.

**Rejected: replacing GATT wholesale with L2CAP.** The control point, service
discovery, and the REPORT handshake work and are cheap; only the bulk payload is
GATT-bound. Moving just the payload is the smaller, reversible change and keeps
one code path for setup across both modes.

**Consequences.**
- A socket write returns once *buffered*, not once transmitted, so closing the
  socket at the end of the write loop discarded the tail (five bytes of 256 KB
  landed in the first attempt). The source holds the socket open until the
  sink's `REPORT`, and `report()` closes it.
- `FINISH` over GATT can outrun the still-draining stream, so the sink waits on
  receive *progress* (not a fixed deadline) before it reports.
- Throughput for an L2CAP run is the sink's first-to-last received-byte clock,
  since the source's transfer span is buffer-inflated.

## 3. A wedged sink is reset, not force-stopped, between runs

**Context.** `/pair-run` force-stopped and relaunched the sink every run. A
force-stop is a SIGKILL: the app never runs `onDestroy`, never
`stopAdvertising()`/`close()`s its GATT server, and over many runs the stack
accumulated zombie advertiser registrations until the advertiser subsystem
corrupted and new connections' ATT stopped routing — recoverable only by a
Bluetooth toggle.

**Decision.** Reset a running sink over HTTP (which aborts a stale run and drops
a lingering link) instead of killing it. Use the HTTP reset as a liveness test:
a sink that does not answer — frozen, or its advertiser already dead — is
relaunched to revive it; the healthy common case never relaunches, so no churn
accumulates.

**Consequences.** Repeated runs against a healthy sink no longer wedge its
stack. A separate, environmental limit remains: phone BT stacks still wedge
under a long session of rapid connect/disconnect churn and need a Bluetooth
toggle to recover.

---

## What the numbers say

Measured Pixel 6 → Pixel 8 Pro, 256 KB, confirmed **2M PHY / MTU 517**, every
run delivering all bytes with zero loss:

| payload path | throughput | why |
|---|---|---|
| GATT writes | ~68 KB/s | Android meters roughly one no-response write per connection event |
| L2CAP socket | ~81 KB/s | payload rides L2CAP's own credit-based flow control, bypassing GATT/ATT |

~81 KB/s is roughly a third of a 2M link's ~150 KB/s embedded-to-embedded
ceiling — the gap is Android metering *both* ends, not the radio. No published
Bluetooth throughput figure is Pixel-to-Pixel; the ones that exist put a mobile
on only one end. The connection interval is not app-settable on Android (only
the peripheral can request one, and only three coarse priority buckets are
exposed), so the per-event ceiling is the platform's, not simble's.

## Running it

From the Testing → Data page: pick a phone for each end and set the payload path
to GATT or L2CAP. Or by hand:

```bash
# one intent — the source scans for the sink's advertised name and pushes bytes
adb -s <source> shell "am start -n com.simble/.SimbleActivity \
  --es role source --es target 'Pixel 8 Pro' --ei bytes 262144 \
  --ei fast 1 --es link l2cap"

# or the whole pair, scripted (launches the sink, drives the source, reads both
# phones' clocks off the REPORT)
.claude/skills/phone-throughput-bench/scripts/bench-pair.sh <source> <sink> 262144
```
