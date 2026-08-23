# Gaps and missing features

*Compiled 2026-08-23. Every entry below was verified against the tree on that
date, not copied from an older list.*

The point of this file is that **most of these are already declared in the code
or the UI** — as a `SUPPORTS` reason string, a "Not implemented:" doc comment,
or a doc-only entry in the API Explorer. Those declarations are the source of
truth; this is an index over them, plus the things that are only gaps in
someone's head.

A gap that is *stated where a user will see it* is not a bug. A gap that is
silently absent is.

---

## How to re-derive this list

Run these rather than trusting the file:

```bash
# Capabilities switched off in the UI, with their stated reason
grep -rhoE '"(websocket|in-page)": "[^"]+"' web/*/[a-z]*.js

# Code that admits what it does not do
grep -rn "Not implemented:" src/ --include=*.rs
grep -rniE "//[/!]? .*(does not model|is a fake|stand-in|not modelled)" src/ --include=*.rs

# API surface documented but not executable
grep -c 'mode: "doc"' web/explorer/explorer.js    # 24
grep -c 'mode: "ref"' web/explorer/explorer.js    # 21
```

---

## 1. Capabilities switched off in the UI

Each is a `SUPPORTS` string, shown to the user in the controller bar with the
reason. Four remain.

| Domain | Off on | Stated reason | Real? |
|---|---|---|---|
| Broadcast | in-page | "the in-page controller models periodic advertising and a BIG, but nothing is bound to it" | True, and the reason was **rewritten** — the old string blamed the radio, which had modelled PA and a BIG since `d78b0fb`. The real blocker is one layer up: `WebLink` has no broadcast device kind and `WebBigBroadcaster`/`WebBigReceiver` demand a netsim URL. Needs a wasm export. |
| HID | netsim | "the HID host is not wired for it yet" | True. Needs a `WebHidHost` wasm export — `HidHost` is only reachable inside `WebLink` (`wasm_ws.rs:1312`). |
| Car | netsim | "both hosts run on this page's own simulated BR/EDR controller — no transport carries a `ClassicHost` to netsim" | True, and **newly true for a different reason**. The old string said the multiplexers were wired directly together with no ACL; they now run over a real simulated BR/EDR link. What is missing is a transport that carries a `ClassicHost` out to netsim. |
| Ranging | netsim | "the tag and locator need a radio that models distance; here the radio is netsim's own, and positions come from `netsim move`" | True and correct — see `controller/propagation.rs` on who owns RSSI. |

All four reasons are now accurate. Two were rewritten on 2026-08-23 after
the thing they blamed stopped being the blocker — the recurring failure mode
here is a reason that was true when written and quietly stopped being true.

## 2. Protocol behaviour the code admits it does not do

| Where | Gap |
|---|---|
| `profiles/bass.rs` | **Biggest one.** Add Source reports "synchronized" unconditionally — for a source that does not exist, or an encrypted one with no code. `BigReceiver` does the real thing next door and is Bumble-verified. This is the largest remaining "our code lies to a foreign peer" surface. ~a day; needs `bass.rs` to hold a transport handle, which is a design decision. |
| ~~`profiles/ascs.rs`~~ | ~~`Releasing` never assigned; `Released` unimplemented; neither §3.2 link-loss rule.~~ **Closed.** A 104-cell {role,state}×{opcode} matrix found 9 cells with the right response code and the wrong resulting state, plus 16 unconstructible. **Still open:** the link-loss entry points have no caller — every `add_ascs` site drops the service handle, so nothing can deliver the event. |
| ~~`smp/pairing.rs`~~ | ~~No SMP timer; no `self.failed` guard.~~ **Closed.** §3.4's 30 seconds, the post-failure drop, and per-opcode `INVALID_PARAMETERS` — 16 guards, each mutation-proven, none previously covered by any test. Found on the way: a remote panic from one well-formed Pairing Random. **Still open:** `tick_smp` has no caller in the scene loop, and an inbound re-pair after a plain Pairing Failed is still dropped responder-side. |
| `profiles/ras.rs` | On-Demand Ranging Data and its Get/Ack/Retrieve flow; Ranging Data Ready / Overwritten notifications; mode-0 and mode-1 steps; multiple antenna paths. `antenna_paths_mask` is hardcoded `0x01` and carries no information. `CONFIG_ID_SHIFT` masks `& 0x07` where the spec field is **4 bits** — config IDs 8–15 truncate on write and mis-read on parse. |
| `controller/sim.rs` | ~~Its catch-all answers every unhandled opcode with Command Complete~~ **Closed.** The real count is **61 Command-Status-only commands of 339** in Core 6.3, not 57 of 319 — the earlier estimate missed the `[v1]`/`[v2]` opcode pairs. `COMMAND_STATUS_OPCODES` now carries the derived table, the catch-all answers anything in it with a Command Status (`UNKNOWN_HCI_COMMAND`) instead of a Command Complete, and `scripts/check_hci_command_answers.py` fails CI if the table drifts from the specification. 19 have real arms; the other 42 are answered with the right *shape* and no modelled behaviour. |
| `controller/sim.rs` | Of the 42 Command-Status commands answered `UNKNOWN_HCI_COMMAND`, the ones worth modelling next, in order: **LE Extended Create Connection** \[v1] 0x2043 / \[v2] 0x2085 (any modern host uses it in place of LE Create Connection, and its completion event is LE *Enhanced* Connection Complete, which nothing here emits yet), **LE Read Remote Features Page 0** 0x2016, **LE Enable Encryption** 0x2019 (no link encryption is modelled at all), and **LE Request Peer SCA** 0x206D. |
| `controller/sim.rs` | What the new arms do *not* model, by their own comments: LE Connection Update applies immediately with no `LL_CONNECTION_UPDATE_IND` instant and can never be rejected by the peer; LE Set PHY skips `LL_PHY_REQ`/`LL_PHY_RSP` and PHY Options entirely; LE Create CIS / LE Accept CIS Request model the *handshake* only — no CIG, no ISO interval, no flush timeout, and ISO SDUs still ride the ACL handle. |
| `controller/sim.rs` | ISO data paths not modelled — an SDU is delivered whether or not a path was set up. BIG modelling is **sequencing only**: no PHY, no scheduling, no encryption (the broadcast code is compared, never used), no timeouts. |
| `classic/avrcp.rs` | `RequestContinuingResponse` not modelled on the send path. |
| `cs/ranging.rs` | No multipath model — the radio has none, so a computed distance is line-of-sight by construction. |

## 3. Things that exist but cannot be reached

| Where | Gap |
|---|---|
| `types/hci_types.rs` | `GapDataType` — a **third** AD-type table, with its own `Display` impl naming fifteen types — has **zero references anywhere in the tree**. It duplicates `gap::advertising::ad_type`, and being unused it was already missing 0x2E and 0x30 while the live table had them. Delete it, or make it the one table. |
| ~~Scripting~~ | ~~Rhai has **no way to load a catalog device**.~~ **Closed.** `catalog::device("hrm")` runs the entry in the caller's own engine (`src/scripting/catalog.rs`), so what comes back is the `ScriptGattServer` a scene already collects — the load *is* the peripheral being added. `catalog::names()`/`catalog::source()` alongside it. |
| ~~Rhai test surface~~ | ~~`assert_over` is **MCP-only**; `wait_for` appears in zero shipped examples.~~ **Closed.** `src/scripting/monitor.rs` puts the MCP monitor's semantics (0.1 s samples, `< > <= >= == !=`, byte index, the extreme) on the script surface; `catalog/tests/monitor.{pass,fail}.rhai` and the `checked_thermostat` catalog entry use both primitives. **Still open:** `mcp.rs` keeps private copies of `compare`/`extreme_for` — `scripting::monitor` is the declared owner, but the delegation needs an edit to `src/mcp.rs`. |

## 4. Declared to users as unavailable

The API Explorer marks **24 members doc-only** (callbacks, `wait_for`, constant
tables — no form, because they cannot be driven by one line of Rhai) and **21
reference-only** (the whole central role: real Rhai, but `WebSession` pumps
only `ScriptGattServer`s, so a `BluetoothGatt` built there would queue HCI
packets nobody drains). Both are honest and correctly labelled. Listed so
nobody "fixes" them by making the labels disappear.

## 5. Roadmap items in `HANDOFF.md` that do not exist

| Promised | Status |
|---|---|
| `run_on("usb", vid:pid)` | **Landed.** `UsbScene` (`transport/usb.rs`) is `LiveScene<UsbTransport>`, one device per dongle; `run_on("usb", device: "vid:pid")` selects it and defers opening to `add_peripheral`, as netsim defers its connection. **The live path is untested** — CI exercises argument parsing, dispatch, the peripheral-only guard, and the no-dongle error, never a real dongle advertising to a real phone. |
| `--ws-server [PORT]` | **Landed**, but not as roadmap item 3 described it. `simble mcp --ws-server [PORT]` (default 7682) serves *the MCP protocol* over RFC 6455 text frames — the actor loop with `WsServerConn` in place of stdin/stdout, one client at a time, a fresh scene per connection. Hosting the `self` `Link` scene so browsers join it **as devices** is still absent, and is the harder half. |
| Async server→client notifications | **Landed.** `subscribe` with `op` + `value` arms a watch; when the condition breaks, the server queues an MCP `notifications/message` (level/logger/data, no `id`) that both loops flush between requests. `initialize` now advertises the `logging` capability. Only the temporal monitor produces them; a live backend's own events (a connection, a disconnect) still do not. |
| Skills (`author-ble-device`, `write-ble-test`, …) | None exist; no skills directory. |
| A symbol lint in `--no-run` | Needs Rhai's `metadata` feature; `Cargo.toml` still has `features = ["serde"]` only. |

## 6. Infrastructure

- **`Cargo.lock` is gitignored** while the crate ships a binary — CI resolves
  different dependency versions than local.
- **`web/emulator/` has no slot in the top nav** (it has a landing-page card).
- **24 test functions are duplicated** inline ↔ `tests/`, and in the three real
  drifts the inline copy is the weaker one. See `docs/test-strategy.md`.
- **`wasm_ws.rs` is 5 044 lines** and holds AD decoding, scan parsing, Rhai web
  extensions, `ScriptedPeripheral`, `CentralDevice`, `SceneEngine`,
  `run_test_script`, `lint_script` and **41 wasm_bindgen exports**. The
  `LeHost` extraction it was waiting for has landed (`device/host.rs`); the
  remaining split is the ~2 200 lines of bindings and finding `SceneEngine` a
  home outside `transport/`.

## 7. Structural gaps against Bumble

*Added 2026-08-23, after BR/EDR landed in the simulated controller
(`9557778`). These are not correctness bugs in code we have — they are whole
capabilities Bumble has and simble does not. Each was confirmed absent on that
date, not inherited from an older list. Recommended order: security first,
because a real peer refuses unauthenticated profile connections, so the other
two cannot be honestly demonstrated without it.*

| Gap | What exists today | What Bumble has | What closing it needs |
|---|---|---|---|
| **Security, both transports** | LE SMP does the pairing math (`smp/pairing.rs`), but the controller does not model encryption start. Classic has *nothing*: no SSP, no link keys, no authentication, no encryption — `Write Simple Pairing Mode` falls through `sim.rs`'s catch-all. | Classic SSP, link keys, authentication + encryption, LE encryption start, CTKD (`bumble/pairing.py`, `smp.py`, `controller.py`). | SSP in `sim.rs` (IO-capability exchange, numeric comparison at minimum), link-key store on `ClassicHost`, Authentication Requested / Set Connection Encryption + their event chains, LE Enable Encryption actually enabling. Bumble as the foreign peer to prove it. |
| **SCO/eSCO** | Nothing: no H4 packet type 0x03 anywhere, no Setup Synchronous Connection, no routing in `Link`. HFP is signalling-only — the Car page can place a call but no audio path exists. | SCO/eSCO with HFP audio end-to-end. | A third H4 packet type through `HciChannel`, `Link` routing, Setup Synchronous Connection (+ its Command-Status chain) in `sim.rs`, and a CVSD/mSBC seam to the existing codec code. |
| **Classic profiles as connectable devices** | ~10 000 lines of protocol code (`classic/{a2dp,avrcp,avdtp,hfp,hid}.rs`) with tests, but none is a `ProtocolHandler` on `ClassicHost` — no scene can host a classic headset, keyboard, or speaker. | Runnable A2DP speakers, HID keyboards/hosts, OPP servers, HFP AG/HF as applications. | One `ProtocolHandler` + SDP record per profile. **Known design item first:** `handle_channel_data` maps one PSM to one handler, but Classic HID needs two channels (0x0011 control, 0x0013 interrupt) distinguished. A2DP first — it unlocks the speaker scenario and can reuse the SBC oracle. |

Parity with Bumble is *not* the goal — Bumble has no scripting, MCP, or web
surface, which is where simble's value lives. Bumble's role stays what it has
been all along: the foreign peer that proves each of these once built.

---

## Ranked, if picking up one thing

1. **`bass.rs`** — the only item here where our code actively misleads a
   foreign peer.
2. **The 17 Command-Status commands** in `sim.rs` — same bug that hit four
   times this week, and `docs/sig-as-oracle.md` describes the ~150-line lint
   that finds them all permanently.
3. **Broadcast in-page** — likely already possible; verify and delete the
   reason string.
4. **RAS `CONFIG_ID_SHIFT`** — a one-hour correctness fix with a real
   truncation bug behind it.

*CSIP RSI (0x2E) was third on this list and is done: the identifier reaches the
air, the scanner decodes it, and `earbud` + `earbud_right` are a real
coordinated set. Reaching the air is what found the bug — the six octets were
being emitted `prand || hash` where CSIS Section 4.9 says `hash || prand`, which
no round trip against our own decoder could have caught. That is the second
time the second consumer found the first one's bug; it is worth expecting.*
