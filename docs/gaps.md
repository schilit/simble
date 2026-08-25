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
grep -c 'mode: "doc"' web/explorer/explorer.js    # 52
grep -c 'mode: "ref"' web/explorer/explorer.js    # 57
```

---

## 1. Capabilities switched off in the UI

Each is a `SUPPORTS` string, shown to the user in the controller bar with the
reason. Four remain.

| Domain | Off on | Stated reason | Real? |
|---|---|---|---|
| Broadcast | in-page | "the in-page controller models periodic advertising and a BIG, but nothing is bound to it" | True, and the reason was **rewritten** — the old string blamed the radio, which had modelled PA and a BIG since `d78b0fb`. The real blocker is one layer up: `WebLink` has no broadcast device kind and `WebBigBroadcaster`/`WebBigReceiver` demand a netsim URL. Needs a wasm export. |
| ~~HID~~ | ~~netsim~~ | ~~"the HID host is not wired for it yet"~~ | **Closed 2026-08-24.** The entry above was itself an instance of the failure mode this file exists to catch: it was written when the hosts were `WebLink::add_central` + `central_start_hid`, and stopped being true when `android::BluetoothHidHost` landed and both hosts became ordinary **scripted centrals** — which `WebScriptedCentral` already hosts on netsim, as Generic does. No `WebHidHost` was needed. The one genuinely missing piece was `WebPeripheral::notify_value`: a HID report describes *change*, so two identical reports are two events, and `set_value`'s value-diff swallowed the second. |
| Car | netsim | "the phone and head unit are one `CarKit`: one SceneEngine, and an RFCOMM port pair they talk through in memory rather than over the link — netsim needs both a ClassicHost on a socket and those two ends split apart" | True, and the string was **sharpened 2026-08-24**. The previous one named only the missing transport, which invited the (wrong) assumption that this is HID-shaped wiring. It is not: `CarKit` holds one `SceneEngine` with *both* endpoints plus `ag_port`/`hf_port` `SharedRfcommPort` handles, so the AT conversation crosses shared Rust memory, not the link. Splitting them is a `car_kit.rs` rewrite. Unverified: whether rootcanal's WebSocket frontend routes inquiry/paging between two clients at all. |
| Ranging | netsim | "the tag's position is dragged on this page's floor plan and turned into RSSI by `controller/propagation.rs` — a model that belongs to the built-in radio alone…" | True, re-confirmed against `controller/propagation.rs` on 2026-08-24 rather than trusting the old string. Its module docs state the rule directly: applying that model to netsim's reports would attenuate twice. **Closing this would be a mistake, not an improvement.** |

Three reasons remain, and the fourth (HID) was closed by checking whether its
premise still held rather than by believing it. That is the recurring failure
mode here: a reason that was true when written and quietly stopped being true.
**A `SUPPORTS` string is a claim with an expiry date — re-derive it, do not
inherit it.**

### The controller bar clobbered the stored choice (fixed 2026-08-24)

Worth recording separately, because it made a *working* capability look
missing and no `SUPPORTS` string was wrong. `createControllerBar`'s `render()`
corrected a selection the current domain could not honour — and **persisted**
the correction. Since HID, Car and Ranging are in-page only and sit one click
away in the same tab strip, merely visiting any of them rewrote
`localStorage["simble-backend"]` to `in-page`. Generic, which supports netsim
fully, then came up in-browser with nothing on screen saying the choice had
been discarded. The fallback is now per-render and per-domain; only a click
writes the preference down.

## 2. Protocol behaviour the code admits it does not do

| Where | Gap |
|---|---|
| ~~`profiles/bass.rs`~~ | ~~Add Source reports "synchronized" unconditionally.~~ **Closed.** `sync_states()` now reports what is actually true — `SyncInfoRequest` when the Assistant offers PAST, otherwise `NotSynchronizedToPa`, with no BIS synchronised either way — and `report_sync_outcome` is the entry point for a real outcome. **Still open:** nothing calls it; that needs `bass.rs` to hold a transport handle, which is a design decision, so a Delegator still cannot report a *real* sync. |
| ~~`profiles/ascs.rs`~~ | ~~`Releasing` never assigned; `Released` unimplemented; neither §3.2 link-loss rule.~~ **Closed.** A 104-cell {role,state}×{opcode} matrix found 9 cells with the right response code and the wrong resulting state, plus 16 unconstructible. **Still open:** the link-loss entry points have no caller — every `add_ascs` site drops the service handle, so nothing can deliver the event. |
| ~~`smp/pairing.rs`~~ | ~~No SMP timer; no `self.failed` guard.~~ **Closed.** §3.4's 30 seconds, the post-failure drop, and per-opcode `INVALID_PARAMETERS` — 16 guards, each mutation-proven, none previously covered by any test. Found on the way: a remote panic from one well-formed Pairing Random. **Still open:** `tick_smp` has no caller in the scene loop, and an inbound re-pair after a plain Pairing Failed is still dropped responder-side. |
| `profiles/ras.rs` | On-Demand Ranging Data and its Get/Ack/Retrieve flow; Ranging Data Ready / Overwritten notifications; mode-0 and mode-1 steps. `antenna_paths_mask` is hardcoded `0x01`. (`CONFIG_ID_SHIFT`'s 3-bit mask and the 5-octet mode-2 step are both **fixed**.) |
| **`cs/ranging.rs` — the sign convention is inverted** | Found by real silicon (`third_party/waves`, `tests/cs_real_capture_test.rs`). A tone delayed by τ arrives rotated by **−**2πfτ, so phase *decreases* with frequency; the capture does exactly that. `controller::propagation::propagation_phase_rad` returns **+**2πfd/c, `cs::ranging::estimate` matches it, and then clamps a negative slope to zero "because no propagation can produce one". Fed a real metre of separation, `estimate` returns **0.0 m for all 62 procedures** — silently, with a healthy ±54 mm standard error beside it. Simulator and estimator agree with each other, which is why every test passed. **The fix must flip `propagation.rs` and `ranging.rs` together**, or every simulated ranging test inverts. |
| `packets/hci_types.rs` — `LeCsSubeventResultHeader` is two octets too long | It gives `procedure_done_status`, `subevent_done_status`, `procedure_abort_reason` and `subevent_abort_reason` an octet each; the spec (and rootcanal's PDL, and Zephyr) pack the two statuses into one octet and the two abort reasons into another. 17 octets where the spec has 15. `controller::sim` emits the same wrong layout so both agree; `cs::tones::parse_subevent_result` compensates by reading counts from step 0. Needs three coordinated edits. |
| `controller/sim.rs` | ~~Its catch-all answers every unhandled opcode with Command Complete~~ **Closed.** The real count is **61 Command-Status-only commands of 339** in Core 6.3, not 57 of 319 — the earlier estimate missed the `[v1]`/`[v2]` opcode pairs. `COMMAND_STATUS_OPCODES` now carries the derived table, the catch-all answers anything in it with a Command Status (`UNKNOWN_HCI_COMMAND`) instead of a Command Complete, and `scripts/check_hci_command_answers.py` fails CI if the table drifts from the specification. 19 have real arms; the other 42 are answered with the right *shape* and no modelled behaviour. |
| `controller/sim.rs` | Of the 42 Command-Status commands answered `UNKNOWN_HCI_COMMAND`, the ones worth modelling next, in order: **LE Extended Create Connection** \[v1] 0x2043 / \[v2] 0x2085 (any modern host uses it in place of LE Create Connection, and its completion event is LE *Enhanced* Connection Complete, which nothing here emits yet), **LE Read Remote Features Page 0** 0x2016, **LE Enable Encryption** 0x2019 (no link encryption is modelled at all), and **LE Request Peer SCA** 0x206D. |
| `controller/sim.rs` | What the new arms do *not* model, by their own comments: LE Connection Update applies immediately with no `LL_CONNECTION_UPDATE_IND` instant and can never be rejected by the peer; LE Set PHY skips `LL_PHY_REQ`/`LL_PHY_RSP` and PHY Options entirely; LE Create CIS / LE Accept CIS Request model the *handshake* only — no CIG, no ISO interval, no flush timeout, and ISO SDUs still ride the ACL handle. |
| `controller/sim.rs` | ISO data paths not modelled — an SDU is delivered whether or not a path was set up. BIG modelling is **sequencing only**: no PHY, no scheduling, no encryption (the broadcast code is compared, never used), no timeouts. |
| ~~`classic/avrcp.rs`~~ | ~~`RequestContinuingResponse` not modelled on the send path.~~ **Closed — and neither path modelled it, with the two halves failing differently.** *Send:* the target emitted the whole response as one AVRCP PDU however long and let AVCTP fragment the over-long AV/C frame underneath. Two simble ends agree on that, which is why 1 287 lines of `avrcp_test.rs` never noticed; AVRCP 4.4.1 caps the control-channel AV/C frame at 512 bytes and 6.3.1 puts the fragmentation at the AVRCP layer, so a conforming controller may drop it. *Receive:* the controller reassembled `START`/`CONTINUE` fragments it had no way to ask for — against any target that fragments per spec, a `GetElementAttributes` with real metadata never answers, with no error anywhere. That is the silent half. The target now fragments at the channel's frame limit and holds the tail; the controller pulls it on the *original* transaction label; `AbortContinuingResponse` discards it. Found on the way: one `PduAssembler` for the whole connection meant any interleaved response — a CHANGED notification, unsolicited by definition — destroyed a half-read fragmented one; the assemblers are keyed by transaction label now. All three fixes mutation-proven against `tests/avrcp_continuation_test.rs`. **Still open:** AVRCP's **browsing** channel (PSM 0x001B) is unwired, and no foreign stack has witnessed the fragmented path — Bumble's own AVRCP carries `# TODO: fragmentation` on both send paths and its controller never sends a continuation request, so it cannot be the oracle for this one. |
| `cs/ranging.rs` | No multipath model — the radio has none, so a computed distance is line-of-sight by construction. |

## 3. Things that exist but cannot be reached

| Where | Gap |
|---|---|
| `types/hci_types.rs` | `GapDataType` — a **third** AD-type table, with its own `Display` impl naming fifteen types — has **zero references anywhere in the tree**. It duplicates `gap::advertising::ad_type`, and being unused it was already missing 0x2E and 0x30 while the live table had them. Delete it, or make it the one table. |
| ~~Scripting~~ | ~~Rhai has **no way to load a catalog device**.~~ **Closed.** `catalog::device("hrm")` runs the entry in the caller's own engine (`src/scripting/catalog.rs`), so what comes back is the `ScriptGattServer` a scene already collects — the load *is* the peripheral being added. `catalog::names()`/`catalog::source()` alongside it. |
| ~~Rhai test surface~~ | ~~`assert_over` is **MCP-only**; `wait_for` appears in zero shipped examples.~~ **Closed.** `src/scripting/monitor.rs` puts the MCP monitor's semantics (0.1 s samples, `< > <= >= == !=`, byte index, the extreme) on the script surface; `catalog/tests/monitor.{pass,fail}.rhai` and the `checked_thermostat` catalog entry use both primitives. **Still open:** `mcp.rs` keeps private copies of `compare`/`extreme_for` — `scripting::monitor` is the declared owner, but the delegation needs an edit to `src/mcp.rs`. |

## 4. Declared to users as unavailable

The API Explorer marks **52 members doc-only** (callbacks, `wait_for`, constant
tables — no form, because they cannot be driven by one line of Rhai) and **57
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

- ~~**`Cargo.lock` is gitignored**~~ **Closed** (`7a9c4ef`) — tracked, 119
  packages pinned, with a `.gitignore` comment saying why so it is not re-added.
- **`web/emulator/` has no slot in the top nav** (it has a landing-page card).
- ~~**24 test functions are duplicated** inline ↔ `tests/`.~~ **Closed** —
  22 inline copies deleted (the `tests/` body was a strict superset in 21;
  one merge carried an assertion across), 2 rfcomm pairs deferred. `tests/mod.rs`
  deleted with them: it re-ran 35 of 44 files as a second binary, double-counting
  376 functions, and `AGENTS.md` had been *instructing* agents to keep it.
- ~~**`.venv/` is tracked in git.**~~ **Closed** (`65ecb30`) — 2 789 files
  untracked. It was also the reason the crate could not be published:
  `cargo package` was **105.9 MiB (37.4 compressed)** against crates.io's
  10 MiB limit, and is now **5.6 MiB (1.4 compressed)**.
- **`transport/usb.rs` is 45.82% covered, not the ~68% it displayed** — 332
  lines of self-agreeing `MockEndpoints` test were inflating it. It is now the
  worst-covered transport and the only one with neither a loopback nor a
  foreign-vector check.
- **~11 files still carry inline `#[cfg(test)]`**, led by
  `device/classic_host.rs` at **1 111 lines** (a third of what remains). Ten
  files were moved to sibling `#[path]` files; `classic_host.rs` was deferred
  only because an agent owned it at the time. By *share*, `crypto/smp_crypto.rs`
  (52%), `audio/lc3.rs` (47%) and `profiles/ascs.rs` (45%) are worst — though
  spec-vector tables are legitimately bulky.
- **The SIG assigned-number CI job is `continue-on-error` with no in-tree
  backstop.** The HCI command-answer job is also non-blocking but has one (the
  offline sweep test that sends all 61 Command-Status opcodes); the SIG job has
  nothing, so registry drift goes unnoticed unless someone reads the log.
- **`sbc.rs::transient_signal` is duplicated and unlinked.** A second,
  currently byte-identical copy lives at `tests/sbc_interop_test.rs:286`, and
  the libsbc golden vectors are keyed to *that* copy. If the two ever drift the
  goldens silently stop describing the signal the unit tests use, and both
  suites stay green.
- **`wasm_ws.rs`'s `run_until` returns `usize`** — ticks consumed, so a test can
  assert *progress* rather than eventual success. That is the strongest of the
  seven `run_until` contracts and the one not adopted when they were
  consolidated; adopting it would rewrite every `assert!` call site.
- **`wasm_ws.rs` is 5 156 lines** (down from 6 818 — three test modules
  totalling 1 671 lines moved out) and holds AD decoding, scan parsing, Rhai web
  extensions, `ScriptedPeripheral`, `CentralDevice`, `SceneEngine`,
  `run_test_script`, `lint_script` and **41 wasm_bindgen exports**. The
  `LeHost` extraction it was waiting for has landed (`device/host.rs`); the
  remaining split is the ~2 200 lines of bindings and finding `SceneEngine` a
  home outside `transport/`.

## 7. Structural gaps against Bumble

*Compiled 2026-08-23, **all three closed the same day** (`f99df5f`, `0d3dab9`,
`4e125bc`). Kept as a record of what each needed and what is still missing
inside each one — none is "done" in the sense Bumble is done.*

| Gap | State |
|---|---|
| ~~**Security, both transports**~~ | **Closed.** SSP (IO-capability exchange, association model, User Confirmation), link keys with a bond store, Authentication Requested / Set Connection Encryption, and LE Enable Encryption. Verified against Bumble over netsim in six live runs asserting *Bumble's* keystore and key-type flag. **Still missing:** CTKD; legacy PIN pairing is deliberately unmodelled; link-key derivation is `aes_cmac` over the addresses, documented as **not** the spec's f2 (no P-192 ECDH), which is the no-PHY scope holding. No `SceneEngine`/`ClassicDevice` wiring, so a scene cannot yet ask for an encrypted link. |
| ~~**SCO/eSCO**~~ | **Closed.** H4 type `0x03` routed on a handle deliberately separate from the ACL's; Setup/Enhanced/Accept/Reject with Synchronous Connection Complete at both ends; the Car page's audio box is solid while a link exists. **Still missing:** no codec — payload crosses byte-for-byte, CVSD and mSBC are a seam, not an implementation. `0x2D` Changed is absent by choice (nothing renegotiates a live link). Interop proved the **AT layer only**; the synchronous link itself has never met a foreign controller. |
| ~~**Classic profiles as connectable devices**~~ | **Partly closed.** `ProtocolHandler` learned multi-PSM and per-channel dispatch, so A2DP (source + sink, SBC) and Classic HID (device + host, both PSMs) are real scene devices. A foreign Bumble source streamed 40 libsbc frames into our sink. AVRCP followed: `device/avrcp.rs` is both roles as `ProtocolHandler`s, `device/media_scene.rs` puts them on a link (and pairs one with A2DP, so a PAUSE from the speaker stops the audio), and `tests/interop/avrcp_peer.py` ran Bumble's controller *and* Bumble's target against ours live. **Still missing:** AVRCP's browsing channel (PSM 0x001B) is unwired — the PDUs are modelled, the framing is not; HFP has signalling and SCO but no `ProtocolHandler`; simble's A2DP *source* has never met a foreign sink; no Rhai bindings for any of it; and `SdpQueryHandler::read_record` drops any record without an RFCOMM channel, so an A2DP Audio Sink record is invisible to an SDP search. |

Parity with Bumble is *not* the goal — Bumble has no scripting, MCP, or web
surface, which is where simble's value lives. Bumble's role stays what it has
been: the foreign peer that proves each of these once built.

## 8. Public API surface — no boundary has ever been drawn

*Added 2026-08-23. Not a bug list: a decision nobody has made. The crate is
`0.1.0`, which is exactly when this costs nothing to fix.*

> **Mostly closed the same day — see `docs/api-surface.md` for the
> measurement, the boundary and the mechanism.** Nine plumbing modules
> (`packets`, `att`, `l2cap`, `gap`, `smp`, `crypto`, `df`, `audio`, `obex`)
> are now `pub(crate)` behind a `testing` feature that `cargo test` turns on
> through a self-dev-dependency, which took the reachable public surface from
> 7 486 items to 5 636 with no test or example edited; CI gained a
> no-features step so `--all-features` cannot hide the closed build. All 14
> spec-discriminant enums carry `#[non_exhaustive]`, and `GapDataType` is
> deleted. **Still open from this section:** the unknown-wire-value policy
> (deliberately undecided — the trade-off is written up as §8 of
> `api-surface.md`, which found a *fourth* answer already in the tree), the
> ~390 unreferenced SIG constant tables in supported modules, and
> `#[doc(hidden)]` / sealed traits, still zero of each.

- **~3 500 public items against 372 `pub(crate)`**: 1 527 `pub fn`, 1 578
  `pub const`, 401 `pub struct`, 296 `pub mod`. `lib.rs` re-exports **all 25
  modules**, so `packets`, `controller`, `l2cap` and `att` are public API and
  every field offset in `df/packets.rs` is a compatibility promise.
- **Zero `#[doc(hidden)]`, zero `#[non_exhaustive]`, no sealed traits.** The
  three modules whose docs say "internal" are fully `pub` anyway.
- **14 of 128 public enums carry explicit spec discriminants** — `AseState`,
  `SamplingFrequency`, `FrameDuration`, `Mute`, `GainMode`, `MediaState`,
  `AddressType` and the rest. These model fields the SIG can extend, so adding
  a spec-defined value is a breaking change for every downstream `match`.
  `#[non_exhaustive]` on those 14 is ~an hour and costs nothing in-crate. The
  other 114 are our own state machines (`StreamState`, `ClassicPhase`) where it
  would be ceremony.
- **The question underneath is bigger: there is no policy for an unknown wire
  value, and the 14 already disagree three ways.** `bap.rs::from_u8` returns
  `Option`, so unknown becomes `None` and is destroyed. `hci_types.rs` uses a
  newtype with a `Display` fallback (`UNKNOWN (0x07)`), so it survives and can
  be echoed back. `ascs.rs` has a bare `_ =>`. The newtype is the right answer
  for anything a foreign peer sends — this session's most expensive bugs were
  all "we lied about what the peer said" — but converting `AseState` would
  touch the state matrix that just landed. **Decide the policy first, convert
  second.**
- Deciding which modules are *supported* (`device`, `devices`, `scene`,
  `scripting`, `api`, `types`) versus *exposed for inspection* (`packets`,
  `controller`, `l2cap`, `att`), and saying so in `lib.rs`, is the cheap half
  and makes a 1.0 possible later.

## 9. Test-surface gaps opened by today's work

- **`lea_source.py` cannot assert what the sink *received*.** It now has six
  checks on foreign facts, but a source streaming into a void would still pass.
  Closing it needs a **headless LE-audio sink example** — which would also make
  the script CI-runnable, since Bumble models CIG/CIS and ISO data paths.
- ~~**Four interop scripts still cannot run without netsim.**~~ **Two.**
  `classic_peer.py` now runs in CI against the **real rootcanal**, which
  upstream publishes as a prebuilt release binary (~16 MB, no Android SDK and
  no bazel) serving bare H4 over TCP — the thing `RootcanalTransport` and
  Bumble's `tcp-client:` already speak. All three inquiry-result forms, SDP
  continuation and SSP run there. See `scripts/fetch_rootcanal.sh` and
  `tests/interop/rootcanal_link.py`.
- **The `auracast_*` pair still cannot run without netsim**, and the reason
  changed: it is no longer "Bumble models no BIG" but **no controller
  reachable without the Android SDK enables BIG**. The upstream rootcanal
  release *implements* BIG (`rust/src/llcp/iso.rs` handles `LE Create BIG`
  and `LE BIG Create Sync`) but ships with those entries left out of its
  supported-commands table, so it answers both with `Unknown HCI Command` —
  while the rootcanal bundled inside netsim has them enabled. The same code,
  gated differently in the two builds. The scripts read
  this off the live controller's supported-commands bitmap, so the day
  upstream ships BIG they run with no edit.
- **A controller that answers is not the same as a controller that works,**
  and the gap is not hypothetical. `rootcanal-rs`'s `rootcanal-ws` links a C
  stub whenever `build.rs` finds neither `$ROOTCANAL_LIB_DIR` nor bazel, and
  that stub answers *every* command with a well-formed Command Complete,
  status `0x00`. A `Reset`-and-wait probe passes against it; so would any
  script that only checks an exit status. `rootcanal_link.py` asserts on the
  *content* of the answers instead — 6 `BD_ADDR` bytes, 8 version bytes, a
  64-byte command bitmap, where the stub returns 0 of each — and CI runs that
  probe as its own step before any script. **Anything else pointed at a live
  controller deserves the same treatment.**
- **No foreign stack has witnessed AVRCP fragmentation, and none can.**
  Bumble's `send_avrcp_response` and `avctp.send_message` both carry a literal
  `# TODO: fragmentation`, and its controller never sends a continuation
  request. That fix rests on spec text plus mutation testing alone.
- ~~**The Explorer is behind the surface it documents.**~~ **Closed
  2026-08-24** — and the gap was ~3× what this entry claimed: 100 members
  documented against 177 registered, the largest omission being the *entire*
  Auracast surface, not the three named here. `tests/explorer_surface_test.rs`
  now walks the eight registration sites and fails when a registered name has
  no entry; mutation-proven (4 of 6 tests fail against the previous page).
  **Still open:** `web/testing/testing.js` still offers only the three original
  example scripts.
- **A truncated advertising name still claims to be complete.**
  `fit_within_legacy_limit` trims a name to fit the 31-byte legacy budget and
  rebuilds through `with_name`, which always emits `COMPLETE_LOCAL_NAME`
  (0x09). Per CSS Part A §1.2 a truncated name must be `SHORTENED_LOCAL_NAME`
  (0x08). Found because the scanner showed a device's name flickering between
  the advertisement's truncated form and the scan response's full one — a real
  scanner has to cope either way, but our emitter is lying about which it
  sent.

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
