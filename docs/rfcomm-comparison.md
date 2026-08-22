# RFCOMM: simble vs. Bumble vs. Zephyr

An audit of `src/classic/rfcomm.rs` prompted by the observation that it is
1518 lines against Bumble's `rfcomm.py` at 1163 — a 1.31× ratio.

**Verdict up front: the ratio is an artifact of the measurement.** Comparing a
Rust file *including* its inline `#[cfg(test)]` module against a Python file
that has no tests in it is not a like-for-like comparison. On non-test,
non-comment logic lines the two files are within 1% of each other. The extra
size is inline tests and doc prose, not duplication, verbosity, or dead logic.

There is, however, a short list of **real correctness gaps** — several shared
with Bumble, one specific to simble — that matter considerably more than the
line count. Those are section 5.

---

## 1. The line-count breakdown

Counted mechanically: a line is *comment* if it begins with `//` (Rust) or `#`
/ is inside a docstring (Python); *blank* if empty; *code* otherwise.

### simble — `src/classic/rfcomm.rs`

| Section | Lines | Code | Comment | Blank |
|---|---|---|---|---|
| Module doc + imports | 1–34 | 8 | 23 | 3 |
| Constants, `frame_type`, `mcc_type` | 35–76 | 20 | 19 | 3 |
| FCS table + `compute_fcs` | 77–126 | 42 | 5 | 3 |
| `RfcommFrame` (encode/parse) | 127–282 | 114 | 24 | 18 |
| MCC framing + `MccPn` + `MccMsc` | 283–422 | 104 | 26 | 10 |
| `Dlc` (credits, tx pipeline) | 423–550 | 92 | 28 | 8 |
| `Multiplexer` + free helpers | 551–1023 | 386 | 52 | 35 |
| `RfcommClient` / `RfcommServer` | 1024–1086 | 37 | 18 | 8 |
| SDP integration | 1087–1193 | 91 | 10 | 6 |
| **`#[cfg(test)] mod tests`** | **1194–1512** | **263** | **16** | **40** |
| **Total** | **1512** | **1157** | **221** | **134** |

Figures are post-cleanup (section 6); the file was 1518/1167 before.

### Bumble — `rfcomm.py`

| Section | Lines | Code | Comment/doc | Blank |
|---|---|---|---|---|
| License, imports, logger | 1–54 | 24 | 22 | 8 |
| Constants + FCS table | 55–119 | 53 | 3 | 9 |
| SDP integration | 120–233 | 81 | 21 | 12 |
| `compute_fcs` + `RFCOMM_Frame` | 234–359 | 103 | 6 | 17 |
| `RFCOMM_MCC_PN` / `_MSC` | 360–439 | 68 | 3 | 9 |
| `DLC` (incl. its frame handlers) | 440–754 | 252 | 21 | 42 |
| `Multiplexer` | 755–1031 | 223 | 16 | 38 |
| `Client` | 1032–1082 | 34 | 6 | 11 |
| `Server` | 1083–1164 | 57 | 8 | 17 |
| **Total** | **1164** | **895** | **106** | **163** |

Bumble has no test module in-file; its RFCOMM tests live in `tests/`.

### The like-for-like figure

| | simble | Bumble | ratio |
|---|---|---|---|
| Raw file lines | 1512 | 1164 | 1.30× |
| Lines excluding the inline test module | 1193 | 1164 | 1.02× |
| **Non-test, non-comment, non-blank code lines** | **894** | **895** | **1.00×** |

The 1.31× gap decomposes as:

- **~320 lines (90% of the gap): the inline `#[cfg(test)]` module.** 13 tests.
  Rust convention puts unit tests in the file; Python does not. Simble *also*
  has 229 lines of integration tests in `tests/rfcomm_test.rs` and 10 more
  RFCOMM tests in `src/device/classic_host.rs`, none of which counts here.
- **~115 lines: doc comments.** 221 vs 106. This repo's `//!`/`///` prose with
  ETSI/Core-Spec citations is a deliberate convention (AGENTS.md "Comments").
  Bumble compensates in the other direction with 38 `logger.*` calls and three
  `__str__` methods (~50 code lines) that simble has no equivalent of, since
  it carries no logging dependency.
- **−29 lines: blank lines.** Bumble is airier (163 vs 134).
- **~1 line: actual logic difference.**

Structural note on why the per-section rows don't line up: Bumble puts the
per-DLC frame handlers (`on_sabm_frame`, `on_ua_frame`, `on_uih_frame`,
`on_mcc_msc`) *inside* the `DLC` class, which holds a back-reference to its
`Multiplexer`. Simble folds the same logic into `Multiplexer` (`on_dlc_frame`,
`on_dlc_uih`) because a `Dlc` holding `&mut Multiplexer` is not expressible
without interior mutability. That moves ~150 lines across the section boundary
in both directions and inflates simble's `Multiplexer` row. Only the totals are
comparable.

---

## 2. The third reference: not NimBLE

**Apache Mynewt NimBLE does not implement RFCOMM.** NimBLE is a BLE-only
stack; there is no BR/EDR host, hence no L2CAP Classic, no SDP server, no
RFCOMM. A 2020 feature request asking for BR/EDR host + L2CAP + SDP + RFCOMM
was closed without implementation. There is nothing to compare against.

**Substituted: Zephyr**, `subsys/bluetooth/host/classic/rfcomm.c` — Apache-2.0
(so license-compatible to read and to describe), C, and a complete production
RFCOMM. **1952 lines, 1440 code lines**, excluding its
`rfcomm_internal.h` packet structs. That is 1.61× simble's logic on the same
metric, and the delta is entirely features simble does not have (section 3).

BlueZ was deliberately *not* read: it is GPL, and this repo is Apache-2.0.
Android's Fluoride (`system/stack/rfcomm/`) is Apache-2.0 and would have been a
valid second reference; I did not read it (section 7).

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

## 7. What was not checked

- **Android's Fluoride** (`system/stack/rfcomm/`) was not read. It is
  Apache-2.0 and would be a legitimate second production reference; Zephyr was
  sufficient to answer the question and I did not want to characterize a stack
  I had not opened.
- **BlueZ** was deliberately not read (GPL vs. this repo's Apache-2.0).
- **Zephyr's `rfcomm.c` was read via a fetched summary**, not line by line. Its
  1440-code-line figure is a direct count of the file I downloaded; the feature
  claims in the matrix come from that summary's identification of named
  handlers (`rfcomm_handle_rpn`, `rfcomm_send_nsc`, `rfcomm_check_fc`, …). I
  did not verify each handler's body.
- **No interop testing was run.** Every gap in section 5 is from reading code
  against the spec. HANDOFF-2026-08-22 §5 is explicit that unit tests do not
  prove interop; by the same token, neither does a code read. C1 and C3 in
  particular should be confirmed against Bumble-as-oracle before anyone spends
  time fixing them.
- **`tests/rfcomm_test.rs` (229 lines) and the 10 RFCOMM tests in
  `src/device/classic_host.rs` were read for coverage, not audited.** They are
  excluded from every count above.
- No performance work. `RfcommFrame::fcs()` recomputes `length_bytes()` on
  every call and `to_bytes()` calls both — Bumble precomputes in `__init__`.
  Irrelevant at simulator scale; noted only so it is on the record.
