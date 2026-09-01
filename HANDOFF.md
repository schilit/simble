# SimBLE — Handoff & Plan

A single place to pick up the thread: what SimBLE is, the three surfaces, the
agentic (MCP) direction, the constraints that shape every decision, and what's
next. Companion to `README.md` (user-facing) — this is the builder's map.

## Where things stand (2026-08-24)

A long session moved several things from "library-only" to "runs in a scene",
and closed the biggest structural gaps against Bumble. The detail lives in
`docs/gaps.md` (living) and the decision records in `docs/`
(`l2cap-handler-dispatch.md`, `run-until-semantics.md`); this is the orientation.

**Classic (BR/EDR) is real now.** `sim.rs` speaks inquiry, paging and ACL
routing, so two `ClassicHost`s meet in a scene over SDP and RFCOMM.
`SceneEngine::add_classic_device` is the fifth thing a scene can host. On top
of that: SSP with link keys, authentication and encryption; SCO/eSCO carrying
HFP audio; and A2DP, Classic HID and AVRCP as real `ProtocolHandler`s, so a
scene can host a speaker, a keyboard and a remote control. The Car page now
runs its AT conversation over a real simulated link.

**CI has foreign oracles, on every push.** This is the change that matters most
for confidence. Four scripts run against Bumble's own virtual controller with
no netsim, and `classic_peer.py` runs against a real standalone rootcanal —
two independent foreign implementations. Both jobs are blocking. The governing
rule still holds and is worth re-reading in `docs/test-strategy.md`: **a test
with simble on both ends proves only that simble agrees with itself.** Today
alone the foreign side caught an SDP continuation returning a prefix, a
reversed RSI, two unhandled inquiry-result forms, and an AVRCP fragmentation
bug that 1,287 lines of in-tree test never noticed — while *our* ASCS matrix
caught Bumble wrong three ways. Neither side is the authority; disagreement is.

**The public API has a boundary.** 7,486 reachable items to 5,636: nine
plumbing modules (`packets`, `att`, `l2cap`, `gap`, `smp`, `crypto`, `df`,
`audio`, `obex`) are gated behind a `testing` cfg that `cargo test` enables via
a self-dev-dependency. CI builds the **closed** surface as its own step —
without that, `--all-features` would mean the gate is never compiled. All 14
spec-discriminant enums are `#[non_exhaustive]`.

**The crate is publishable.** `cargo package` is 1.4 MiB compressed (it was
37.4 — a tracked `.venv` was the whole difference), and `Cargo.lock` is
tracked. Not published yet, and 1.0 is not the goal: classic has no CTKD, SCO
carries no codec, and the A2DP source has never met a foreign sink.

**`rootcanal-rs` is a submodule** at `third_party/`, itself vendoring the
rootcanal C++. It gives an in-process *real* controller for tests, behind
`--cfg rootcanal_oracle`, off by default. Every `actions/checkout` needs
`submodules: recursive` or nothing resolves — Cargo's resolver is cfg-agnostic,
so a gated dependency still has to exist.

**Tests are organised now.** 700 inline tests moved to sibling `#[path]` files
(`sim.rs` 8,483 → 5,492 lines), duplicate inline/integration pairs deleted, and
`tests/common/` shares what nine files were each redefining. One consequence
worth knowing: coverage numbers moved in **both** directions, because
assertion-heavy tests over small files charge their `panic!` arms to the file
under test.

### The failure mode to keep watching

Four separate times this session, a claim that was **true when written** had
quietly stopped being true and misdirected someone: the HID page's "SimBLE has
no central-role scripting", the Broadcast radio string, the Car multiplexer
string, and `AGENTS.md` instructing agents to recreate a file that had just
been deleted. A fifth — the controller bar silently rewriting the user's stored
backend — made a *working* capability look missing for weeks.

The structural answer is in `docs/README.md`: every document now states which
contract it is under. A **decision record** is allowed to age and is superseded,
never edited. A **living** document is worthless the moment it is stale. Where a
check can replace prose, it should: `scripts/check_hci_command_answers.py` and
`tests/explorer_surface_test.rs` both exist because a sentence could not be
trusted to stay true.

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

1. **SimBLE MCP — agent-first** — `src/mcp.rs`. `simble mcp` speaks JSON-RPC over
   stdio (or over WebSocket with `--ws-server [PORT]`) so an **agent** builds
   and tests BLE devices as tool calls in a conversation: the interactive,
   stateful version of the AI-first loop, and the surface this project is
   increasingly organized around. An agent needs no checkout and no build step —
   `example` hands it a working device script and `lookup` answers the
   assigned-number questions.
2. **Web (wasm)** — `web/`, compiled to WebAssembly. Interactive demos: Playground,
   Testing, Scanner, Scripted device, Color Bulb, Speaker, Server+Client, Scene,
   Shared, Controllers. For authoring and exploration. Light theme,
   `color-scheme: light`.
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

## The MCP agentic layer (current focus)

`simble mcp` is a **stateful** server: it holds a live scene across tool calls
(`Server` in `src/mcp.rs`). Tools shipped and tested (`tests/mcp_scenarios.rs`
+ unit tests):

- **Discover** (stateless): `example` (serve one of 16 ready-to-run device
  scripts — the "no repo needed" path), `lookup` (SIG assigned numbers by name
  fragment or UUID, from the vendored registry in `gatt/sig_names.rs`)
- **Author/check** (stateless): `lint`, `run_test`
- **Scene**: `run_on` (controller select — `self`, `netsim`, and `usb` all
  wired; `usb` takes an optional `device: "vid:pid"`), `add_peripheral`, `tick`,
  `status` (god-view of hosted devices), `scan` (radio-view — what a scanner
  hears, deduplicated per advertiser)
- **Behavioural**: `connect`, `read`, `write`, `assert` (a central against a
  peripheral; characteristics named by **UUID**, resolved to handles internally)
- **Temporal**: `subscribe`, `assert_over` (a real monitor — a condition that
  must hold across a window; FAILs on first violation). `subscribe` with `op` +
  `value` arms the *asynchronous* form: the server pushes an unsolicited
  `notifications/message` the moment the condition breaks.

Agent-facing output conventions: UUIDs are annotated with their SIG names, and
failures return `isError` so a failing test is machine-detectable.

**Two transports, one server.** `simble mcp` speaks JSON-RPC over stdio;
`simble mcp --ws-server [PORT]` (default 7682) speaks it over RFC 6455 text
frames instead, reusing `WsServerConn` from the `--usb` bridge. One client at a
time, each getting a fresh scene. `netsim` and `usb` are both **peripheral-only**
— the far side (emulator, phone) plays the central — and the central-side tools
say so rather than failing obscurely.

Model: **a scene is the set of device scripts the agent adds; the controller is
where they run.** `run_on(target)` re-targets the controller. The agent's
devices are hosted by this process; peers (the emulator app on netsim, a
browser's devices) are things the agent's devices talk *to*, not relocate.

Register it: `claude mcp add simble -- "$PWD/target/release/simble" mcp` (loads
next session; client launches the **release** binary — rebuild after tool
changes).

### The actor loop (why it's built the way it is)

`serve_lines` is a **single-threaded, non-blocking actor loop**, and
`serve_stdio` is it over stdin/stdout. A tiny reader thread ferries *lines* over
a channel (it never touches the scene, which is non-`Send`); the main loop polls
without blocking. Between requests it calls `pump_live()` — so netsim/usb
peripherals answer their centrals while no tool call is active — and flushes
`take_notifications()`. `serve_ws` is the same loop with `WsServerConn`'s
non-blocking `poll_messages`/`send_text` in place of lines.

A regression that reinstates a blocking read is silent: every request is still
answered, so the suite stays green while the server can no longer pump or speak
unprompted. `test_actor_loop_pushes_notifications_while_input_is_idle` pins it —
it asserts a queued notification reaches the sink *before anything is ever
sent*, over a reader whose `read` blocks. Before it, `serve_stdio` had coverage
count 0 (`docs/test-strategy.md` gap 7).

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

1. ~~**`run_on("netsim", port)` — pump-on-tick.**~~ **Done.** Each peripheral gets
   its own `NetsimTransport` (`LiveScene<T>` over the `HciTransport` trait); the
   actor loop pumps them between requests, so devices answer the emulator even
   while no tool call is active. Verified against a real Android emulator.
2. ~~**`run_on("usb", vid:pid)`.**~~ **Done.** `UsbScene` = `LiveScene<UsbTransport>`
   in-process (no bridge — the bridge exists only because a *browser* can't open
   USB); one device per dongle, opened at the first `add_peripheral`, selectable
   by `device: "vid:pid"`. **Never run against real hardware** — CI covers the
   argument parsing, dispatch, and error paths only. First job for whoever has a
   dongle: `run_on("usb")`, `add_peripheral(hrm)`, and find it from a phone.
3. **`--ws-server [PORT]`** — half done. The *protocol* now travels over
   WebSocket (`simble mcp --ws-server`, default 7682): the same actor loop with
   `WsServerConn` in place of stdio, one client at a time, fresh scene per
   connection. What remains is the harder half the original entry meant —
   hosting the `self` `Link` scene so browsers connect **as devices on it**
   (agent + browser share one scene), the multi-client generalization of
   `--usb`. Still must live in the single-threaded event loop (can't thread the
   scene).
4. ~~**Async server→client notifications.**~~ **Done for the monitor.**
   `subscribe(uuid, op, value)` arms a watch and the server pushes
   `notifications/message` — "HR exceeded 200" — the moment the condition breaks,
   on either transport. Sustained violations announce once; a value that swings
   back and out again re-announces. Not yet produced by anything else: a live
   backend's own events (connection, disconnect, `last_error`) are still only
   visible by polling `status`.
5. **Classic and the foreign oracles landed** — see "Where things stand"
   above. What remains there: CTKD, a SCO codec, AVRCP browsing (PSM 0x001B),
   an A2DP *source* that has met a foreign sink, and Rhai bindings for the
   Classic profiles.
6. **Skills** to pair with the MCP: `author-ble-device` (the `android::*`/`uuid::*`
   API + "lint then run_test" loop), `write-ble-test`, `reproduce-ble-bug`,
   `test-app-against-emulator`.
7. **Smaller polish:** ~~`scan` should report **distinct** devices~~ **done** (one
   entry per advertiser plus a `reports` count). A **symbol lint** in `--no-run`
   (flag `android::BluetoothGattServ` typos before running) needs Rhai's
   `metadata` feature — `gen_fn_signatures` isn't compiled in without it.

## The bigger long-range plan (separate track)

There is a full **Bumble-parity port roadmap** (Classic BR/EDR: SDP/RFCOMM/HFP/
A2DP/AVRCP/HID; the LE Audio family; a `drivers/` layer for real chips; test
parity) captured as a plan file. It's phased and dependency-ordered
(Phase 0 tooling/AGENTS.md → SMP pairing FSM → BLE profiles → Classic core →
Classic profiles → drivers). That's the "grow the stack" track; the MCP/agentic
work above is the "make it usable by agents" track — they're complementary.

## File map (where to look)

- `src/mcp.rs` — MCP server: `Server`, tools, `serve_lines`/`serve_stdio`/
  `serve_ws`, the `LiveBackend` select, monitors + notifications, `request()`.
- `src/bin/simble.rs` — the `simble` CLI (tests, `--no-run`, `--usb`, `mcp`,
  `mcp --ws-server`).
- `src/transport/ws.rs` — shared RFC 6455 codec + `WsServerConn`: a message
  layer (`poll_messages`/`send_text`, used by MCP) and `pump` (HCI, the bridge).
- `src/transport/netsim.rs` — `NetsimTransport` (WebSocket client to netsim).
- `src/transport/usb.rs` — `UsbTransport` (physical dongle) + `UsbScene`
  (`LiveScene<UsbTransport>`, one device per dongle).
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
