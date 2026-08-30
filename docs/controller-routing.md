# Controller routing

**Status: a specification, not a description.** None of this is built. It
records a design and the constraints measured against the tree from 2026-08-25
onward. The **reference below** (capabilities, types, operations) is the clear
summary; the dated sections after it are the design record — the reasoning,
the corrections, and the one thing actually proven, in the order they happened.

The premise the whole idea once rested on — that Bluetooth works in the Android
emulator (API 34) — has since been **verified** (2026-08-29 note below): the
emulator's Android stack comes up `ON` over a netsim controller. The substrate
holds; what remains is build work, not a gating check.

---

# The SimBLE environment

## What it can do

SimBLE is a pure-Rust Bluetooth Low-Energy and Classic host stack whose devices
are Rhai scripts (hand-written or AI-authored). This document specifies how one
SimBLE environment **routes** those devices onto controllers — and, from that,
what the environment as a whole is capable of:

- **Run scripted devices deterministically, in-process, with no hardware** — the
  `Link` ether, `tick()`-driven and reproducible.
- **Put the same script on real radio with one field change** — target a USB
  dongle instead of `Link`; the device is now physically discoverable.
- **Host many isolated private networks** — parallel tenants (agents, CI jobs)
  each in their own world, with no cross-talk.
- **Drive devices on real phones** — Android and iOS, each through its platform
  API, as first-class nodes.
- *(optional, off by default)* **Back the Android emulator's Bluetooth with a
  controller of your choosing** — a simulated ether or a real dongle. This is the
  *only* piece that needs gRPC (the emulator speaks netsim's `PacketStreamer`,
  Google's protocol), so it is an isolated, feature-gated adapter, never in the
  core — everything else is HTTP REST + ws://.
- **Switch a running device's controller live** — sim↔real — without migrating
  state (a reset event re-homes the host).
- **Speak one ws:// protocol — SimBLE v1** — for all of it (`list` / `run` /
  `attach` / `route` / …), driven from the CLI, an MCP agent, or a browser. It is
  SimBLE's *own first* protocol; separately it can **serve** the netsim protocol
  for compatibility, so real netsim clients and the Android emulator interoperate
  — but that protocol is netsim's, not an earlier version of this one.

## The model: four types

| type | is | created or registered | key fields |
|---|---|---|---|
| **node** | a participant that **owns controllers and executes runs** | **registered** (external, already exists) | `name`, `kind` (router/browser/android/iphone), its controllers |
| **controller** | what a device's host **attaches to** (below HCI, or a phone's platform API) — an *attach point* | comes with its node | `name`, `kind`, **`api_class`** (`hci`/`android`/`coreBluetooth`), **`network`**, `real`/`deterministic`/`attachable`/`runnable`/`available`/`in_use_by` |
| **network** | a **world / ether** — who-hears-whom | sim ethers **created**; `real` is a fixed singleton | `name`, `kind` (`link`/`rootcanal`/`rf`/`netsim`), `devices` (members), `deterministic`/`real`/`shared`/`leaf` |
| **device** | a **running** scripted instance | started by `run`/`spawn` | handle, **`controller`** (its route), `address`, connection state |

The relationships, stated once:

- **A node is exactly what can execute a `run`.** The local `simble` process and
  a phone are both nodes; both allow `run`. **MCP, the CLI, and the v1 ws:// socket
  are *interfaces to* a node, not nodes** — one `simble` process is one node
  reached many ways, which is what keeps a **single owner** of its hardware (two
  interfaces are not two owners; that would be the dongle-contention bug).
- **A node owns controllers; a `run` executes on *that* node, on a controller it
  owns.** You reach a controller by connecting to its owning node — a dongle/sim
  on the local node, a phone's radio on the phone-node. A node does **not** reach
  into another node; **cross-node orchestration is the client's job** — the client
  holds a connection to each node it composes (the local node via MCP, a phone via
  its own interface). A node's own *backends* (including a `netsim` forward) are
  its controllers, not other nodes, so forwarding there is fine; delegating a run
  to an independent node is not. You cannot drive a controller you do not own.
- **A controller has one `network`** (the world it drops a device into) and one
  **`api_class`** (the platform interface, which *gates* what a `run` can do:
  only `hci` allows attach and low-level control; `android`/`coreBluetooth` are
  high-level GATT/advertise only, iOS tightest). `real` controllers (dongles,
  phone radios) are static; sim controllers are minted per device.
- **A network holds many controllers/devices that hear each other.** A device is
  in **exactly one** network (this is not a bridge — crossing needs an explicit
  bridge *device*). Isolation is a **simulated-only** guarantee: `real` is the
  one shared physical world.
- **A device is owned by the connection that started it**; `stop`/close releases
  its controller (which is what frees an exclusive dongle when a client dies).

## Operations

**Four lists (observability):** `/v1/controllers` (attach points, each with its
`network` + `api_class` + availability), `/v1/networks` (worlds + members, incl.
the `real` singleton), `/v1/devices` (running instances + their controller/route),
`/v1/nodes` (participants).

**The verbs:**

| op | does | notes |
|---|---|---|
| `run {script\|device, controller?}` | run a script **server-side** on a controller → `result` | default controller = the local node's `Link`; host colocated with the controller, no HCI on the wire |
| `spawn {…}` | like `run`, but persistent → a `device` handle | lives until `stop`/close |
| `attach ?controller=` | a **client-side** host — raw H4 HCI stream | the only per-packet-on-the-wire mode; for external stacks (the emulator, via gRPC) |
| `route {device, controller}` | rebind a device's controller | **drops, never migrates** (`0x10` reset); may change the *world*; guarded by availability + `api_class`; same-node only (cross-node = re-host) |
| `stop {device}` / close | teardown | releases the controller; close releases all a connection owns |
| `tick {seconds}` | advance **simulation** time | sim-only |
| `send {device, message}` | mutate/trigger a running device | needs the script to expose the input |
| `create` / `destroy {network}` | mint / remove a private sim ether | an **internal resource** |
| `register` / admit `{node}` | admit an **external** node (a phone) | discovery via adb today, dial-in in the fabric |

**Two verbs, deliberately distinct:** **networks are `create`d** (internal ethers
the environment mints); **nodes are `register`ed** (external devices — a phone, a
browser — that already exist, admitted into the fabric). You never `create` a
phone, and you never `register` an ether.

**How to know availability:** the lists *hint* (`available`/`in_use_by`), but they
race — the authoritative answer is the **atomic claim**: `run`/`spawn`/`route`
returns a `device` handle or `controller busy`. Never check-then-claim; an
unavailable controller must error, never silently fall back to simulation.

**Wire encoding:** **control is HTTP REST** (each op an endpoint —
`GET /v1/controllers`, `POST /v1/run`, …; JSON in, JSON out; `curl`-able); the
**one streaming case, `attach` (raw HCI), is a ws:// stream** (H4 frames both
ways). `dispatch` is transport-neutral, so the same handler serves either. Scripts
are always a JSON body — never a URL param.

**No gRPC.** The whole architecture is HTTP REST + the one ws:// stream — no new
dependency class. gRPC arises in *exactly one* place and it is not our choice:
the Android **emulator** speaks netsim's `PacketStreamer` gRPC to its controller
(Google's protocol, baked into the emulator), so serving the emulator would mean
speaking gRPC. That edge — "back the emulator with a real dongle," controller-
routing's original motivation but not its spine — is therefore **dropped from the
core**; if ever wanted it is an isolated, feature-gated `PacketStreamer` adapter,
never in the default build. Everything else needs none of it.

**Safety:** every `run`/`spawn` is Rhai, and Rhai is sandboxed (no I/O,
deterministic) — which is what makes accepting a device definition, custom or
catalog, over the wire safe where native code would not be.

---

# Design record

The sections below are the reasoning that produced the model above, dated in the
order it was worked out. Terminology evolved as it went — "backend" became
**`controller`**, "backing" became **`network`**, and the protocol is **SimBLE
v1** (what these sections call "v2" — the netsim protocol they call "v1" was
never a SimBLE version, just netsim's, spoken for compatibility). So where the
record and the reference above differ in a word, **the reference is authoritative**.

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

One ws:// endpoint per node; **JSON for control, binary H4 for HCI**. Additive
over v1 (v1 = "attach to netsim").

**Wire encoding: URL for connect-time routing, JSON for everything else.** A
WebSocket upgrade is an HTTP GET with *no body*, so whatever the server needs to
establish the connection must be in the **URL** — a short op path, and the attach
stream's `controller` (it binds at upgrade, having no request-message phase).
Everything after the socket opens rides as **JSON messages**: request data,
commands, events. Scripts are *always* a JSON message body, never a URL param —
URLs cap around 2–8 KB and a real Rhai device blows past that. Don't split one
request's data across URL and body; control ops carry it all in one JSON message.

**The term is `controller`, and `network` for the shared medium.** What a
device's host attaches to below HCI *is* a controller (a dongle; a rootcanal/sim
controller) — not a generic "backend". Controllers that share a medium form an
**ether**, i.e. a **network**: a `Link` or `rootcanal` *instance* hosts many
controllers that hear each other. So a dongle is one controller (its ether is the
one real air); a private network is an ether that *mints* a controller per device.

**Three lists — capacity, ethers, state.**
- `/v2/controllers` — the attach points: `usb` dongles and the sim/`rootcanal`
  controllers, each tagged with its **`network`** (the world it drops a device
  into — dongles and phone radios are `real`, a rootcanal controller is its net),
  plus `real`/`deterministic`/`available` flags. Grouping by `network` is the
  who-hears-whom view from the attach-point side; it cross-references
  `/v2/networks` (controller → world → members). `real` controllers are static;
  sim controllers are minted per device, so they come and go with running devices.
- `/v2/networks` — the worlds and their members (who-hears-whom): the created,
  isolated sim ethers `link` (default) and `rootcanal` private networks; the
  singleton **`real`** RF world (`kind:"rf"`, `shared:true`) — *not*
  creatable/isolatable, entered through a dongle, and its `devices` list only our
  own dongle-backed ones (external real peers are on that air but unlisted); and
  the `netsim` forward (`leaf`). Every device is on *some* network; `real` is the
  one shared, non-isolated world, which is why isolation is a simulated-only
  guarantee.
- `/v2/devices` — what *is* running: each device's handle, **its controller (its
  route)**, address, connection state. A route is a device's controller binding,
  so this lists routes; it is what `route` needs (handles) and the observability
  window.

**Controller availability: the list hints, the claim decides.**
`/v2/controllers` reports `available` per entry (and `in_use_by` for an exclusive
one) — good for a UI or for picking, but *advisory*: between listing a dongle free
and claiming it, another client can take it (a TOCTOU race), so do not gate on it.
The authoritative answer is the claim itself: `run`/`spawn`/`route` on a controller
either succeeds (returning a `device` handle) or returns `controller busy`,
**atomically** — the router is the single owner and serialises claims, so the
error cannot race. Dongles are the case that matters (exclusive); sim networks are
effectively always available (mint-on-demand). An unavailable controller must be a
clear error, **never a silent fallback to simulation**. Optional: a `wait:true`
that queues the claim until one frees, and a subscribable `/v2/controllers` that
streams availability changes instead of polling.

**The ops.**
- `run` / `spawn` — run a script **server-side** on a controller (`run` one-shot
  → `result`; `spawn` persistent → a `device` handle). The host runs *next to* the
  controller, so HCI never crosses the wire — the low-latency, quotable path, and
  the preferred one for SimBLE-native devices. (Join a `network` and a controller
  is minted for you; name a dongle to use that one.)
- `attach ?controller=` — **client-side** host, a raw H4 HCI stream. The only mode
  with per-packet HCI on the wire; for external stacks that cannot move
  server-side — the emulator, whose edge is gRPC `PacketStreamer`, not this ws://.
- `route {device, controller}` — rebind a device to a different controller
  (swap the controller *under* its host). **Drops, never migrates:** a live
  connection is controller-resident link-layer state, so the op injects an **HCI
  Hardware Error (0x10)**, the host resets, drops every connection, and re-inits
  on the new controller (which has a **different address**). Any reconnect is a
  **new** connection, never the old one moved. Because a controller carries a
  `network`, routing to a controller on a *different* network **changes the
  world** — the device leaves its ether (sim↔real, or between private nets) and
  its peer set changes; a same-network swap (e.g. `dongle-0`→`dongle-1`) keeps
  the world. Guarded by the same rules as a claim: the target must be
  **available** (else `busy`) and its **`api_class`** must support the device
  (an HCI-level device cannot route onto a `coreBluetooth` phone controller).
  Scope: `route` swaps the controller under a host that **stays put** — an
  attached host (the emulator, whose controllers the router mediates) or a
  server-side device on the **same node**. Moving the host *across nodes* (a
  server-side device onto a phone) is not a route — it is a **re-host** (`stop`
  then `run`/`spawn` on the new node), since a run executes on the controller's
  owning node.
- `create` / `destroy` network — the private-ether namespace.
- `stop {device}` / connection close — teardown. Devices are **owned by the
  connection that started them**: `stop` releases one (graceful — clean peer
  disconnect), closing releases all (best-effort). Teardown *always releases the
  controller* — which is what frees an exclusive dongle when a client dies, rather
  than leaking it as "device busy".
- `tick {seconds}` — advance **simulation** time (sim-only; a real-radio device
  runs on real time). A host→scene command for what BLE cannot express.
- `send {device, message}` — mutate/trigger a running device (the pub-sub
  `setGeneration` precedent); needs the **script to expose the input**.

**Where `run` executes: the node that owns the controller.** `run` has no fixed
locus — it *follows the controller*, colocated, which is the whole low-latency
point. A controller the router owns (dongle, `rootcanal`, `Link`) → the script
runs **in the router process**, the host stack driving HCI in-process; a browser's
sim/netsim controller → **in the browser** (wasm); a **phone's** Android radio →
**on the phone** (the `android/rust` JNI engine), because only Android can drive a
phone's radio — so a `run` targeting a phone is **delegated** to the phone-node and
executes on-device. The rule: a node runs scripts only on controllers **it owns**;
you cannot drive a controller you do not own. Two caveats: the *runtime* differs
even though the Rhai definition is portable (host-stack → GATT/HCI on a
router/browser node; → Android's `BluetoothGattServer`/advertising/L2CAP on a
phone, which has no HCI); and on-device Rhai is **scaffolded** (`android/rust`), so
today a phone runs its fixed Java behaviours while the router-process `run`
(dongle/sim) works via MCP's `LiveBackend`.

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

## 2026-08-29 — the node model (controllers, API classes, delegation)

The entity that was implicit until now: a **node** is a v2 participant that
**owns controllers and executes runs**. Controllers belong to nodes, and a `run`
executes on the node that owns the named controller.

**The local `simble` node is the default.** The `simble` process you launch is a
node — the "full" one: it owns the default **`Link`** controller (plus
`usb`/`rootcanal` by feature, a `netsim` forward by config), runs scripts on them,
and — when it opens a server socket — is the **router** other nodes attach to. Its
modes are faces of one node: `simble mcp` is it in MCP mode, `simble --usb` in
bridge mode; netsim is a controller/medium *under* it, never a node. A `run` with
**no `controller`** targets this node's `Link`, so "default node + default
controller" is the local process on its deterministic ether. "Router" is a *role*
it plays while serving a socket, not a separate thing.

**A controller carries an `api_class`, and it gates the run.** The class is the
platform interface used to drive that radio; the capability flags derive from it.

| `api_class` | driven via | envelope |
|---|---|---|
| `hci` | raw HCI, the full host stack | full — attachable, precise timing, arbitrary/malformed PDUs, deliberate misbehaviour |
| `android` | `BluetoothGattServer` / advertise / L2CAP sockets | high-level GATT/advertise/L2CAP; no HCI, no raw interval/DLE |
| `coreBluetooth` | `CBPeripheralManager` / `CBCentralManager` | most restrictive — no MAC address, constrained advertising/background |

An iPhone cannot run the Android API — it is `coreBluetooth`, a different and
tighter class. The Rhai definition is portable, but its *realisation* maps onto
the target's class, and the class is a **gate**: a well-behaved GATT device runs
on any class, but a script needing HCI-level control (a malformed advertisement, a
precise connection interval, an injected PDU) runs only on `hci`. So `run` checks
the script against the controller's `api_class` and rejects cleanly ("needs `hci`;
`phone-0` is `coreBluetooth`") rather than silently doing something lesser. A
sample entry:

```
{"name":"phone-0","kind":"iphone","node":"phone-0","api_class":"coreBluetooth",
 "network":"real","real":true,"runnable":true,"attachable":false}
```

**Registration.** A phone joins *as a node* — today by **adb** (the bridge
enumerates adb-visible phones, controls them with `am start` + the phone's HTTP
`StatsServer`); in the fabric, a phone-node **dials in** over ws://, announces
itself and its controller, and appears in `/v2/nodes` (phone-initiated, which
handles NAT/wifi). Browsers are nodes too (an `hci` node over a wasm sim/netsim
controller).

**Client orchestration, not node delegation.** A node does **not** forward runs to
other nodes. To use another node's controller, a **client** connects to that node
and runs there — the client holds a connection to each node it composes. For an
**MCP agent**, whose only interface is the tool surface, the MCP server itself
plays that client role: it connects *out* to the other nodes on the agent's behalf.
So the local `simble` process wears two hats — a **node** (owns and serves its own
controllers) and, when serving an agent, a **client** (reaching other nodes). The
reaching is always the *client* role: a phone is always reached by a client — which
may be the local process acting as one — never orchestrated by another node's
node-role, and its own node-role (running its scripts on its radio) stays intact.
So MCP exposes both to the agent: run on the local node's controllers (node), and
list/connect/run across other nodes (client). Today reaching a phone is adb + HTTP
with fixed Java roles; a phone speaking the v1 ws:// protocol and arbitrary-Rhai
on-device both need the scaffolded `android/rust` engine.

## 2026-08-29 — current state vs. the design

Where the code actually is, measured against the model above. The design ran
ahead of the code (it is a spec), but the foundation is real and one core path
already works.

**Built, and the closest thing to the whole idea:** MCP's `LiveBackend::{Netsim,
Usb}` — `add_peripheral(address, script)` + `pump`/`tick`. An agent can `run` a
scripted device on netsim **or** a dongle today. Also built: the individual
backends (`Link` in `controller/sim.rs`, `NetsimScene`, `UsbScene`,
`RootcanalTransport`, `LiveTransport`), the transport-agnostic stack, MCP over
stdio and ws:// (`simble mcp --ws-server`), and the phone/browser surfaces.

**Formalised (aa66b0f):** the shared scene shape is now the `transport::Scene`
trait — `name`/`add_peripheral`/`pump`/`tick`/`now`/`device_count`/
`peripheral_status_json` plus scanning (a default only real-RF overrides).
`NetsimScene` and `UsbScene` implement it, and MCP's `LiveBackend` is now
`Box<dyn Scene>` (the 2-variant enum and its ten match-arm methods are gone). A
new controller is one `impl Scene` away. This was sequencing **step 2**; it makes
the rest smaller.

**Begun (3258019):** the v1 protocol's message layer — `Request`/`Response`
(JSON-tagged), the `Controller` entity, `dispatch` — with **`list_controllers`**
implemented (enumerates the `link` + USB dongles). The `src/v1.rs` module is the
seed; `run`/`spawn`/`attach`/`route`, the other lists, and the ws:// wiring
remain.

**Not built (the architecture proper):** the `hci-router`; the rest of the v1
verbs (`run`/`attach`/`route`/…) and the `/v1/{networks,devices,nodes}` lists over
ws://; the formal node/network/device entities, `api_class` gate, and `register`;
`route` (the `0x10` live switch); the private-network create/destroy namespace;
**gRPC `PacketStreamer`** to serve the emulator (confirmed absent — `mcp.rs` says
"no tonic"); `rootcanal-rs` at runtime (`cfg`-dev-dep only); on-device Rhai
(`android/rust` is scaffolded, phones run fixed Java roles).

**Distance:** *small* — the `Controller` trait (formalise the shared shape, no new
deps; sequencing step 2, and it shrinks the rest). *Medium* — the v1 protocol as a
thin server over `LiveBackend`-style dispatch (MCP-over-ws is a frame), `route`/
`0x10`, the network namespace. *Large* — gRPC `PacketStreamer` + emulator serving
(a new dependency class), the full node/registration model, on-device Rhai. None
of it is blocked now that the emulator premise holds; it is unwritten, not gated.
