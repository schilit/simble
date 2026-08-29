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

**A v2 wire protocol: list, then attach.** The current netsim protocol is one
fixed verb — "connect to the ether" (`?address=…` and you are on netsim). v2 is
an *additive superset*: a control op **`.list`** enumerates the backends the
router offers (Link ethers, rootcanal ethers, dongles, an external-netsim
forward), and **`?backend=foo`** attaches the stream to a *named* backend rather
than only netsim. Serve both — v1 clients (and real netsim) keep working; v2 is
the drop-in-in-front upgrade. Keep two query axes distinct: *which backend*
(`?backend=…`) is separate from *which device* (`?address=…`, which v1 already
uses). And a `?backend=` is a **request, not a grab**: the router owns the
hardware and *routes* the client there or refuses if it is exclusively held —
clients never open a dongle themselves, so v2 does not reintroduce
two-processes-fighting-over-a-dongle.

**One ws:// transport carries both.** `.list` is request/response and
`?backend=` is a stream, but both ride ws://: a stream is the main path, and a
single-shot (open, one request, one response, close) is also fine — RPC over
WebSocket is well-trodden, and ws:// is the browser-friendly transport (no CORS
preflight). So the URL disambiguates control-vs-stream and one ws:// server
serves both — which is where **F5 (control) and F6 (data) collapse into a single
server**. Request/response is *semantics*, not transport, so the same `.list`
handler can also answer plain HTTP for `curl`/tooling if wanted; that is cheap
optionality, not a second server you must build.

**Router-node startup: an optional server socket plus backend wiring.** A node
comes up wired on two sides — an optional **server socket** (the downstream v2
endpoint others attach to) and its **backends** (default `Link`; `usb`,
`rootcanal`, or a **netsim forward** by feature/config). Both sides speak the
same v2 protocol, so the node is just a link in a chain (`clients → [node] →
netsim`) and could itself be someone's backend. Both are *optional*: a bare
`simble mcp` session driving its own built-in backends needs neither socket. The
single-owner rule holds — **one** router-owning process owns the metal, and
everyone else (browser, a second tool, a chained node) attaches to its server
socket rather than co-owning; the socket is how others *share*, not a second
grab.

**Private networks: a namespace of ethers.** The router is not "the ether" but a
*named set* of them — `net-a`, `net-b`, … each its own `Link` (default) or
`rootcanal` copy (with that feature), alongside the dongles and any external
forward. A device attaches to one by name; `.list` enumerates them; the router
creates and destroys them on demand, each just another in-process loop it owns.
This is what makes the multiple customers safe: every client / agent / CI job
gets an **isolated world** with no cross-talk — real multi-tenancy. Two hard
edges: a device lives in **exactly one** ether (this is not a bridge — crossing
needs an explicit bridge *device*, never a router toggle), and isolation is a
**simulated-only** guarantee — you can spin up N private rootcanal/Link ethers,
but real dongles all transmit into the one physical air, so a test that needs
isolation must be sim-backed.

**netsim is a leaf, not a relay.** The v1 netsim protocol has no forwarding
verb, so netsim can only *terminate* a chain: you forward **to** it, never
**through** it (`mcp → router → netsim` works; `mcp → netsim → dongle` cannot —
netsim will not forward to the dongle). Forwarding is a v2/router-only
capability; the routing intelligence is always the node we add **in front** of
netsim, never something netsim provides — which is the same limitation as "no
phy port" / "cannot be built inside netsim," seen from the routing side.

## 2026-08-29 — premise verified: Bluetooth works in the emulator

The one unverified premise from the top — "does Bluetooth work in the Android
emulator on the image to hand (API 34)" — is now **checked, and true**.

Booted `Pixel_7_API_34` headless (API 34, `sdk_gphone64_arm64`). `svc bluetooth
enable` returned `enable: Success`; `dumpsys bluetooth_manager` reports
`state: ON`, `enabled: true`, `address: 01:00:00:BB:BB:BB` (a netsim-assigned
BD_ADDR — the `BB:BB:BB` is netsim's signature), and `netsimd` is running as the
backing controller. So the Android host stack comes up on a functional
netsim/rootcanal controller and reaches a working `ON` state.

That is the *substrate* confirmed, not the whole idea: it proves the emulator
has real Bluetooth over a virtual controller. The next thing to prove is the
*routing* payoff — that the emulator can be pointed at **us** instead of netsimd
(the gRPC `PacketStreamer` hook) and then reach a dongle. But the gating premise
that could have sunk the effort ("if it is false the whole substrate argument
changes") is retired: it holds.

## 2026-08-29 — the v2 API surface and device lifecycle

One ws:// endpoint per node; **JSON for control, binary H4 for HCI**, URL- or
op-disambiguated. Additive over v1 (v1 = "attach to netsim").

**Two lists — capacity vs state.**
- `/v2/backends` — what *can* back a device: `link` (default), `rootcanal`
  copies, `usb` dongles, a `netsim` forward (marked `leaf`). Each with
  `real`/`deterministic` flags.
- `/v2/devices` — what *is* running: each device's handle, **its backend (its
  route)**, address, connection state. A route is a device's backend binding, so
  this lists routes; it is what `route` needs (handles) and the observability
  window.

**The ops.**
- `run` / `spawn` — run a script **server-side** on a backend (`run` one-shot →
  `result`; `spawn` persistent → a `device` handle). The host runs *next to* the
  controller, so HCI never crosses the wire — the low-latency, quotable path, and
  the preferred one for SimBLE-native devices.
- `attach ?backend=` — **client-side** host, a raw H4 HCI stream. The only mode
  with per-packet HCI on the wire; for external stacks that cannot move
  server-side — the emulator, whose edge is gRPC `PacketStreamer`, not this ws://.
- `route {device, backend}` — switch a device's backend. **Drops, never
  migrates:** a live connection is controller-resident link-layer state, so the
  op injects an **HCI Hardware Error (0x10)**, the host resets, drops every
  connection, and re-inits on the new controller (which has a **different
  address**; for sim↔real it is a **different world** entirely — the host leaves
  the old ether). Any reconnect is a **new** connection, never the old one moved.
- `create` / `destroy` network — the private-ether namespace.
- `stop {device}` / connection close — teardown. Devices are **owned by the
  connection that started them**: `stop` releases one (graceful — clean peer
  disconnect), closing releases all (best-effort). Teardown *always releases the
  backend* — which is what frees an exclusive dongle when a client dies, rather
  than leaking it as "device busy".
- `tick {seconds}` — advance **simulation** time (sim-only; a real-radio device
  runs on real time). A host→scene command for what BLE cannot express.
- `send {device, message}` — mutate/trigger a running device (the pub-sub
  `setGeneration` precedent); needs the **script to expose the input**.

**Two script inputs, by field, never guessed.**
- `script` — inline Rhai **source text**. A filename is a *client-side* concept
  (the server cannot read the client's disk), so the client reads its own file
  and sends the text; the field's name is the type, no path/source sniffing.
- `device` — a catalog **name the server resolves**. Never a client-supplied
  server path (path-traversal). The catalog is a shorthand; `script` is general.

**Interaction model.** The primary way to interact with a running device is *as a
Bluetooth peer* — connect / read / write / subscribe over BLE; the device just
does its job, no side channel. Host→script commands (`tick`, `send`) are the
out-of-band control BLE cannot carry (time, test-only mutations/misbehavior).
Events flow out (`stage`/`log`/`result`); commands flow in.

**Safety.** Every `run`/`spawn` is Rhai, and Rhai is sandboxed (no I/O,
deterministic), which is what makes accepting a device definition — custom or
catalog — over the wire safe where native code would not be.
