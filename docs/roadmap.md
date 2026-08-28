# Roadmap / follow-ups

> Living tracker, opened 2026-08-28. Items are the outstanding work after the
> phone-to-phone + L2CAP-min arc and the wasm_ws engine extraction landed on
> `main`. Status is one of: **done**, **in progress** (an agent/branch is on it),
> **blocked** (needs a decision or hardware), **planned** (specified, not
> started), **idea** (not yet specified).

## Repo health

| # | item | status | notes |
|---|---|---|---|
| H1 | **Android-target build break** in `src/transport/usb.rs`. | **done** (2560f4d) | Not an nusb version bump: nusb 0.2.7 cfg-gates `bus_id`/`device_address`/`port_chain`/`list_devices` off `target_os = "android"` (no USB enumeration there). Fixed by cfg-gating with android fallbacks; desktop codegen byte-identical. The "Android crate" CI check should now pass. |
| H2 | **`cargo fmt` drift** — a rustfmt-version mismatch flags files repo-wide; the fmt CI check is red. | planned | the last CI red. Check CI's rustfmt version first (it may be newer than what last formatted the repo); then one `cargo fmt` pass. Touches many files. |
| H3 | **Typed zerocopy host parsing.** | **partly done** (7a4fa3d) | HCI event parsing was *already* zerocopy in `packets/hci_events.rs`; converted the remaining raw reads in `scan_report.rs` `decode_ad_structures`. Follow-on (still hand-rolled): ext/periodic/BIG advertising reports, `packets/big.rs`, `ext_adv.rs`, btsnoop, SMP, L2CAP signaling, `classic_host.rs`. |

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
