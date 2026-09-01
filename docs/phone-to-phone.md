# Phone-to-phone bulk transfer

> Implemented on the `phone-to-phone` branch. Throughput figures are
> representative of the lab hardware, not fixed.

---

## 1. The central is the phone's own GATT client, not simble-over-HCI

**Decision.** Give the Android app a *source role*
([`BulkSource`](../android/app/src/com/simble/BulkSource.java)) that drives the
transfer with Android's own `BluetoothGatt` **client**, not simble's HCI stack.

**Why.** Stock Android does not expose its controller as an HCI radio over adb,
so a phone cannot be simble's central over HCI — which appears to rule out
phone-to-phone transfer, the common real-world shape no published Bluetooth
number covers. But that limit is simble-over-HCI's only; the phone's own client
is a perfectly good central. Discovery, MTU/PHY negotiation, the control point
(`BEGIN`/`FINISH`/`REPORT`), and the byte-count report all stay on GATT and ride
the link, so no laptop is in the data path — it only launches the two apps over
adb. Not an RPC/remote-control client (the shape
[`phone-as-backend.md`](phone-as-backend.md) analyses), because that keeps the
host in the loop for every operation — exactly the network this measurement
exists to remove.

**Consequences.**
- The bridge orchestrates a run over adb (`am start … --es role source`), so the
  browser — which cannot fire an intent — drives it through a `/pair-run`
  endpoint.
- Both ends must run SimBLE Android; a dongle central stays on the GATT path.
- The run reports tracing-style phase spans (discover / connect / negotiate /
  transfer), the same breakdown the dongle path gives.

## 2. Payload can leave GATT for an L2CAP socket; control stays on GATT

**Decision.** Add a second payload path — an **L2CAP Connection-Oriented
Channel** — selectable per run (`link=gatt|l2cap`). The sink opens an insecure
L2CAP server (Android 10+) and publishes its PSM in a read characteristic; the
source reads the PSM and streams the payload over the socket, where L2CAP's own
credit-based flow control packs the connection event. GATT is kept for
discovery, control, and reporting; only the payload moves. A source that finds
no PSM falls back to GATT.

**Why.** GATT writes are metered by ATT at roughly one no-response write per
connection event, so a 256 KB run tops out near 68 KB/s on a confirmed 2M /
MTU-517 link — paid twice with a mobile on each end. Moving only the payload
(not replacing GATT wholesale) is the smaller, reversible change: the control
point, service discovery, and REPORT handshake are cheap and already work, and
one setup code path is kept across both modes.

**Consequences (rules the L2CAP path must follow).**
- A socket write returns once buffered, not once transmitted, so the source
  holds the socket open until the sink's `REPORT`, and `report()` closes it —
  closing at the end of the write loop discards the tail.
- `FINISH` over GATT can outrun the still-draining stream, so the sink waits on
  receive *progress* (not a fixed deadline) before it reports.
- Throughput for an L2CAP run is the sink's first-to-last received-byte clock,
  since the source's transfer span is buffer-inflated.

## 3. A wedged sink is reset, not force-stopped, between runs

**Decision.** Reset a running sink over HTTP (which aborts a stale run and drops
a lingering link) instead of killing it. Use the HTTP reset as a liveness test:
a sink that does not answer — frozen, or its advertiser already dead — is
relaunched to revive it; the healthy common case never relaunches, so no churn
accumulates.

**Why.** A force-stop is a SIGKILL: the app never runs `onDestroy`, never
`stopAdvertising()`/`close()`s its GATT server, and over many runs the stack
accumulated zombie advertiser registrations until the advertiser subsystem
corrupted and ATT stopped routing — recoverable only by a Bluetooth toggle. A
separate, environmental limit remains: phone BT stacks still wedge under a long
session of rapid connect/disconnect churn and need a Bluetooth toggle to
recover.

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
