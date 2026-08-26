# Measurement regions

**Status: a specification, not a description.** Nothing here is built yet.
`src/device/throughput.rs` still measures with the linear segment chain this
document argues against. Read this as a design record: it says what to build
and why, and it is superseded rather than edited. It becomes stale the moment
someone implements it — at which point the parts that survived belong in
rustdoc on the code, and this file keeps only the reasoning.

## The question it answers

Today a benchmark run is timed by the runner that performs it. `BulkCentral`
holds its own stopwatch, decides for itself when a phase ended, and carries a
per-phase watchdog to notice when one never does. Every new measurement — a
different profile, a script on a phone, a Rev 1.1 versus Rev 1.2 comparison —
would need its own copy of all three.

The alternative is to measure at the layer where the events already happen:
the packets crossing the controller boundary, which `live.rs` **already
captures in full** as btsnoop. The timings would then be a derivation over a
stream that exists, rather than bookkeeping each runner repeats.

Two things have to be right for that to work, and this document is about the
second one.

## Why not have scripts mark their own phases

Because a script can only time what it can see, and what it can see is the
wrong thing.

A script sees `write(chunk)` return. It cannot see the gap between *queued
into a `VecDeque`* and *handed to the controller*, and that gap is where the
time goes: write-without-response lets a central hand 256 KB to its
controller far faster than the link drains it. A script-marked stopwatch
reports a fast transfer over a link that has moved almost nothing — wrong in
the flattering direction, which is the worst direction.

Script marks also measure the author's discipline rather than the protocol.
Two revisions of a script have to be instrumented identically or the
comparison between them is noise, which is exactly the comparison a
Rev 1.1/1.2 exercise exists to make.

There is a sharper version of the argument, and it is not hypothetical.
`charge()` was accounting placed by hand at one call site. A second call site
existed — `step`, which is public and which callers legitimately drive
directly — so ACL packets were billed twice and credited once, the budget sat
at zero, and a healthy two-dongle link reported *"stalled in transfer — 0 of
16384 bytes"*. Hand-placed accounting is correct only while every caller
routes through the site you happened to instrument, and nothing enforces
that. Instrumentation at the layer where the packets are actually produced
cannot be bypassed by a caller who calls the API a different way.

**Scripts should contribute names, never measurements.**

## What the current model cannot express

The segments are stored as a chain of end stamps:

```rust
started_ms, discover_end_ms, connect_end_ms, negotiate_end_ms,
last_queued_ms, report_arrived_ms
```

Each segment starts where the previous one ended. That is a partition of a
line, and it forces four properties that are all false of Bluetooth:

- **Nothing nests.** `negotiate` is really MTU exchange, then service
  discovery, then PHY request, then subscribe. When a run stalls in
  negotiation, *which of the four* is precisely the question being asked, and
  the model cannot hold the answer.
- **Nothing overlaps.** Real stacks pipeline; a chain forbids it by
  construction.
- **Nothing happens outside a segment.** Controller bring-up, a `Read Buffer
  Size` follow-up, a credit return — all of it is attributed to whichever
  segment happened to be open.
- **A phase has exactly one ending.** This is the interesting one.

## An operation has two endings, not one

Nearly every Bluetooth operation is split: the call that starts it and the
event that finishes it are different packets, arriving at different times,
and *the acknowledgement in between is neither*.

| | opens | acknowledges | completes |
|---|---|---|---|
| scan | `LE Set Scan Enable` | `Command Complete` — scanning is on | `LE Advertising Report` — **`Found`** |
| connect | `LE Create Connection` | `Command Status` — request accepted | `LE Connection Complete` — **`Connected`** |
| MTU | `Exchange MTU Request` | — | `Exchange MTU Response` |
| write w/o response | `ATT Write Command` | `Number Of Completed Packets` — the *controller* took it | **nothing exists** |

The middle column is the trap. `Command Status` for `LE Create Connection`
means "I will attempt this", not "connected"; a model that closes the region
there reports a 0.2 ms connect that is really the controller saying *sure*.
So the open edge is an API call, the close edge is a completion notification,
and the acknowledgement is a third kind of edge that closes a *nested*
region rather than the outer one. That is the distinction this whole design
turns on.

The bottom row is the reason both ends of a bulk transfer must be measured.
Write-without-response has no completion notification at any layer; the
closest thing is the controller confirming it took the buffer, which says
nothing about arrival. A region with no available close edge must be closed
by the *peer* — which is what `BulkSink`'s control-point report already does,
and why `BulkReport::confirmation` exists.

## The model

A **region** is an interval with an explicit open edge and an explicit close
edge, a name, and a parent. Regions form a tree; siblings may overlap.

Names follow the split: **the region takes the gerund, the closing edge takes
the perfective.**

| region | closes on |
|---|---|
| `Discovering` | `Found` |
| `Connecting` | `Connected` |
| `Negotiating` | `Negotiated` |
| `Transferring` | `Transferred` |

This is not new vocabulary — it is what the four existing segments already
mean, which is a good sign for the model. The gain is that `Negotiating` now
has children (`ExchangingMtu`/`MtuAgreed`, `Discovering services`/`Found`,
`Subscribing`/`Subscribed`), and that each of them has its own accepted-versus-
completed pair where the protocol provides one.

Regions come from two sources into one tree:

- **Protocol regions**, opened and closed automatically by the packets
  crossing the controller boundary. Identical for every script, every
  controller, every backend. Nobody can forget to add them and nobody can
  place them unfairly.
- **Script regions**, opened and closed by the script for meaning the
  protocol cannot infer — `Rev 1.1 handshake`, `retry burst`. The script
  supplies the *name and the bracket*; the clock is still the API's.

## Unclosed regions are the diagnostic

This is the property worth building the model for, and it generalises the
single most expensive class of bug in this project.

Every serious failure in recent work was the same shape — **a state with no
exit**:

| what happened | as a region |
|---|---|
| `stty -f <path>` blocked uninterruptibly | opened, never closed |
| a write to a controller that stopped draining | opened, never closed |
| ISO buffers exhausted, 200 SDU/s into 8 buffers | could never open |
| discovery never heard the peer | `Discovering` never closed |
| ACL double-charge pinned the budget at zero | `Transferring` never closed |

Each was found by a different bespoke watchdog, or by nothing at all — the
blocking write sat at 0% CPU looking healthy. Under a region model none of
them needs its own detector: **an open region with no close edge is the
failure**, uniformly, and the deepest still-open region names the phase to
blame. `BulkOptions::timeout_ms` stops being a per-phase stall timer and
becomes one rule over the tree.

It renders for free, too: an unterminated bar is visually obvious in a way
that a missing bar is not.

## The part that is actually hard: correlation

Close edges must be matched to the right open region, and Bluetooth does not
hand you a call ID. The matching key differs per operation — opcode for
commands, connection handle for link operations, `(opcode, handle)` for GATT
— and where several operations of the same kind are in flight at once there
may be no distinguishing key at all.

This is where the design can go quietly wrong. A mispaired close does not
error; it produces a plausible bar of the wrong length, which is worse than
no measurement. So:

- Every region type declares its correlation key explicitly. No implicit
  "most recent open region of this name" fallback, which is the rule that
  looks right until two operations overlap.
- Where the protocol genuinely provides no key, the region is marked
  *ambiguous* and the deriver refuses to pair rather than guessing.
- The deriver is tested against captures with deliberate overlap, not only
  against clean sequential runs — the same lesson as everything else here:
  simulated agreement proves nothing.

## Regions whose two edges are on different clocks

A region opened by the central and closed by an event the *peripheral*
observed spans two hosts. `throughput.rs` already has the right taxonomy for
this, and it should be lifted to the region model unchanged rather than
reinvented:

- `server-stamped` — both edges on the caller's own clock. The honest number.
- `peer-reported` — the far edge is real but on another clock; the byte count
  is trustworthy, the instant is when its report reached us.
- `unconfirmed` — no far edge exists. **Bytes sent, not confirmed delivered**,
  and a reader must be told so.

A region carries its confirmation level, and a tree containing any
`unconfirmed` region cannot be presented as a measured total. This is the
same reason the phone-as-backend work exists: a network in the loop turns
`server-stamped` into something weaker, silently.

## Consequences for what exists

- **The four segments no longer sum to the total.** Nesting and overlap make
  that arithmetic meaningless, and there is a test asserting it today —
  `the_four_segments_are_stamped_in_order_and_sum_to_the_total`. It should be
  replaced by containment (a child lies within its parent), not deleted
  quietly.
- **`advance_phase` and `watchdog` largely go away**, replaced by the open/
  close rules and the one unclosed-region rule.
- **Emission is Perfetto's JSON trace format**, which is a tree of nested
  slices — the model above is already its shape, so export is a dump rather
  than a translation, and a run opens in the real Perfetto UI with zooming and
  selection for free. `test-strategy.md` argues this at more length.
- **It works on captures, not only live runs.** Since the deriver reads the
  H4 stream that `live.rs` already records, an old btsnoop file can be
  region-ified after the fact — including one taken from a phone or from
  Wireshark, where no runner of ours was present at all.

## Deliberately not decided

- Whether protocol regions are derived inside the transport layer or by a
  separate pass over the captured stream. The second is more testable and
  costs a buffer; it is probably right, but it is not settled here.
- Whether script regions can close a protocol region's parent, or only nest
  within whatever is open. Allowing it is more expressive and much easier to
  misuse.
- What an emulator run means. Region *counts* are exact there — a revision
  that removes a round trip is deterministically visible — but durations are
  host-stack software time, not radio time. The model should probably refuse
  to report durations from a backend that cannot support them, rather than
  reporting them with a caveat nobody reads.
