# Roadmap / follow-ups

> Living tracker, opened 2026-08-28. Items are the outstanding work after the
> phone-to-phone + L2CAP-min arc and the wasm_ws engine extraction landed on
> `main`. Status is one of: **done**, **in progress** (an agent/branch is on it),
> **blocked** (needs a decision or hardware), **planned** (specified, not
> started), **idea** (not yet specified).

## Repo health (CI on `main` is red on the first two)

| # | item | status | notes |
|---|---|---|---|
| H1 | **Android-target build break** in `src/transport/usb.rs` — `DeviceInfo` API mismatch (`bus_id()`/`device_address()`/`port_chain()`), likely an `nusb`/`rusb` version bump. Fails the "Android crate" CI check. | in progress | isolated to `usb.rs` (+ maybe `Cargo.toml`) |
| H2 | **`cargo fmt` drift** — a rustfmt-version mismatch flags files repo-wide; the fmt CI check is red. | planned | must match CI's rustfmt version; touches many files, so do it *after* H1/typed-parsing land to avoid merge churn |
| H3 | **Typed zerocopy host parsing** — pull raw-byte HCI-host work (`pkt[8]` index math, byte-order slips) into typed zerocopy views. Unblocked by the wasm_ws extraction. See the `zerocopy-packet-structs` + `lehost` notes. | in progress | start with the moved `scan_report.rs` + HCI event parsing; behavior-preserving |

## Features

| # | item | status | notes |
|---|---|---|---|
| F1 | **Publisher/collector (pub-sub)** — publisher advertises `[generation][size][PSM]`; collector scans, dedupes on generation, connects + pulls only what's new; the completed pull is the delivery ack. Reuses the L2CAP-min path. | blocked | needs the **latest-generation-only vs. held-queue** decision before building |
| F2 | **Granular link tunables** — per-parameter PHY / DLE / connection-interval controls, settable MTU target, phone-to-phone parity. Phase 0 (fast on/off) and L2CAP-min are done; the granular knobs are not. | planned | a plan exists (from the fast-link work); touches `BulkOptions` + wasm rebuild + the intent chain |
| F3 | **ISO / BIS (Auracast) over real RF** | idea | mentioned early, deferred |
| F4 | **nRF54L15 `hci_uart` + Channel Sounding** | idea | the `--serial` bridge exists; CS does not |

## Measurement

| # | item | status | notes |
|---|---|---|---|
| M1 | **Re-benchmark with `.90` on stable Android** — the 8-Pro→8-Pro pair was flaky on the beta build; clean numbers are now possible. | planned | needs the phones/adb — not a background task |
| M2 | **Harden L2CAP-min's occasional interval-miss** — one run cratered to ~12.6 KB/s (slow-default-interval); the single MTU op usually but not always settles the fast interval. | idea | speed-vs-robustness; may add a second cheap ATT op or a settle check |

## Done this arc (for context)

- Phone-to-phone bulk transfer (source role, GATT + L2CAP paths), fast-link
  toggle, MTU/PHY reporting, bridge `/pair-run`, adb phone discovery,
  reset-not-restart, L2CAP-min (one-MTU setup) — on `main`.
- `wasm_ws.rs` engine extraction (6255 → 2955 lines) — on `main`.
- Surface docs reconciled (web/android READMEs, `lib.rs` fourth surface).
