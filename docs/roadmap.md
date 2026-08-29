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
| H2 | **CI green** — fmt drift + the lint failures it was masking. | **done** (fafdb11) | `cargo fmt -p simble` (current stable), then the clippy/doc failures each gate exposed once the prior passed: mcp.rs collapsible-if, three bridge lints, seven doc-links-to-private-items from the extraction. `main`'s CI is fully green for the first time. |
| H3 | **Typed zerocopy host parsing.** | **done** (7a4fa3d, ef2d64d, asha, 8947a5b) | A full survey settled the scope. Every clean-fit, misread-prone parser is now a typed view: `packets/{hci_events,ext_adv,big,iso,l2cap_signaling,att}`, `client/gatt_client`, btsnoop (`netsim.rs`), `scan_report.rs` AD decode, the **SMP pairing path** (`MasterIdentification`/`IdentityAddressInformation`, EDIV as `U16`), **ASHA ReadOnlyProperties** (the reserved-byte skip), and the **three classic inquiry-result records** (`InquiryResponse`/`InquiryResponseRssi` — the variable `cod_offset` is now structural, per-form tests added). Deliberately left hand-rolled, with rationale: **variable-length TLV** where a fixed struct is the wrong tool (`classic/sdp.rs` bodies, `classic/avrcp.rs` BE cursor, `profiles/ancs.rs` attribute stream, `parse_mtu_option`) and **small already-bounds-checked reads** where a struct adds ceremony not safety (`classic_host` security events, `profiles/bass.rs`, `classic/{a2dp,avdtp}.rs` 2–3-field reads, transport length-field framing in `rootcanal.rs`/`usb.rs`). |

## Features

| # | item | status | notes |
|---|---|---|---|
| F1 | **Publisher/collector (pub-sub), latest-only** — publisher advertises `[generation][size][PSM]` and serves the payload over L2CAP; collector scans, dedupes on generation (skips without connecting when not newer), else pulls; the ack is the delivery receipt. | **done** (657c3e4…1aefa2c, on `main`) | Core + all three follow-ons: `bench-pubsub.sh`, in-place generation bump over HTTP (`/publish?gen=N`, PSM kept), and a web publish/collect panel on Testing → Data (bridge `/publish` + `/collect`). Verified through the browser end to end. |
| F2 | **Granular link tunables** — per-parameter PHY / DLE / connection-interval controls, replacing the single fast on/off lever. | **done** (d18ba07, web wiring) | `BulkOptions` gained `phy_mask` / `tx_octets` / `conn_interval_min`/`max`, each independently settable or switchable off, defaults byte-matching the old fast bundle (`fast_link_commands`, unit-tested). Web bench page has a **Link tuning (advanced)** block feeding them; verified in-page (2M-only honored: "PHY update: TX LE 2M RX LE 2M", run completed). Applies to the in-page/dongle path — the phone-to-phone path keeps its own `fast` adb lever (Android exposes only PHY/priority/MTU as a central, not raw interval/DLE). MTU target and phone-path splitting left as a smaller follow-up. |
| F3 | **ISO / BIS (Auracast) over real RF** | **blocked (hardware)** | The *simulated* BIS stack already exists (`device::big_broadcaster::BigBroadcaster`, `LE_CREATE_BIG`, sim controller, `scripting/broadcast.rs`). What's left is driving a **real** BIS-capable controller to broadcast on the air — needs a dongle whose controller supports ISO/BIS plugged into the `--usb` bridge. Next action: confirm a BIS-capable dongle, then wire `BigBroadcaster` through the live HCI path and confirm a receiver hears it. |
| F4 | **nRF54L15 `hci_uart` + Channel Sounding** | **blocked (hardware)** | The `--serial` (H4-over-UART) bridge already brings up an nRF `hci_uart` build. Channel Sounding is the gap, and the sim controller does not model CS, so there is nothing to verify a blind implementation against. Next action: flash an nRF54L15 with a CS-capable Zephyr `hci_uart`, then add the CS HCI command/event handling and validate ranging against it. |

## Measurement

| # | item | status | notes |
|---|---|---|---|
| M1 | **Re-benchmark with `.90` on stable Android** — the 8-Pro→8-Pro pair was flaky on the beta build; clean numbers are now possible. | **blocked (hardware)** | Needs the physical phones + adb; nothing to code. Next action: run `bench-pubsub.sh` / the pair-run bench with `.90` on stable Android and record the clean numbers. |
| M2 | **Harden L2CAP-min's occasional interval-miss** — one run cratered to ~12.6 KB/s; the single MTU op usually but not always settles the fast interval. | **blocked (verification)** | The change is concrete — in `BulkSource`, key the stream start off the actual connection-parameter update (`onConnectionUpdated`, where available) instead of the fixed 120 ms `PHY_SETTLE_BEAT_MS` delay — but its whole value is empirical and unverifiable without phones, and a blind edit risks regressing the working l2cap-min path. Next action: write it behind a flag and A/B it on the phones against the current fixed-delay path. |

## Done this arc (for context)

- Phone-to-phone bulk transfer (source role, GATT + L2CAP paths), fast-link
  toggle, MTU/PHY reporting, bridge `/pair-run`, adb phone discovery,
  reset-not-restart, L2CAP-min (one-MTU setup) — on `main`.
- `wasm_ws.rs` engine extraction (6255 → 2955 lines) — on `main`.
- Surface docs reconciled (web/android READMEs, `lib.rs` fourth surface).
