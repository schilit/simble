# What the tests here can and cannot prove

*Written 2026-08-23 from a coverage analysis plus a week of bugs. The headline
number is 89.86% line coverage, and this document is mostly about why that
number is close to useless on its own.*

---

## The governing rule

**A test with simble on both ends proves that simble agrees with itself.**

That is not a slogan. An `LE Create Connection` carrying 12 of its 25 parameter
bytes passed every simulated test in this repository and was rejected by the
first real controller it met. Four RAS characteristic UUIDs were *invented* —
assigned to nothing — and every test passed, because both the server that
published them and the client that read them used the same wrong constant.

So tests here divide by what they can disagree with:

| Kind | Can catch | Cannot catch |
|---|---|---|
| Self round-trip (encode→decode) | Nothing about correctness | A wrong constant, a wrong layout, a wrong order |
| Device-to-device | Sequence, state, teardown | Wire format — both ends share the structs |
| Foreign oracle (Bumble, liblc3, libsbc, netsim, SIG vectors) | Wire format, real semantics | Only what it happens to exercise |
| Axis sweep | Whole input classes | Anything about intent |

---

## The headline number is inflated three ways

**89.86% line / 88.35% function** (`cargo llvm-cov`, at `9df1253`). Corrections:

1. **Inline `#[cfg(test)]` bodies count as covered production lines.**
   Recomputing over only the lines above the `#[cfg(test)]` marker drops files
   sharply: `device/car_kit.rs` 79%→71%, `packets/ext_adv.rs` 81%→76%,
   `device/big_receiver.rs` 90%→**78.5%**.
2. **`tests/mod.rs` re-runs 35 of the 44 integration files as a second binary.**
   369 test functions execute twice. There are **1 044 distinct** test
   functions; a headline of ~1 400 is ~26% double-counting.
3. **The nine files it omits include the foreign-oracle ones** —
   `bumble_vectors_test`, `sbc_interop_test`, `adts_interop_test`. The
   duplication actively favours the self-checking tests.

---

## Load-bearing code with no foreign oracle at all

Ranked by traffic. This list is the most useful output of the analysis.

| Module | Size | Note |
|---|---|---|
| `profiles/ras.rs`, `cs/*`, `device/channel_sounding.rs`, `ranging_scene.rs` | ~3 200 lines | Neither Bumble nor Zephyr implements RAS. The UUIDs were caught only by reading the SIG registry. The *physics* has no reference either — the path-loss test inverts simble's own model. |
| `classic/avrcp.rs` + `avctp.rs` + `avc.rs` | 4 339 | Zero inline tests. Bumble implements AVRCP; nothing points at it. |
| `classic/avdtp.rs` | 2 207 | Zero inline tests. The media it carries is oracle-checked; the signalling that sets it up is not. |
| `classic/sdp.rs` | 1 438 | Round-trip only. Bumble has an SDP client. |
| `smp/pairing.rs` | 1 136 | Primitives have Core-spec vectors; the session protocol has none. |
| `profiles/bap.rs`, `bass.rs`, `pacs.rs`, `csip.rs` | ~2 100 | BAP's BASE is checked by the auracast scripts **manually and never in CI**. |
| `profiles/ancs.rs`, `ams.rs` | 2 093 | Apple protocols, zero inline tests, no oracle. |
| `packets/ext_adv.rs`, `big.rs`, `iso.rs`, `hci_events.rs` | ~2 600 | Exercised by interop runs, never asserted field-by-field against foreign bytes. |
| `classic/hid.rs`, `device/hid_host.rs`, `hid_reports.rs` | ~2 150 | Descriptors and usage tables — the archetypal "wrong constant nobody notices". |
| `df/*` | 589 | Three of five HCI parsers never called. |
| `obex/*` | ~1 450 | Bumble has OPP. |

**Which do have one:** `crypto/*` (FIPS-197, RFC 3610, Core Vol 3 Pt H, ECDH
vector), `gap/ead.rs` (CSS Pt A §2.3), `audio/sbc.rs` (libsbc, both directions
— the strongest in the repo), `classic/a2dp.rs` ADTS framing (Bumble),
`classic/rfcomm.rs` (one Bumble SABM frame), `transport/ws.rs` (RFC 6455).
Manual and out-of-CI: `hfp_oracle.py`, `gatt_client.py`, `lea_source.py`,
`auracast_*.py`. `audio/lc3.rs`'s goldens are explicitly disclaimed as
non-conformance in its own module doc.

### The single highest-leverage change

**The four `tests/interop/*.py` runs already produce foreign bytes. Capture
them as consts and assert against them in-tree.** That converts four manual
checks into CI checks and closes the BAP, ext_adv, big and ASCS oracle gaps at
once.

---

## Gaps ranked by consequence

1. **`smp/pairing.rs` failure paths.** *(Partly closed in `be4f832`.)* `fail()`
   had execution count zero. Three tamper tests now cover confirm and DHKey
   rejection, and they are mutation-proven: with the checks disabled, both
   end-to-end pairing tests still pass and only the new ones fail. Still open:
   the `INVALID_PARAMETERS` guards per opcode, `Phase::Failed` entry from every
   state, and the missing 30-second timer.
2. **ASCS state machine.** Every `INVALID_ASE_STATE_MACHINE_TRANSITION` return
   is count 0. `AseState::Releasing` appears once across all tests. The right
   test is a table over {state} × {opcode} asserting the response code **and
   that the state did not change on rejection** — the second half is what makes
   it real, since a rejection that still mutates state is the actual bug shape.
3. **`bap.rs` BASE and codec config — self-round-trip only.** No exact-wire-byte
   assertion anywhere. If `Freq16000` encoded as `0x04`, or the Frame Duration
   and Audio Channel Allocation LTV codes were transposed, every test passes and
   no real sink renders the stream.
4. **`big_receiver.rs` failure ladder.** `on_command_status()` 100% uncovered;
   all four `Failed(status)` assignments count 0; Sync Lost never simulated.
5. **`df/packets.rs` — 51.9%.** Three of five parsers never called, including
   the connectionless IQ report. These are `#[repr(C)]` zerocopy structs with no
   serialiser, so a wrong field offset can only be caught by parsing real bytes;
   there is no round-trip to accidentally cover it. **Smallest high-value fix
   available.**
6. **`classic/avdtp.rs` acceptor rejects.** `StreamState::Closing` and
   `Aborting` are never asserted by any test.
7. **`mcp.rs::serve_stdio()` — count 0 in full.** The agent-facing I/O loop was
   rewritten twice recently. A regression that blocks on stdin presents as "the
   MCP server hangs" with a green suite.

---

## Duplicated tests

**24 pairs**, inline `#[cfg(test)]` ↔ `tests/`: aics ×11, vocs ×5, bap ×4, at
×2, rfcomm ×2. All 24 have differing bodies, and in each of the three real
drifts **the inline copy is the weaker one** — e.g.
`test_volume_offset_out_of_range_is_rejected` asserts on `MIN_VOLUME_OFFSET - 1`
inline (derived from the constant under test, so a wrong constant still passes)
against the literal `-256` in `tests/`.

Recommendation: delete the inline copy of each pair, and either delete
`tests/mod.rs` or make it the only way integration tests run.

---

## Tests whose name promises more than the body checks

- `car_kit.rs::test_the_service_level_connection_runs_in_the_order_the_profile_specifies`
  — no ordering assertion in the body.
- `ascs.rs::test_truncated_pdu_reports_invalid_length` — covers one opcode;
  six others have untested `INVALID_LENGTH` paths.
- `avdtp_test.rs::test_abort_returns_stream_to_idle` — verifies the terminal
  state, never observes `Aborting`, so it cannot distinguish a correct
  transition from a jump straight to Idle.

The good pattern to copy is in `path_loss.rs`: alongside the self-inversion
test sit `test_a_wrong_exponent_biases_the_estimate_far_away` and
`test_a_wrong_reference_power_scales_every_estimate` — they perturb a constant
and assert the output moves.

---

## The bug shape to watch

Four bugs in one week, all the same: **a command whose answer nobody handled,
or a state with no exit.**

- `BigReceiver::terminate()` — Command Complete unhandled → `Receiving` forever
- `CsInitiator` — Command Status unhandled → hung in `Securing`
- `sim.rs` CS Procedure Enable — unknown config answered with silence
- `sim.rs` CS Remove Config — no completion event

An interop script that only sets up and streams cannot reach any of them.
Device-to-device tests found two as a side effect of being written. The
systematic fix is the state×event matrix in gap 2 above, with the
exhaustiveness rule: **for every command a host sends, one row per answer the
controller may give.** The SIG publishes the contract to derive it from — see
`docs/sig-as-oracle.md`.
