# SimBLE — builder's orientation

What SimBLE is, its three surfaces, the agentic (MCP) direction, and the
constraints that shape every decision. Companion to `README.md` (user-facing) —
this is the builder's map. Current and planned work is tracked in
`docs/roadmap.md`.

## The idea

SimBLE is a **pure-Rust virtual Bluetooth LE host stack** (HCI → L2CAP → ATT/GATT
→ SMP), plus a **device-simulation engine** where a device is a short **Rhai
script** against an Android-shaped API. The thesis:

> **A device is a script. Add `assert(...)` and the same script is a test.**

Because runs are **deterministic** (no radio, no timing), the AI generate-check-fix
loop actually converges, and the validated script is the artifact — the same file
an LLM writes, that you eyeball, and that CI runs, with no hand-translation. The
north star: *describe a Bluetooth scenario in plain language → get a runnable,
checkable, shippable test.*

Branding: crate is lowercase `simble`; prose/display is **SimBLE**; Bumble (the
Python stack that inspired the positioning) is credited once, in the README.

## Three surfaces, one engine

The *same* stack drives all three; only the frontend differs.

1. **SimBLE MCP — agent-first** — `src/mcp.rs`. `simble mcp` speaks JSON-RPC over
   stdio (or over WebSocket with `--ws-server [PORT]`) so an **agent** builds
   and tests BLE devices as tool calls in a conversation: the interactive,
   stateful version of the AI-first loop. An agent needs no checkout and no build
   step — `example` hands it a working device script and `lookup` answers the
   assigned-number questions.
2. **Web (wasm)** — `web/`, compiled to WebAssembly. Interactive demos: Playground,
   Testing, Scanner, Scripted device, Color Bulb, Speaker, Server+Client, Scene,
   Shared, Controllers. For authoring and exploration.
3. **Native (CLI + library)** — `src/bin/simble.rs`. `simble FILE.rhai ...` runs
   device scripts as tests (exit 0 all-pass / 1 any-fail); `simble --no-run FILE`
   lints (compile only); `simble --usb [VID:PID]` is the USB↔WebSocket bridge;
   `simble mcp` is the MCP server. This is the CI + human path.

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

## The MCP agentic layer

`simble mcp` is a **stateful** server: it holds a live scene across tool calls
(`Server` in `src/mcp.rs`). The tools (`tests/mcp_scenarios.rs` + unit tests):

- **Discover** (stateless): `example` (serve one of the ready-to-run device
  scripts — the "no repo needed" path), `lookup` (SIG assigned numbers by name
  fragment or UUID, from the vendored registry in `gatt/sig_names.rs`).
- **Author/check** (stateless): `lint`, `run_test`.
- **Scene**: `run_on` (controller select — `self`, `netsim`, `usb`),
  `add_peripheral`, `tick`, `status` (god-view of hosted devices), `scan`
  (radio-view — what a scanner hears, deduplicated per advertiser).
- **Behavioural**: `connect`, `read`, `write`, `assert` (a central against a
  peripheral; characteristics named by **UUID**, resolved to handles internally).
- **Temporal**: `subscribe`, `assert_over` (a real monitor — a condition that must
  hold across a window). `subscribe` with `op` + `value` arms the *asynchronous*
  form: the server pushes an unsolicited `notifications/message` the moment the
  condition breaks.

Agent-facing output conventions: UUIDs are annotated with their SIG names, and
failures return `isError` so a failing test is machine-detectable.

**Two transports, one server.** `simble mcp` speaks JSON-RPC over stdio;
`simble mcp --ws-server [PORT]` (default 7682) speaks it over RFC 6455 text frames,
reusing `WsServerConn` from the `--usb` bridge. One client at a time, each getting
a fresh scene. `netsim` and `usb` are both **peripheral-only** — the far side
(emulator, phone) plays the central.

Model: **a scene is the set of device scripts the agent adds; the controller is
where they run.** `run_on(target)` re-targets the controller. The agent's devices
are hosted by this process; peers are things they talk *to*, not relocate.

Register it: `claude mcp add simble -- "$PWD/target/release/simble" mcp` (client
launches the **release** binary — rebuild after tool changes).

### The actor loop (why it's built the way it is)

`serve_lines` is a **single-threaded, non-blocking actor loop**, and `serve_stdio`
is it over stdin/stdout. A tiny reader thread ferries *lines* over a channel (it
never touches the scene, which is non-`Send`); the main loop polls without
blocking. Between requests it calls `pump_live()` — so netsim/usb peripherals
answer their centrals while no tool call is active — and flushes
`take_notifications()`. `serve_ws` is the same loop with `WsServerConn`'s
non-blocking `poll_messages`/`send_text` in place of lines.

A regression that reinstates a blocking read is silent: every request is still
answered, so the suite stays green while the server can no longer pump or speak
unprompted. `test_actor_loop_pushes_notifications_while_input_is_idle` pins it — it
asserts a queued notification reaches the sink *before anything is ever sent*, over
a reader whose `read` blocks.

## Constraints that shape everything

- **Rhai is non-`Send`** (no `sync` feature — see `scripting/mod.rs`). A scripted
  device **cannot cross threads**, so the scene stays on one thread and live
  backends must be a **single-threaded event loop**, not threads. This is why the
  actor loop matters.
- **No async runtime.** Everything is synchronous, non-blocking `pump()` calls.
- **Near-zero dependencies.** rhai, serde/serde_json, thiserror, zerocopy; `nusb`
  (native USB, pure Rust); wasm-bindgen/web-sys/js-sys (wasm only). MCP needs **no**
  new deps (JSON-RPC over stdio = serde_json + std::io; **not** gRPC/tonic).
- **Determinism is the product.** Deterministic addresses and ticks — agent loops
  converge and CI is stable. Keep `self` deterministic (tick-on-command); reserve
  wall-clock ticking / pushed events for live backends.
- **Packets are zero-copy** (`#[repr(C)]` + the zerocopy derives, little-endian
  `U16`, `Ref::from_prefix`). New packet code follows this.
- **Verify gate:** `cargo fmt --all --check`, `cargo clippy --all-targets
  --all-features -- -D warnings`, `cargo test --all-targets`, and `cargo doc` under
  `RUSTDOCFLAGS="-D warnings"` (broken intra-doc links fail CI + the Pages deploy).
  CI also runs the Testing-page examples through `simble`, and against Bumble's own
  virtual controller and a standalone rootcanal — two independent foreign oracles.

## File map (where to look)

- `src/mcp.rs` — MCP server: `Server`, tools, `serve_lines`/`serve_stdio`/
  `serve_ws`, the live-backend select, monitors + notifications.
- `src/bin/simble.rs` — the `simble` CLI (tests, `--no-run`, `--usb`, `mcp`,
  `mcp --ws-server`, `v1`).
- `src/transport/ws.rs` — shared RFC 6455 codec + `WsServerConn`.
- `src/transport/netsim.rs` — `NetsimTransport` (WebSocket client to netsim).
- `src/transport/usb.rs` — `UsbTransport` (physical dongle) + `UsbScene`.
- `src/transport/wasm_ws.rs` — the browser bindings; the engines it once held now
  live in `scan_report`, `device`, `scene`, and `scripting`.
- `src/controller/sim.rs` — `Link` + `SimController` (the `self` controller).
- `src/scripting/` — the Rhai engine and `android::*` / `uuid::*` bindings.
- `web/` — the browser demos (see `web/controllers/` for the ladder writeup).

## Working agreements

- Push over SSH: `git push git@github.com:schilit/simble.git main`.
- Commits end with `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`.
- Keep the three surfaces sharing one implementation; don't fork the engine.
- Name tools/labels for **intent**, keep jargon ("controller") in the docs.
