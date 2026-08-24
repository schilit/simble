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

1. ~~**Inline `#[cfg(test)]` bodies count as covered production lines.**~~
   *Measured, and fixed for the ten largest offenders — see "Test bodies moved
   out of the implementation files" below.* The distortion was real but not
   uniformly in the direction assumed, and the earlier hand-estimate
   over-corrected.
2. ~~**`tests/mod.rs` re-runs 35 of the integration files as a second
   binary.**~~ *Fixed — see "Duplicated tests" below.* It re-ran 376 test
   functions, ~25% of a 1 528 headline. `tests/mod.rs` is gone and the headline
   is now 1 130, which equals the number of distinct test functions.
3. ~~**The files it omits include the foreign-oracle ones**~~ — same fix. The
   double-counting had favoured the self-checking tests over
   `bumble_vectors_test`, `sbc_interop_test` and `adts_interop_test`; it no
   longer exists to favour anything.

---

## Test bodies moved out of the implementation files

The ten largest inline `#[cfg(test)]` blocks now live in sibling files —
`sim.rs` keeps `#[cfg(test)] #[path = "sim_tests.rs"] mod tests;` and nothing
else. The tests are still compiled as part of the module, so private access is
unchanged; what changes is that `cargo llvm-cov` now attributes their lines to
`*_tests.rs` instead of to the production file. **9 175 lines of test body
stopped being counted as production code.** Same 1 323 tests, same names, same
module paths.

Line coverage, before (test bodies inline) and after (attributed separately):

| File | before | after | production lines |
|---|---|---|---|
| `transport/usb.rs` | 68.42% | **45.82%** | 299 |
| `device/big_receiver.rs` | 91.13% | **83.38%** | 337 |
| `mcp.rs` | 91.81% | 86.15% | 1 170 |
| `device/car_kit.rs` | 90.17% | 86.81% | 940 |
| `transport/wasm_ws.rs` | 92.16% | 88.27% | 1 970 |
| `controller/sim.rs` | 93.99% | 90.05% | 2 823 |
| `classic/rfcomm.rs` | 95.34% | 92.93% | 735 |
| `device/channel_sounding.rs` | 96.91% | 93.50% | 277 |
| `audio/sbc.rs` | 97.14% | 96.19% | 525 |
| `packets/att.rs` | 97.09% | **99.28%** | 276 |

Three things to take from this table.

**`transport/usb.rs` is the real finding: 45.82%, not the ~68% it displayed.**
332 lines of `MockEndpoints`-driven test were carrying it. It is now the
worst-covered transport by a wide margin, and it is the one transport with no
loopback test — `transport/ws.rs` has RFC 6455 vectors and a real bridge test,
`netsim` has the scripts, USB has a mock that agrees with itself.

**`packets/att.rs` went *up*.** The inline block does not only inflate; for a
small production file with assertion-heavy tests it *deflates*, because the
`_ => panic!("Expected ReadBlobReq")` arm of every `match` assertion is a line
that never executes while the test passes. Fifteen such arms in `att_tests.rs`
were being charged against `att.rs`. Any file whose tests assert by matching
pays this, and the payment is invisible until the bodies are separated.

**The earlier hand-estimate over-corrected**, and this is why: recomputing "over
only the lines above the `#[cfg(test)]` marker" assumes every line below it was
covered, which the panic arms disprove. It predicted `device/big_receiver.rs`
90%→78.5%; the measured drop is 91.13%→83.38%. Estimate the direction by hand
if you like, never the magnitude.

`packets/ext_adv.rs` (80.63%) still has its tests inline and is still
overstated; the old 81%→76% guess for it remains a guess.

Whole-crate figures, at this commit, `cargo llvm-cov --lib --tests`:

| | line | function |
|---|---|---|
| as reported (test files now their own rows) | 90.37% | 88.45% |
| **excluding the 12 moved test files** | **89.03%** | **87.26%** |

The moved test files themselves measure 99.20% — the missing 0.8% is exactly
the never-taken failure arms described above. **89.03% is the honest number for
the ten files that were fixed; the crate figure is still overstated by every
file that still has its tests inline.**

---

## Load-bearing code with no foreign oracle at all

Ranked by traffic. This list is the most useful output of the analysis.

| Module | Size | Note |
|---|---|---|
| `profiles/ras.rs`, `cs/*`, `device/channel_sounding.rs`, `ranging_scene.rs` | ~3 200 lines | Neither Bumble nor Zephyr implements RAS. The UUIDs were caught only by reading the SIG registry. The *physics* has no reference either — the path-loss test inverts simble's own model. |
| ~~`classic/avrcp.rs` + `avctp.rs` + `avc.rs`~~ | 4 339 | ~~Zero inline tests. Bumble implements AVRCP; nothing points at it.~~ **Corrected, then closed.** The *inline* half was true and the conclusion was not: `tests/avrcp_test.rs` is 1 287 lines, so AVRCP was never untested — it was **unreachable and unwitnessed**. Both are now fixed: `device/avrcp.rs` makes both roles scene devices, and `tests/interop/avrcp_peer.py` runs Bumble's AVRCP against simble's in *both* directions, live. `tests/avrcp_foreign_bytes_test.rs` pins the octets. |
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

## Duplicated tests — resolved

There were **24 pairs** sharing a name across inline `#[cfg(test)]` ↔ `tests/`:
aics ×11, vocs ×5, bap ×4, at ×2, rfcomm ×2. All 24 had differing bodies, and
wherever they had actually drifted **the inline copy was the weaker one**. The
clearest case: `test_volume_offset_out_of_range_is_rejected` asserted on
`MIN_VOLUME_OFFSET - 1` inline — derived from the very constant under test, so
a wrong `MIN_VOLUME_OFFSET` still passes — against the literal `-256` in
`tests/`.

**22 of the 24 inline copies are now deleted**; the `tests/` version survives in
every case, because in every case it was a superset. Four pairs had genuinely
drifted, all in the same direction:

| Pair | What only `tests/` had |
|---|---|
| `aics::test_mute_when_mute_disabled_is_rejected` | mute and change-counter unchanged after the reject |
| `aics::test_set_manual_gain_mode_when_manual_only_is_rejected` | gain mode unchanged after the reject |
| `vocs::test_set_volume_offset_requires_fresh_change_counter_each_time` | counter reached 2 |
| `bap::test_broadcast_audio_announcement_round_trip` | encoded length is 3 |

One pair needed a **merge** rather than a pick:
`vocs::test_volume_offset_out_of_range_is_rejected`. `tests/` had the strong
literal bounds but had dropped the inline copy's "a rejected write does not
advance the change counter" assertion; that assertion moved into the `tests/`
body. No assertion was lost in the de-duplication.

The remaining 2 pairs are `classic/rfcomm.rs`'s
`test_multiplexer_startup_handshake` and
`test_data_delivered_immediately_after_dlc_opens`, deferred while that file is
under other work. They are the same shape — resolve them the same way.

Not a pair, despite the shared name: `test_unsupported_opcode_is_rejected`
exists inline in `aics.rs`, `vocs.rs` and `ascs.rs`, and in `tests/vcp_test.rs`.
Four different services, four different subjects — all four stay.

### `tests/mod.rs` is deleted

Cargo already compiles every `tests/*.rs` as its own test binary. `tests/mod.rs`
declared 35 of them as modules a second time, so cargo built *it* as a 52nd
test binary too and those 35 files' **376** test functions ran twice per `cargo
test`. It had no reason to exist beyond `tests/` looking like a module
directory: nothing in `Cargo.toml` or `.github/workflows/ci.yml` referenced it,
and its history is only `mod` lines accreting since the initial commit. Worse,
the 16 files it left out were disproportionately the foreign-oracle ones
(`bumble_vectors_test`, `sbc_interop_test`, `adts_interop_test`), so the
double-counting inflated precisely the self-checking tests this document warns
about.

The suite headline therefore **drops from 1 528 to 1 130**, and that is the
point — the two numbers now mean the same thing:

| | before | after |
|---|---|---|
| lib (inline `#[cfg(test)]`) | 641 | 619 |
| `tests/*.rs`, once each | 510 | 510 |
| `tests/mod.rs` second run | 376 | — |
| doc-test | 1 | 1 |
| **`cargo test` reports** | **1 528** | **1 130** |
| **distinct test functions** | 1 152 | 1 130 |

(`cargo test` without `--all-features`; `--features lc3` adds 7 more.)

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
