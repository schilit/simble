# RFCOMM: simble vs. Bumble vs. Zephyr

> The five correctness gaps in section 5 have since been fixed; read them as a
> record of what was wrong and why, not current behaviour. Two recommendations
> remain undone: the unused `DYNAMIC_CHANNEL_NUMBER_START/END` constants, and
> converting `MccPn` to a zerocopy typed view.

An audit of `src/classic/rfcomm.rs` against two mature RFCOMM stacks — Bumble
(`rfcomm.py`) and Zephyr (`rfcomm.c`) — prompted by simble's file being larger
than Bumble's (1512 vs 1164 lines, 1.30×).

**Conclusion: simble's RFCOMM is correct and appropriately sized.** On non-test,
non-comment code lines it is within 1% of Bumble; the apparent size gap is inline
`#[cfg(test)]` tests and doc-comment prose, not duplication or dead logic (§1).
The real payoff of the comparison was five correctness gaps — several shared with
Bumble, one simble-specific — since fixed (§5), plus a short list of dead code
(§6). Against Zephyr, the only feature delta is the optional half of the MCC
command set (§3).

---

## 1. Size is a measurement artifact

| | simble | Bumble | ratio |
|---|---|---|---|
| Raw file lines | 1512 | 1164 | 1.30× |
| Excluding the inline test module | 1193 | 1164 | 1.02× |
| **Non-test, non-comment code lines** | **894** | **895** | **1.00×** |

The 1.30× decomposes as ~320 lines of inline `#[cfg(test)]` tests (Rust puts unit
tests in the file; Python puts them in `tests/`, and simble has 239 more RFCOMM
tests there and in `classic_host.rs` that don't count here either), ~115 lines of
`//!`/`///` prose with ETSI/Core-Spec citations (an AGENTS.md convention; Bumble
instead carries ~50 lines of logging simble has no equivalent of), Bumble's airier
blank lines, and ~1 line of actual logic difference. Per-section counts don't line
up because Bumble holds the per-DLC frame handlers inside its `DLC` class while
simble folds them into `Multiplexer` (a `Dlc` holding `&mut Multiplexer` isn't
expressible without interior mutability) — so only the totals compare.

---

## 2. The third reference: Zephyr

**Zephyr**, `subsys/bluetooth/host/classic/rfcomm.c` — Apache-2.0 (license-
compatible to read and describe), C, a complete production RFCOMM. **1952 lines,
1440 code lines**, excluding its `rfcomm_internal.h` packet structs. That is
1.61× simble's logic on the same metric, and the delta is entirely features
simble does not have (section 3).

Not NimBLE: it is BLE-only, with no BR/EDR host and therefore no RFCOMM. Not
BlueZ: GPL, incompatible with this Apache-2.0 repo.

---

## 3. Feature matrix

| Capability | simble | Bumble | Zephyr |
|---|---|---|---|
| FCS (CRC-8, TS 07.10 Annex B) | yes | yes | yes |
| Frame encode/decode (SABM/UA/DM/DISC/UIH) | yes | yes | yes |
| 2-byte EA length field | yes, correct | **encodes; decode is buggy** | yes |
| Initiator role (SABM(0), open DLC) | yes | yes | yes |
| Acceptor role (listen, accept DLC) | yes | yes | yes |
| Multiplexer session state machine | yes, 6 states | yes, 7 states | yes, 9 states |
| Per-DLC state machine | yes, 5 states | yes, 6 states | yes |
| PN (Parameter Negotiation) | yes | yes | yes |
| PN convergence-layer (CFC) negotiation | **no — CFC assumed** | **no — CFC assumed** | yes |
| Credit-based flow control | yes | yes | yes |
| Credit top-up at low-water mark | yes | yes | yes |
| MSC (Modem Status) exchange | yes | yes | yes |
| MSC FC (aggregate flow control) honoured | **no** | **no** | yes |
| RPN (Remote Port Negotiation) | no | no | yes |
| RLS (Remote Line Status) | no | no | yes |
| TEST / FCON / FCOFF | no | no | yes |
| NSC (Non-Supported Command response) | **no** | **no** | yes |
| DLC open by bare SABM (no PN) | **no — silently dropped** | **no — silently dropped** | yes |
| Dynamic server channel allocation | **no** (constants exist, unused) | yes | yes |
| DLC teardown on session close | **no** | yes (`abort()`) | yes |
| Peer-DISC on a DLC removes it + notifies | **yes** | **no** (`# TODO`, leaks) | yes |
| T1/T2 timers, retransmission | no (by design — no async) | no | yes |
| SDP record build + RFCOMM channel discovery | yes | yes | n/a (separate file) |
| Logging | no (by design) | yes | yes |
| Zerocopy typed frame header | yes | n/a | n/a (C structs) |

Reading the matrix: against **Bumble**, simble is at feature parity plus two
correctness wins and minus one feature (dynamic channel allocation). Against
**Zephyr**, both Python and Rust are missing the optional half of the MCC
command set and the CFC negotiation that gates it.

### Where simble is genuinely better than Bumble

1. **`RfcommFrame::parse` decodes the 2-byte length correctly.**
   Bumble's `parse_mcc` (`rfcomm.py:284`) computes
   `length = (data[3] << 7) & (length >> 1)` — a bitwise `&` where `|` is
   meant, *and* the wrong index (`data[3]`; the MCC length octets are
   `data[1]`/`data[2]`). `RFCOMM_Frame.from_bytes` (`rfcomm.py:330`) repeats
   the `&`-for-`|` bug. Neither fires in practice (MCC payloads here are ≤8
   bytes, and the frame path never uses the decoded value to slice), but the
   code is wrong. Simble's `parse_mcc` (`rfcomm.rs:287–299`) does
   `(len_byte0 >> 1) | (len_byte1 << 7)` against the right octets.
2. **Peer-initiated DLC teardown is handled.** Simble's `Action::ClosedByPeerDisc`
   (`rfcomm.rs:975`) removes the DLC and emits `DlcClosed`. Bumble's
   `DLC.on_disc_frame` (`rfcomm.py:570`) is a `# TODO: handle all states` that
   sends UA and leaves the DLC in `self.dlcs` still marked `CONNECTED` — a leak,
   and subsequent writes go into a dead link.

Both differences are real logic, and both are within the ~1-line net total,
because they trade against features Bumble has that simble does not.

---

## 4. Is the size justified?

**Yes.** There is no meaningful waste in this file.

- **Duplication:** `cargo dupes report --exclude-tests` flags six entries in
  `rfcomm.rs`, five of which are the expected "structurally identical,
  semantically distinct" category AGENTS.md explicitly permits: the four
  three-line frame constructors (`sabm`/`ua`/`dm`/`disc`, `rfcomm.rs:181–198`)
  and single-expression accessors (`Dlc::is_open`, `Multiplexer::is_connected`).
  The one substantive flag is addressed in section 6.
- **Dead code:** two unused public constants (finding W1). Nothing else.
- **Verbosity:** the state machine is dense; `on_dlc_frame`'s local `Action`
  enum (`rfcomm.rs:924–930`) is 20 lines longer than Bumble's equivalent, but it
  exists to end the `&mut self.dlcs` borrow before mutating `self.state` — a
  borrow-checker cost, not padding, and the alternative is `RefCell`.
- **Hand-rolled parsing:** already partly zerocopy. What remains is either a
  legitimate zerocopy candidate (finding W2) or genuinely variable-length EA
  encoding that zerocopy cannot express, as the module doc already says.

The FCS table alone is 34 lines in both files and 2.8% of simble's file; the
rest is a fair, tight implementation of the same protocol.

---

## 5. Correctness gaps (ranked — these matter more than size)

`docs/HANDOFF-2026-08-22.md` item 7 makes RFCOMM-on-`ClassicHost` the
highest-value Classic item, so these are ranked by how likely they are to bite
that work against a real peer.

### C1 — A bare SABM on an unknown DLCI is silently dropped, not even DM'd

`src/classic/rfcomm.rs:933–935`. `on_dlc_frame` looks up `self.dlcs`, finds
nothing, and returns `(vec![], vec![])`. PN is **optional** before SABM
(RFCOMM 1.1 §5.3 / TS 07.10 §5.4.6.1): a peer may open a DLC with a bare SABM
using default parameters. Zephyr handles this. Simble sends *nothing*, so such
a peer hangs until its own T1 fires (60 s in Zephyr) rather than failing fast.

Minimum fix: answer `DM` so the peer gives up immediately. Full fix: create the
DLC with defaults (frame size 127, no CFC) when the channel is in
`listen_configs`. Bumble has the same gap (`rfcomm.py:821`, warn-and-return).

### C2 — Unrecognized MCC commands are dropped instead of answered with NSC

`src/classic/rfcomm.rs:828–831` (the `_ =>` arm). TS 07.10 §5.4.6.3.8 requires
a Non-Supported Command Response so the sender stops waiting. Simble's module
doc says incoming RPN is "silently ignored rather than answered" — that is
accurate as written but is not what the spec asks for; the spec's answer to
"not implemented" is NSC, not silence. Cheap: NSC is a 1-byte value field.
Bumble has the same gap; Zephyr sends NSC.

### C3 — The PN convergence-layer field is never read; CFC is assumed

`src/classic/rfcomm.rs:846–882`. `on_mcc_pn_command` ignores `pn.cl` and always
answers `cl: 0xE0` (`rfcomm.rs:870`). Per RFCOMM 1.1 §5.5.3 the responder may answer 0xE0 only if
the requester used 0xF0; if a peer requests `cl = 0x00` (no credit-based flow
control) it will not expect the leading credit octet on P/F=1 UIH frames, and
simble's credit octets will be delivered to the application as stream data.
Zephyr distinguishes `BT_RFCOMM_CFC_SUPPORTED` / `NOT_SUPPORTED`. Bumble's
`DLC.accept()` (`rfcomm.py:654`) hardcodes `cl=0xE0` identically.

Related: the MSC `FC` bit — the pre-CFC aggregate flow-control mechanism — is
parsed into `MccMsc::fc` and then never consulted. Simble sends regardless.

### C4 — Session teardown leaves DLCs alive (simble-specific)

`src/classic/rfcomm.rs:800–807` (`on_disc`) and the `Disconnecting` arm of
`on_ua` (`rfcomm.rs:792–795`) set `MultiplexerState::Disconnected` but leave
`self.dlcs` populated with `DlcState::Connected` entries, and emit no
`DlcClosed` for them. A caller can then `write()` into a torn-down session and
get frames back to send on a dead channel. Bumble's `on_l2cap_channel_close`
(`rfcomm.py:1018`) calls `dlc.abort()` on every DLC.

No test catches this because `RfcommHandler::on_channel_closed`
(`src/device/classic_host.rs:355`) drops the whole `Multiplexer`, which
sidesteps it. Fix is three lines: drain `self.dlcs` and emit `DlcClosed` for
each.

### C5 — `initial_credits > 7` is silently masked to nonsense

`MccPn::to_bytes` (`src/classic/rfcomm.rs:344`) masks `initial_credits & 0x07`
— correct, the field is 3 bits — but `Multiplexer::listen` (`rfcomm.rs:671`)
and `open_dlc` (`rfcomm.rs:682`) accept any `u8`, and `Dlc::rx_credits` is
initialised from the **unmasked** value. `listen(1, 1000, 8)` leaves simble
believing it granted 8 credits while the peer reads 0 and never transmits.
Bumble at least logs a warning (`RFCOMM_MCC_PN.__post_init__`, `rfcomm.py:372`).
Fix: reject or clamp at the `listen`/`open_dlc` boundary.

---

## 6. Waste findings (ranked by value)

### W1 — `DYNAMIC_CHANNEL_NUMBER_START` / `_END` are dead, and advertise a feature that does not exist

`src/classic/rfcomm.rs:48` and `:50`. Both are `pub const`, referenced nowhere
in `src/`, `tests/`, or `examples/`. They were carried over from Bumble, where
they are load-bearing: `Server.listen(channel=0)` scans that range for a free
channel (`rfcomm.py:1109–1125`). Simble has no allocator — `listen()` requires
an explicit channel — so the constants read as a supported capability that is
not implemented. Being `pub`, they draw no `dead_code` warning.

This is exactly the failure mode HANDOFF-2026-08-22 §5 warns about: a name that
looks like a feature. Two options, owner's call: implement `listen(0)` dynamic
allocation (~10 lines, matches Bumble and Zephyr), or delete both constants.
Not done here because deleting `pub` items is a public-API change.

### W2 — `MccPn` is the last good zerocopy candidate in this file

`src/classic/rfcomm.rs:333–363`. `MccPn::parse`/`to_bytes` hand-index a **fixed
8-byte little-endian record** — `data[4] as u16 | (data[5] as u16) << 8`, an
explicit `if data.len() < 8` guard, and an 8-element array literal. That is
precisely the shape `src/packets/hci_events.rs` was converted from, and the
project TODO's target. A `#[repr(C)]` struct with the six derives and
`max_frame_size: U16<LittleEndian>` (the pattern already used in
`src/df/packets.rs` and `src/packets/hci.rs`) replaces both methods with a
`Ref::from_prefix` and removes ~28 lines of index arithmetic plus the manual
bounds check.

`MccMsc` (`rfcomm.rs:383–409`) is a 2-byte *bitfield* record; zerocopy buys the
bounds check but every bit still has to be shifted out by hand, so the win is
marginal. `parse_mcc`/`make_mcc` and the frame length field are EA
variable-length and cannot be zerocopy — the module doc already states this
correctly.

### W3 — `RfcommServer::listen` is byte-identical to `Multiplexer::listen`

`src/classic/rfcomm.rs:1068–1075` vs `:671–678`. Both are eight-line
delegations to `reserve_listen_channel` with the same parameters and the same
doc sentence. `cargo dupes` flags the pair. They live on different types, so
collapsing needs a trait or an accepted `cargo dupes ignore`; 8 lines either
way. Low value, listed for completeness.

### W4 — `RfcommClient` is a 17-line wrapper around a one-line call

`src/classic/rfcomm.rs:1029–1046`. A unit struct whose sole method forwards to
`ClassicChannelManager::connect`. Nothing in `src/` uses it — the real client
path is `RfcommHandler` in `src/device/classic_host.rs` — and its only caller
is `tests/classic_integration_test.rs:386`, whose own comment describes it as
exercising "that facade". It mirrors Bumble's `Client`, but Bumble's owns the
multiplexer lifecycle (`start`/`shutdown`/`__aenter__`) while simble's does not.
Owner's call whether the symmetry is worth 17 lines.

### W5 — done: MSC frame construction was triplicated

**Applied in this change.** The identical six-line "build the default MSC
frame" sequence appeared three times: in `on_mcc_msc`, and in both the SABM and
UA arms of `on_dlc_frame`. Collapsed into a `msc_frame(dlc_c_r, dlci, command)`
free function beside `unknown_dlci`. Pure refactor, no behaviour change; net −6
lines. `cargo test` (1115 passing, 0 failing) and
`cargo clippy --all-targets --all-features -- -D warnings` stay clean.

---

## 7. Confidence limits on this comparison

- **Zephyr's `rfcomm.c` was read via a fetched summary**, not line by line. The
  1440-code-line figure is a direct count of the file; the matrix's feature
  claims come from the summary's identification of named handlers
  (`rfcomm_handle_rpn`, `rfcomm_send_nsc`, `rfcomm_check_fc`, …), whose bodies
  were not verified.
- **No interop testing was run.** Every gap in section 5 is from reading code
  against the spec, which does not prove interop; C1 and C3 in particular should
  be confirmed against Bumble-as-oracle before fixing.
- **Perf, on the record only:** `RfcommFrame::fcs()` recomputes `length_bytes()`
  on every call and `to_bytes()` calls both, where Bumble precomputes in
  `__init__` — irrelevant at simulator scale.
