# SimBLE — Handoff & Plan

A single place to pick up the thread: what SimBLE is, the three surfaces, the
agentic (MCP) direction, the constraints that shape every decision, and what's
next. Companion to `README.md` (user-facing) — this is the builder's map.

## The idea

SimBLE is a **pure-Rust virtual Bluetooth LE host stack** (HCI → L2CAP → ATT/GATT
→ SMP), plus a **device-simulation engine** where a device is a short **Rhai
script** against an Android-shaped API. The thesis:

> **A device is a script. Add `assert(...)` and the same script is a test.**

Because runs are **deterministic** (no radio, no timing), the AI generate-check-fix
loop actually converges, and the validated script is the artifact — the same file
an LLM writes, that you eyeball, and that CI runs, with no hand-translation. This
is the north star: *describe a Bluetooth scenario in plain language → get a
runnable, checkable, shippable test.*

Branding: crate is lowercase `simble`; prose/display is **SimBLE**; Bumble (the
Python stack that inspired the positioning) is credited once, in the README.

## Three surfaces, one engine

The *same* stack drives all three; only the frontend differs.

1. **Web (wasm)** — `web/`, compiled to WebAssembly. Interactive demos: Playground,
   Testing, Scanner, Scripted device, Color Bulb, Server+Client, Scene, Shared,
   Controllers. For authoring and exploration. Light theme, `color-scheme: light`.
2. **CLI** — `src/bin/simble.rs`. `simble FILE.rhai ...` runs device scripts as
   tests (exit 0 all-pass / 1 any-fail); `simble --no-run FILE` lints (compile
   only); `simble --usb [VID:PID]` is the USB↔WebSocket bridge; `simble mcp` is
   the MCP server. This is the CI + human path.
3. **MCP server** — `src/mcp.rs`. `simble mcp` speaks JSON-RPC over stdio so an
   **agent** builds and tests BLE devices as tool calls in a conversation. This is
   the interactive, stateful version of the AI-first loop.

The core functions are shared so the surfaces can't diverge: `run_test_script`
and `lint_script` (in `src/transport/wasm_ws.rs`) back the CLI, the web Testing
page (`run_test`), and the MCP `run_test`/`lint` tools.

## The controller ladder (transport story)

Everything above HCI is SimBLE; a **controller** supplies the Link Layer + PHY
below it. SimBLE ships one tiny built-in controller and lets you climb:

| Rung | What | Reach | Code |
|---|---|---|---|
| **self** (built-in) | `sim::Link` — in-process, no radio | native + wasm | `src/controller/sim.rs`, `SceneEngine` in `wasm_ws.rs` |
| **rootcanal-ws** | AOSP Rootcanal via `rootcanal-rs`, over WebSocket | native + browser (WS) | companion repo `schilit/rootcanal-rs` |
| **netsim** | Rootcanal + 3D position/ranging + emulator | native + browser (WS) | `NetsimTransport` (`transport/netsim.rs`) |
| **USB dongle** | `UsbTransport` (`nusb`) — real radio, real phones | native; browser via `simble --usb` bridge | `transport/usb.rs`, `transport/ws.rs` |

**Key unifier:** every rung above `self` is reached over the *same* netsim-style
WebSocket. `transport/ws.rs` holds the hand-rolled RFC 6455 codec shared by the
netsim **client** (`NetsimTransport`) and the bridge **server** (`WsServerConn`).
The `simble --usb` bridge = `WsServerConn` ↔ `UsbTransport` over one `HciChannel`;
verified end-to-end by `tests/ws_bridge_loopback.rs` (our client ↔ our server).

## The MCP agentic layer (current focus)

`simble mcp` is a **stateful** server: it holds a live scene across tool calls
(`Server` in `src/mcp.rs`). Tools shipped and tested (`tests/mcp_scenarios.rs`
+ unit tests):

- **Author/check** (stateless): `lint`, `run_test`
- **Scene**: `run_on` (controller select), `add_peripheral`, `tick`, `status`
  (god-view of hosted devices), `scan` (radio-view — what a scanner hears)
- **Behavioural**: `connect`, `read`, `assert` (a central against a peripheral;
  characteristics named by **UUID**, resolved to handles internally)
- **Temporal**: `subscribe`, `assert_over` (a real monitor — a condition that
  must hold across a window; FAILs on first violation)

Model: **a scene is the set of device scripts the agent adds; the controller is
where they run.** `run_on(target)` re-targets the controller. The agent's
devices are hosted by this process; peers (the emulator app on netsim, a
browser's devices) are things the agent's devices talk *to*, not relocate.

Register it: `claude mcp add simble -- "$PWD/target/release/simble" mcp` (loads
next session; client launches the **release** binary — rebuild after tool
changes).

### The actor loop (why it's built the way it is)

`serve_stdio` is a **single-threaded, non-blocking actor loop**. A tiny reader
thread ferries stdin *lines* over a channel (it never touches the scene); the
main loop polls without blocking. This is the seam where live backends will
pump sockets and push server→client notifications between requests.
`write_message` centralizes newline-delimited output for both responses and
future notifications.

## Constraints that shape everything

- **Rhai is non-`Send`** (this build has no `sync` feature — see
  `scripting/mod.rs`). A scripted device **cannot cross threads**. So the scene
  stays on one thread; live backends must be a **single-threaded event loop**,
  not threads. This is why the actor loop matters.
- **No async runtime.** Everything is synchronous, non-blocking `pump()` calls.
- **Near-zero dependencies.** rhai, serde/serde_json, thiserror, zerocopy; `nusb`
  (native USB, pure Rust); wasm-bindgen/web-sys/js-sys (wasm only). MCP needs
  **no** new deps (JSON-RPC over stdio = serde_json + std::io; **not** gRPC/tonic).
- **Determinism is the product.** Deterministic addresses, deterministic ticks —
  agent loops converge and CI is stable. Keep `self` deterministic (tick-on-
  command); reserve wall-clock ticking / pushed events for live backends.
- **Packets are zero-copy** (`#[repr(C)]` + the six zerocopy derives, little-endian
  `U16`, `Ref::from_prefix`). New packet code follows this.
- **Verify gate:** `cargo fmt --all --check`, `cargo clippy --all-targets
  --all-features -- -D warnings`, `cargo test --all-targets`, and `cargo doc`
  under `RUSTDOCFLAGS="-D warnings"` (broken intra-doc links fail CI + the Pages
  deploy). CI also runs the Testing-page examples through `simble`.

## Roadmap — what's next

Ordered by leverage; each independently landable.

1. **`run_on("netsim", port)` — pump-on-tick.** Give each peripheral its own
   `NetsimTransport`; `tick` pumps them (adverts out, emulator packets in). Needs
   a backend enum (`SelfScene` vs `Netsim`) threaded through the tools. Connect =
   check-and-connect (the "start netsimd" error already exists; **no auto-launch**).
   Loopback-testable; the emulator path needs a running emulator to confirm.
2. **`run_on("usb", vid:pid)`.** Single device on a real dongle (a dongle *is* one
   controller — pick which device). Reuses `UsbTransport` in-process (no bridge —
   the bridge exists only because a *browser* can't open USB).
3. **`--ws-server [PORT]`.** Host the `self` `Link` scene so browsers connect as
   devices on it (agent + browser share one scene). The multi-client
   generalization of `--usb`. Must live in the single-threaded event loop (can't
   thread the scene) — the actor loop is the foundation.
4. **Async server→client notifications.** Push "HR exceeded 200" the moment it
   happens (`notifications/message`) instead of polling `assert_over`. Pairs with
   live ticking; `write_message` is ready.
5. **Skills** to pair with the MCP: `author-ble-device` (the `android::*`/`uuid::*`
   API + "lint then run_test" loop), `write-ble-test`, `reproduce-ble-bug`,
   `test-app-against-emulator`.
6. **Smaller polish:** `scan` should report **distinct** devices (it currently
   returns every accumulated advert). A **symbol lint** in `--no-run` (flag
   `android::BluetoothGattServ` typos before running) needs Rhai's `metadata`
   feature — `gen_fn_signatures` isn't compiled in without it.

## The bigger long-range plan (separate track)

There is a full **Bumble-parity port roadmap** (Classic BR/EDR: SDP/RFCOMM/HFP/
A2DP/AVRCP/HID; the LE Audio family; a `drivers/` layer for real chips; test
parity) captured as a plan file. It's phased and dependency-ordered
(Phase 0 tooling/AGENTS.md → SMP pairing FSM → BLE profiles → Classic core →
Classic profiles → drivers). That's the "grow the stack" track; the MCP/agentic
work above is the "make it usable by agents" track — they're complementary.

## File map (where to look)

- `src/mcp.rs` — MCP server: `Server`, tools, actor loop, `request()` entry point.
- `src/bin/simble.rs` — the `simble` CLI (tests, `--no-run`, `--usb`, `mcp`).
- `src/transport/ws.rs` — shared RFC 6455 codec + `WsServerConn` (bridge server).
- `src/transport/netsim.rs` — `NetsimTransport` (WebSocket client to netsim).
- `src/transport/usb.rs` — `UsbTransport` (physical dongle).
- `src/transport/wasm_ws.rs` — `SceneEngine`, `ScriptedPeripheral`, `CentralDevice`,
  `run_test_script`, `lint_script`, and the wasm exports.
- `src/controller/sim.rs` — `Link` + `SimController` (the `self` controller).
- `src/scripting/` — the Rhai engine and `android::*` / `uuid::*` bindings.
- `web/` — the browser demos (see `web/controllers/` for the ladder writeup).
- `tests/mcp_scenarios.rs` — agent workflows on the `self` scene.
- `tests/ws_bridge_loopback.rs` — client↔server WebSocket interop.

## Working agreements

- Push over SSH: `git push git@github.com:schilit/simble.git main`.
- Commits end with `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.
- Keep the three surfaces sharing one implementation; don't fork the engine.
- Name tools/labels for **intent**, keep jargon ("controller") in the docs.
