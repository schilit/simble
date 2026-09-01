# Controller routing

The v1 control protocol, and the model of controllers, networks, and devices it
routes.

**Status:**

- **Built — the device router** (the v1 `Node`): the four lists plus `run`,
  `stop`, `send`, `route`, `create`, `register`, `tick`, and `get_clock`. Runnable
  on the deterministic `link` controller with no hardware, and served by
  `simble v1`.
- **Not built — the async backend router**: routing raw HCI to
  `rootcanal-rs` / real radios / netsim, and the `attach` verb. A separate crate,
  designed at the end of this doc.

## What the environment can do

simble is a pure-Rust Bluetooth LE and Classic host stack whose devices are Rhai
scripts. One environment routes those devices onto controllers:

- **Run scripted devices deterministically, in-process, no hardware** — the
  `link` ether, `tick()`-driven and reproducible.
- **Put the same script on real radio with one field change** — target a USB
  dongle instead of `link`; the device is now physically discoverable.
- **Host many isolated private networks** — parallel tenants, each in its own
  world, no cross-talk.
- **Drive devices on real phones** — Android and iOS, each through its platform
  API, as first-class nodes.
- **Switch a running device's controller live** — sim↔real — without migrating
  state; a reset event re-homes the host.
- **Speak one protocol, SimBLE v1**, for all of it, from the CLI, an MCP agent,
  or a browser. Control is HTTP REST; the one streaming case (`attach`, raw HCI)
  is ws://. simble can separately *serve* the netsim protocol for compatibility.

## The model: four types

| type | is | created or registered | key fields |
|---|---|---|---|
| **node** | a participant that owns controllers and executes runs | registered (external, already exists) | `name`, `kind`, its controllers |
| **controller** | what a device's host attaches to (below HCI, or a phone's platform API) | comes with its node | `name`, `kind`, `api_class` (`hci`/`android`/`coreBluetooth`), `network`, `real`/`deterministic`/`attachable` |
| **network** | a world / ether — who-hears-whom | sim ethers created; `real` is a fixed singleton | `name`, `kind` (`link`/`rootcanal`/`rf`/`netsim`), `deterministic`/`real`/`shared`/`leaf` |
| **device** | a running scripted instance | started by `run` | handle, `controller` (its route), `address` |

- **A node is what can execute a `run`.** The local `simble` process and a phone
  are both nodes. MCP, the CLI, and the v1 socket are *interfaces to* a node, not
  nodes — one process is one node reached many ways, which keeps a single owner of
  its hardware.
- **A node owns controllers; a `run` executes on a controller it owns.**
  Cross-node orchestration is the client's job — a node does not reach into
  another node.
- **A controller has one `network`** (the world it drops a device into) and one
  **`api_class`** (which gates what a `run` can do: only `hci` allows attach and
  low-level control; `android`/`coreBluetooth` are high-level GATT/advertise).
- **A network holds many controllers/devices that hear each other.** A device is
  in exactly one network. Isolation is a simulated-only guarantee; `real` is the
  one shared physical world.
- **A device is owned by the connection that started it**; `stop`/close releases
  its controller.

## Operations

Four lists: `/v1/controllers`, `/v1/networks`, `/v1/devices`, `/v1/nodes`.

| op | does | notes |
|---|---|---|
| `run {controller, script, address?}` | run a script on a controller → a device handle | persistent until `stop`; default controller `link`; there is no separate `spawn` |
| `stop {device}` / close | teardown | releases the controller; the handle stays valid but inert |
| `send {device, event, data?}` | deliver an input event → `fn on_event` | needs the script to expose the input |
| `route {device, controller}` | rebind a device's controller | drops and re-runs (never migrates); may change the world; keeps the handle and address |
| `tick {advance_us}` | advance simulation time | sim-only; returns the next deadline (absolute µs) |
| `get_clock` | read the clock now + next deadline | wait until the deadline instead of spinning |
| `create {network}` | mint a private sim ether | an internal resource |
| `register {node}` | admit an external node | bookkeeping; this node does not drive it |
| `attach ?controller=` | a raw H4 HCI stream (ws://) | for external stacks (the emulator); **not built** — belongs to the backend crate |

**Wire encoding:** control is HTTP REST (an op per endpoint, JSON in/out); `attach`
is the one ws:// stream (H4 both ways). `dispatch` is transport-neutral, so one
handler serves either.

**No gRPC in the core.** gRPC arises in exactly one place: the Android emulator
speaks netsim's `PacketStreamer` gRPC to its controller (Google's protocol). So
backing the emulator with a chosen controller would mean speaking gRPC — that edge
is dropped from the core; if ever wanted it is an isolated, feature-gated adapter.

**Bring your own server.** v1 bundles no http/ws server — a host that runs it
already has one. It exposes the entry points a host's server calls: `dispatch`
(typed), `handle_json` (JSON in/out, for ws://), `handle_http` (method+path+body →
status+body, for REST). `simble v1 [PORT]` is one such server, in the CLI.

**Safety:** every `run` is Rhai, sandboxed (no I/O, deterministic) — which is what
makes accepting a device definition over the wire safe where native code would not.

## Routing chooses a world; it is not a bridge

Routing a device's HCI to a dongle **moves that host onto a different
controller** — it does not join two ethers. The instant a device is
dongle-backed, its link layer transmits over real RF: it can hear real peers and
can no longer hear simulated ones. It has left the simulated medium, not bridged
to it. So `route` chooses which world a device lives in, and a switch drops the
device rather than migrating it (see `joining-controller-worlds.md`: HCI-layer
facilities move a host, they do not bridge).

This is why routing cannot live inside netsim: netsimd is not ours, the emulator's
backend is netsim's choice, and we cannot route what we do not sit in. simble owns
its own controllers and routes among them.

## What's built: the device router

The v1 `Node` (`src/v1.rs`) owns named controllers and routes scripted devices
between them over the synchronous `transport::Scene` trait (`link` + `usb`, no
async). A device has one stable global handle that survives `stop` and `route`;
the node maps it to a controller and that controller's local index. All
controllers share the node's clock, so one `tick` advances the whole node and the
minimum deadline covers it.

- **Clock:** integer-microsecond, absolute-deadline (the serializable,
  deterministic analog of `std::time::Instant`). `tick(advance_us)` returns the
  next event's absolute clock; a script declares its next wake with the `wake_at`
  binding, and the scene folds the earliest across devices.
- **`stop`** tombstones a device: the slot stays (handles never shift or reuse),
  and its address is released from the medium.
- **`route`** drop-and-re-runs on the target controller, keeping handle and
  address — the deterministic analog of a controller switch injecting an HCI
  Hardware Error (`0x10`) so the host re-initialises.
- **`create`** mints a private `link` ether; **`register`** records an external
  node in `list_nodes`.

### The controller factory (the injection seam)

A `Node` does not hard-code its controllers; it holds a `ControllerFactory`:

```rust
pub trait ControllerFactory {
    fn create(&self, name: &str) -> Result<Box<dyn Scene>, String>;
    fn available(&self) -> Vec<Controller> { Vec::new() }
}
```

`BuiltinControllers` is the default (`link`/`usb`/`netsim`); `CompositeFactory`
tries several in order. An app adds a backend by composing:

```rust
Node::with_factory(Box::new(CompositeFactory::new(vec![
    rootcanal_factory,             // from the backend crate
    Box::new(BuiltinControllers),  // link / usb / netsim
])))
```

So a backend adds controllers without `simble-stack` depending on it — the crate
depends only on this trait.

## Not built: the backend router (a separate crate)

Routing *raw HCI* to `rootcanal-rs`, real radios, and external netsim — and
backing external stacks — is the async half, and by design its own crate:

- **The backend crate depends on `simble-stack`, never the reverse.** This keeps
  `simble-stack` async-free and publishable — `rootcanal-rs` is a path dependency,
  which blocks `cargo package` on whatever crate holds it.
- **The `rootcanal-rs` backend is an actor** that owns the `rootcanal-rs`
  scheduler loop (one loop, one owner) in its own task and hands off a
  `Sink<HciPacket>` (host→controller) / `Stream<HciPacket>` (controller→host)
  pair. Its client trait can be owned by `rootcanal-rs` upstream. The actor
  wrapper is defined *outside* simble; the app injects it (dependency injection),
  so which actor framework it uses is the app's choice.
- **simble consumes it as a factory/network** through the `ControllerFactory`
  seam above. The bridge is the existing `HciTransport` trait: a thin
  `impl HciTransport` drains and fills the actor's Sink/Stream, and
  `LiveScene<T: HciTransport>` runs scripted devices on it — zero async in
  `simble-stack`, which only keeps `HciTransport`, the HCI packet types, and the
  H4 codec public.
- **Backends are Cargo features** (default `link` + `usb`; opt-in `rootcanal`,
  `netsim`). Switching one injects a Hardware Error (`0x10`) upward, the same
  drop-and-re-run `route` already does. The `attach` verb (raw H4 HCI for an
  external stack) lives here.

The existing transports already converge on the shape the backend crate
formalises: `NetsimScene` (WebSocket client to netsimd), `UsbScene` (a dongle
pool), `RootcanalTransport` (H4 over a stream to a rootcanal server), and
`LiveScene<T: HciTransport>` (the stack is transport-agnostic). `rootcanal-rs`
itself is present only as a `cfg`-gated dev-dependency today — the capability is
real; the runtime wiring is what the crate adds.

The Android-emulator premise this rests on is verified: Bluetooth comes up `ON`
over a netsim controller in the emulator (Pixel 7, API 34).
