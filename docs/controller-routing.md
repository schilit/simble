# Controller routing

**Status: a specification, not a description.** None of this is built. It
records a design and the constraints measured against the tree on
2026-08-25, and is superseded rather than edited.

It also rests on **one unverified premise**: that Bluetooth works in the
Android emulator on the system image to hand (API 34 here). That was
deliberately not tested — emulators are a later thread — and it is the first
thing to check before any of this is worth building, because if it is false
the whole substrate argument changes.

## The question

To make an emulator useful beyond simulation, something has to decide what
backs a given device's controller: a simulated ether, or a real radio. That
decision does not exist anywhere in the tree today, and it cannot be added
where it first appears it should go.

## What exists

Verified, not remembered:

| | what it is |
|---|---|
| `transport/netsim.rs` | `NetsimScene` — joins **netsimd** as a client, one WebSocket per device |
| `transport/usb.rs` | `UsbScene` — a dongle pool; `next_dongle()` hands devices real radios |
| `transport/rootcanal.rs` | `RootcanalTransport` — a *client* of a rootcanal server, H4 over a stream |
| `transport/live.rs` | `LiveTransport`, `SIMBLE_HCI` — the stack is already transport-agnostic |

The scenes already share a shape — `new`, `add_peripheral`, `pump`, `tick` —
without sharing an interface. That convergence is the argument that a routing
layer is a formalisation of something real rather than a new abstraction
invented for its own sake.

Two things are absent and both matter:

- **No gRPC anywhere.** No `tonic`, no `prost`, nothing. Serving netsim's
  interface introduces a dependency class this crate does not have.
- **`rootcanal-rs` is not a runtime dependency.** It is a `cfg(rootcanal_oracle)`
  *dev*-dependency, reachable only by `tests/rootcanal_oracle_test.rs`, and
  deliberately so: an optional path dependency fails `cargo package`, and
  simble is published. Using it at runtime is blocked until it is on
  crates.io, or the feature is excluded from the published crate.

That second point is easy to miss, because the submodule is right there and
its README advertises multi-controller Link Layer RF routing. The capability
is real; the *wiring* to use it outside a cfg-gated test is not.

## The distinction everything turns on

`bumble-bridging-evaluation.md` already names it in one line, in the summary
table: HCI-layer facilities are **"No — moves a host."**

Routing a device's HCI to a dongle **moves that host onto a different
controller**. It does not join two ethers. The instant a device is
dongle-backed, its link layer is transmitting over real RF: it can now hear
**real** peers and can no longer hear **simulated** ones. It has left the
simulated medium, not bridged to it.

So this is not a bridge and must never be described as one. **Routing chooses
which world a device lives in.** Everything below follows from that, and the
physics does not change: a rootcanal PDU is a data structure and a real packet
is on an antenna.

## Why it cannot be built inside netsim

netsimd is not ours, and two measured facts close the door:

- **The emulator's backend is netsim's choice, not ours.** We are not in that
  path, and cannot route what we do not sit in.
- **netsimd exposes no phy port** (§2 of the bridging evaluation). A
  standalone rootcanal does, and joining at the link layer was *proven*
  against one — 546 link-layer packets in 4 s — but that facility is absent
  from netsimd specifically.

## The hook: serve the interface ourselves

An emulator can be pointed at something other than netsimd. Bumble
demonstrates it: `android_netsim.py` `mode=controller` serves netsim's
`PacketStreamer` gRPC so an emulator connects to Bumble instead. That is
existence proof, not speculation.

If **simble** serves that endpoint, the emulator connects to us and the
routing decision becomes ours by construction — no netsim patch, no upstream
dependency:

```
emulator ──gRPC PacketStreamer──> simble ──┬─> dongle        (real RF)
                                           ├─> rootcanal     (simulated ether)
                                           └─> in-process Link (deterministic)
```

## What can back a device, and who hears whom

The routing switch is the cheap part. Ether membership is the semantics, and
it is what a caller actually needs to reason about:

| backing | hears | deterministic | catches controller quirks |
|---|---|---|---|
| in-process `Link` | others on the same `Link` | yes — `tick()`-driven | no |
| rootcanal (standalone, phy socket) | others on that rootcanal | no | no |
| netsimd (as client, today) | others on netsimd, incl. emulators | no | no |
| **USB dongle** | **only real RF** | **no** | **yes** |

The last row is the whole point and the whole cost in one line. A
dongle-backed emulator gains real controller behaviour and simultaneously
loses every simulated peer. There is no configuration in which it has both.

## What it buys

`test-strategy.md` concedes the emulator's honest limit: *an emulator runs
Android's host stack over rootcanal's controller, not a phone's firmware* —
so it catches host-stack and profile bugs, which is most of them, and never
controller quirks.

A dongle-backed emulator is **Android's real host stack on a real
controller**, which closes exactly that gap. The class of bug it would newly
reach is the class that has actually cost time here: a CSR answering `LE Read
Buffer Size` with zeros, an ACL pool smaller than the code assumed, ISO
credit behaviour. Today those need a phone.

## Costs, stated plainly

- **A gRPC server is new dependency surface** in a published crate that
  currently has none. It should be feature-gated and excluded from the default
  build.
- **`rootcanal-rs` cannot be a runtime dependency yet.** Until it is
  published, the rootcanal-backed row is reachable only as a standalone
  server over `RootcanalTransport`, not in-process.
- **Single tenancy is a trap already sprung upstream.** Bumble's
  implementation is single-tenant: a second device gets
  `PacketResponse(error='Device busy')` from `lease_sink`. A scene wants many
  devices, so multi-tenancy has to be in the design from the start rather
  than discovered when the second emulator connects.
- **Dongle count is a hard ceiling.** Routing *n* devices to real radios needs
  *n* dongles; `UsbScene`'s pool already models this, and the failure when the
  pool is empty must be a clear error rather than a silent fallback to
  simulation — a device that quietly stops being real invalidates the run
  without saying so.

## Sequencing

1. **Verify emulator Bluetooth works at all** on the image to hand. Everything
   else is void if it does not.
2. **Formalise the backend interface** the four scenes already almost share.
   Useful on its own, no gRPC required, and it is what makes the rest small.
3. **Serve `PacketStreamer`**, multi-tenant, feature-gated — with the
   in-process `Link` as the only backing at first, which is testable without
   an emulator in the loop.
4. **Add dongle backing**, and with it the ether-membership rules above made
   explicit in the API rather than left to documentation.

## Open questions

- Whether a single scene may mix backings at all. It is expressible and it is
  a good way to produce a run whose halves cannot hear each other while
  *looking* like they should. Refusing the mix outright may be the better API.
- Whether a measurement taken across a dongle-backed emulator is quotable.
  Per `measurement-regions.md` the host stack is real but the timing includes
  the emulator's scheduling, so it is neither the simulated case nor the phone
  case and probably needs its own confirmation level.

## 2026-08-28 — the hci-router refinement, and a single-client cut

A design pass named the router and, more usefully, shrank it.

**One client means no server.** The picture above — a router that owns the
controller and *serves* many clients over an interface — is the netsim model,
and netsim already is that. simble has a single client: its own actor and the
scripted devices it loads. So it does not reimplement a multi-tenant server. The
actor holds a `rootcanal-rs` handle and pumps its main loop **directly,
in-process**; per-device routing (simulated ether vs real radio) stays internal
to that one actor. The multi-tenant path — serving the interface to many clients
— is only for backing the Android emulator or other external clients, which is
the ambition above, not this.

**A main loop has one owner.** `rootcanal-rs` runs a scheduler loop and cannot be
driven by two things. That is the concrete reason "a shared router called by two
actors" is wrong: it splits ownership of that loop. Single-client, the owner is
the actor itself; if a multi-client server is ever built, the loop is owned by
*that* actor and clients attach — never shared.

**The backend interface is a `Controller` trait; the router implements it too.**
Formalise the shape the transports already share (`new` / `add_peripheral` /
`pump` / `tick`) as a trait, expressed as a `Sink<HciPacket>` +
`Stream<HciPacket>` pair. `rootcanal-rs`, a USB dongle, and an external netsim
each implement it; the router implements it as a **composite** — a backend made
of backends, so it substitutes and nests. The `futures` / tokio /
tokio-tungstenite this needs lives in the router crate, never `simble-core`,
which stays no-async-runtime.

**Framework-agnostic — bring your own actor.** Which actor framework wraps this
(actix, ractor, a hand-rolled tokio task) is the consumer's opinion and it
differs, so the library depends on none. It exposes the router as a *driveable*
thing — the `Controller` trait, the `Sink`+`Stream` pair, a runtime-neutral pump
— and the actor is a thin wrapper the consumer writes, exactly as simble's host
stack is already a driveable state machine (`handle_packet` / `poll` / `tick`)
that any loop can own. "Make it an actor" never meant "depend on an actor
framework."

**Backends are Cargo features; the default is the in-process `Link`.** The
default backend is the deterministic, dependency-free in-process `Link`
(tick-driven, tier 1) — so the router builds, packages, and runs everywhere with
nothing to install and nothing that can break `cargo package`. Real radio is default too:
**`usb`** (`nusb`) is a *default-enabled* feature — on by default on native
(cfg-gated off wasm, like `simble-stack`), so real dongles work out of the box,
but a pure-simulation consumer can drop it with `default-features = false`. The
dividing line is *cost*: `Link` and `usb` are free (no packaging or dep burden —
`nusb` is pure-Rust, on crates.io), so they default on; the ones with a cost are
opt-in — a **`rootcanal`** feature adds an in-process `rootcanal-rs` ether (a path
dep) and a **`netsim`** feature adds ws:// forwarding to an external netsim (the
async/tokio-tungstenite deps). Private networks follow the same axis — multiple
`Link` ethers by default, `rootcanal` copies when that feature is on. The feature
split fixes the *build*
default (nobody compiles `rootcanal-rs` unless they ask); it does not by itself
fix *publishing* — a `rootcanal-rs` *path* dep blocks `cargo package` even as an
optional feature-dep, so publishing the router crate still waits on `rootcanal-rs`
reaching crates.io (or the cfg-gated-dev-dep trick). The win is that the
constraint is isolated to the opt-in feature; the default (`Link`, `usb`) is
clean and publishable.

**Switching a backend injects an HCI Hardware Error (0x10).** Rather than migrate
live controller state, a switch injects a Hardware Error Event upward so the host
re-initialises against the new backend — the host's re-init sequence
(`init_commands`) already exists. Active connections drop, as they would on a
real controller failure; the new work is recognising 0x10 and rebuilding
advertising/GATT state.

**Correction to "serve the interface ourselves."** Two protocols, not one:
simble-as-device speaks netsim over **ws://** (`NetsimTransport`), but the
**emulator** speaks its controller over **gRPC `PacketStreamer`**. So routing
simble's *own* devices needs no gRPC; *backing the emulator* does. An earlier
claim that a ws:// proxy "sidesteps the gRPC blocker" holds only for the first
case — gRPC lives at the emulator-facing edge, and nowhere else.
