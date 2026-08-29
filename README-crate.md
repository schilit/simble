# simble-stack

[![CI](https://github.com/schilit/simble/actions/workflows/ci.yml/badge.svg)](https://github.com/schilit/simble/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

A pure-Rust **Bluetooth Low-Energy and Classic host stack** whose devices are
AI-coded Rhai scripts, for rapid prototyping and testing on virtual HCI
controllers and real radios.

> **Preview.** An early preview release — the public API is unstable and may
> change between preview versions. The crate is published as **`simble-stack`**
> (the bare `simble` name was taken in 2019); the library itself is imported as
> **`simble`**.

SimBLE implements the Bluetooth host stack — HCI, L2CAP, ATT/GATT, SMP pairing
(Legacy and Secure Connections), plus BR/EDR (SDP, RFCOMM) and profiles above it
(A2DP, AVRCP, HFP, HID, and the LE Audio set) — in pure, dependency-light Rust,
with no async runtime and no C. A device is a short [Rhai](https://rhai.rs)
script; a *scene* is several of them on one link. The same stack runs a scene
against a built-in **virtual controller** (in-process, no hardware) or against a
**real radio** (a USB dongle, an `hci_uart` serial controller, or Android's
netsim), so a test that passes in software can be re-run on the air unchanged.

## Use it as a library

```toml
[dependencies]
simble-stack = "0.1.0-preview.1"
```

```rust
use simble::controller::sim::Link;   // the in-process virtual controller
use simble::types::Address;
```

The supported API lives under `device`, `devices`, `scene`, `scripting`,
`types`, `transport`, `client`, `gatt`, `profiles`, `classic`, `controller`,
and `cs`. See the [`examples/`](https://github.com/schilit/simble/tree/main/examples)
directory for runnable programs — `in_process_scene` is the shortest end-to-end
one (a scanner discovers advertisers and exchanges data, no radio).

The wire-format modules (`packets`, `att`, `l2cap`, `gap`, `smp`, `crypto`, …)
are intentionally *not* part of the public API by default — they carry no
stability promise. See the crate-root documentation for the two tiers.

## Or install the tools

```bash
cargo install simble-stack
```

installs the CLI binaries:

- **`simble`** — run a Rhai device or test script, bridge a real controller
  (`--usb`, `--serial /dev/tty…`), or serve the agent interface (`simble mcp`).
- `simble-hrm`, `simble-keyboard`, `simble-gatt-dump`, `simble-cs-ranging` —
  focused single-purpose tools.

```bash
simble path/to/device.rhai         # run a scripted device / test
simble mcp                         # serve SimBLE to an AI agent over stdio
```

## The bigger picture

This crate is **all the Rust**: the library, the CLI, the `simble mcp` agent
server, and the browser-bindings source (build the `cdylib` for `wasm32`
yourself). What lives on GitHub instead is the non-Rust half — the **web**
playground and device showcase that runs this same stack in the browser, and
the **SimBLE Android** app that puts a real phone radio in a scene:

**→ [github.com/schilit/simble](https://github.com/schilit/simble)** · **[live web demos](https://schilit.github.io/simble/)**

One stack, four front-ends — MCP, Web, Native, Android; the first three share
this crate's engine, so they cannot diverge.

## License

Apache-2.0.
