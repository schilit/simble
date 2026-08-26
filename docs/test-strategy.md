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

This happened in two passes. The first took the ten largest inline
`#[cfg(test)]` blocks; the second took the twelve that were left above ~200
test lines. In both, the implementation file keeps `#[cfg(test)] #[path =
"foo_tests.rs"] mod tests;` and nothing else. The tests are still compiled as
part of the module, so private access is unchanged; what changes is that
`cargo llvm-cov` now attributes their lines to `*_tests.rs` instead of to the
production file.

### First pass — the ten largest

**9 175 lines of test body stopped being counted as production code.** Same
1 323 tests, same names, same module paths.

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

### Second pass — the remaining twelve

The twelve implementation files still holding more than ~200 lines of inline
test are now split the same way. **3 636 lines of test body moved**, which
`llvm-cov` counts as 2 356 executable lines that had been charged to production
files. Same 1 410 tests, same names, same module paths — verified by diffing
the full `module::path` → count map before and after, not just the total.

| File | before | after | production lines |
|---|---|---|---|
| `transport/netsim.rs` | 88.15% | **81.78%** | 247 |
| `device/hid_host.rs` | 96.24% | **90.62%** | 128 |
| `controller/lmp.rs` | 94.06% | 91.01% | 267 |
| `device/classic_host.rs` | 93.85% | 91.19% | 1 158 |
| `classic/rtp.rs` | 97.50% | 95.74% | 188 |
| `device/big_broadcaster.rs` | 97.18% | 95.71% | 326 |
| `transport/ws.rs` | 89.65% | 88.35% | 352 |
| `obex/server.rs` | 96.88% | 95.74% | 141 |
| `profiles/ascs.rs` | 95.48% | 94.36% | 514 |
| `profiles/ras.rs` | 99.03% | 98.28% | 174 |
| `cs/ranging.rs` | 99.08% | 98.00% | 100 |
| `gap/advertising.rs` | 99.68% | 99.43% | 176 |

**`transport/netsim.rs` is this pass's `usb.rs`: 81.78% line and 66.67%
*function*, not the 88.15% it displayed.** That makes it the second
worst-covered transport after USB, and it undercuts the sentence three
paragraphs up — "`netsim` has the scripts" was doing more reassuring than the
number supports. A third of its functions are never called by any test.

**`device/hid_host.rs` is the sharpest drop, −5.62pp, off only 128 production
lines.** A small file with a large inline block is where the distortion is
worst in relative terms; 190 lines of test were flattering 128 lines of code.

**Nothing went *up* this time.** `packets/att.rs` gained 2.19pp in the first
pass because its production file was small enough that the never-taken
`panic!` arms outweighed the inflation. None of these twelve is small enough
for that to win — the closest, `cs/ranging.rs` at 100 production lines, still
lost 1.08pp. The deflation effect is real but it only dominates below roughly
`packets/att.rs`'s scale.

Whole-crate figures after both passes, `cargo llvm-cov --lib --tests`:

| | line | function |
|---|---|---|
| as reported (test files as their own rows) | 90.49% | 88.47% |
| **excluding every `*_tests.rs`** | **88.59%** | **86.79%** |

The as-reported figure is **byte-for-byte identical before and after this
pass** — 90.49% / 88.47% either way. That is the proof the move is pure
re-attribution: the same lines execute, they are merely filed under a different
name. Only the production-only figure moves, 89.15% → 88.59%, and that 0.56pp
is the part that was never production code to begin with.

`packets/ext_adv.rs` (80.63%) and every inline block under ~200 lines are still
unmeasured in this sense, so **88.59% remains an upper bound, not the answer.**

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
`audio/lc3.rs`'s goldens are explicitly disclaimed as non-conformance in its
own module doc.

### Foreign oracles now in CI — and the ones still not

*This section replaces "Manual and out-of-CI: `hfp_oracle.py`,
`gatt_client.py`, `lea_source.py`, `auracast_*.py`", which is no longer true
of the first two.*

The `tests/interop/*.py` scripts needed `netsimd` from the Android SDK, which
is why none of them ran in CI. That was a mistaken requirement: they need a
*controller and a link*, and **Bumble ships both** — `bumble.controller` plus
`bumble.link.LocalLink` is the same architecture as simble's `sim.rs` plus
`Link`. `tests/interop/bumble_link.py` publishes one such controller over
`tcp-server:` (bare H4), which simble's **existing** `RootcanalTransport`
already speaks. No new transport was needed; `src/transport/live.rs` only
*picks* between the two that existed.

A second controller then followed, for the scripts Bumble cannot host at all.
**rootcanal is published as a prebuilt binary** — a ~16 MB GitHub release
asset from `google/rootcanal`, no Android SDK and no bazel — and it serves
bare H4 over TCP, the same thing `RootcanalTransport` and Bumble's
`tcp-client:` already speak. `scripts/fetch_rootcanal.sh` installs it and
`tests/interop/rootcanal_link.py` runs it. Still no new transport.

| Script | In CI | Controller | State |
|---|---|---|---|
| `hfp_oracle.py` | ✅ | none | needs no controller at all — was miscategorised as netsim-dependent |
| `gatt_client.py` | ✅ | Bumble | full run under `--transport bumble` |
| `a2dp_peer.py` | ✅ | Bumble | full run |
| `avrcp_peer.py` | ✅ | Bumble | both phases, including the foreign `delegate.volume` |
| `classic_peer.py` | ✅ | rootcanal | full run under `--transport rootcanal`: all three inquiry-result forms, SDP continuation, and SSP including the authenticated-key assertions |
| `auracast_source.py` | ❌ | — | needs **BIG**, and *no* controller reachable without the SDK has it (below) |
| `auracast_sink.py` | ❌ | — | same |
| `lea_source.py` | ❌ | — | its peer is the browser page, not a binary |

**The two rootcanals are not the same rootcanal**, which is the finding that
decides the last two rows. Measured with `Read_Local_Supported_Commands`
against both, and confirmed behaviourally:

| | netsim's rootcanal | upstream v1.12.0 |
|---|---|---|
| `HCI_Inquiry`, `Write_Inquiry_Mode` | yes | yes |
| `LE_Periodic_Advertising_Create_Sync` | yes | yes |
| `LE_Create_BIG`, `LE_BIG_Create_Sync` | yes | **no** — `Unknown HCI Command` |

So inquiry came within reach of CI and BIG did not. The auracast pair still
**exits 77**, but the reason is now read off the live controller's own
supported-commands bitmap rather than asserted in a comment — and CI asserts
that they *do* skip, because a BIG script exiting 0 against a controller with
no BIG is a script claiming coverage it does not have.

netsim is still the default for every script and is not replaced.

### Why a controller that answers is not yet a controller

The obvious vehicle for this was `rootcanal-rs`'s `rootcanal-ws`, and it is
deliberately unused. Its `build.rs` resolves the native library three ways and
the third is `c/ffi_stub.c` — and that stub is **not inert**. It answers
*every* command with a well-formed Command Complete carrying status `0x00`. A
probe that sends `Reset` and requires an answer passes against it. So would a
whole interop script that only ever checks an exit status.

`tests/interop/rootcanal_link.py` therefore asserts on the **content** of the
answers, never on their arrival: a real controller owes 6 `BD_ADDR` bytes, 8
version bytes and a 64-byte supported-commands bitmap, where a stub answers 0
return-parameter bytes to all three. `requires()` then gates on named command
bits *inside* that bitmap. Each layer costs real implementation, which is the
property a liveness check does not have. CI runs the probe as its own step
before any script.

**So the remaining oracle gap is now precise:** BIG/broadcast and LE Audio
unicast are the profiles whose only foreign witness is still a manual netsim
run. Inquiry is no longer among them.

### The single highest-leverage change

*Partly done.* Five of the eight scripts now run in CI, which was the cheaper
half of this. The other half stands and is unchanged by it:

**The `tests/interop/*.py` runs that still cannot run in CI already produce
foreign bytes. Capture them as consts and assert against them in-tree** — the
pattern `tests/classic_foreign_bytes_test.rs` and
`tests/avrcp_foreign_bytes_test.rs` already establish. That is the only way
the BAP/BASE, `ext_adv`, `big` and ASCS gaps get a CI-visible oracle, because
the controller that can exercise them is the one CI does not have.

The next-cheapest concrete step is smaller than it looks: **a headless simble
LE-audio sink example**. Bumble's controller *does* model CIG/CIS and ISO data
paths, so `lea_source.py` becomes CI-runnable the moment there is a binary to
point it at instead of the browser page — and that closes ASCS's only foreign
witness.

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
   *One cell now has a foreign witness:* `lea_source.py` asserts that a
   **Bumble** peer reads the ASE back as `Enabling` (0x03) after Enable, per
   ASCS §5.3. One cell is not a matrix, but it is the only cell checked by
   anything that is not simble — and it guards the `bass.rs` bug shape, where
   our code reported a state it was not in and only a foreign reader could
   tell. (netsim path only; see the CI table above.)
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

---

## The same shape again, on hardware — and what to do about it

*Added 2026-08-25, after a day of real-radio work.*

The four bugs above were found in simulation. A day against dongles and a
phone produced five more, and **every one was the same shape**: something
broke and the code's answer was to hang rather than to say so.

- `stty -f <path>` opening a CDC device — blocked in an uninterruptible
  kernel open, holding the device until it was replugged. Three of them
  accumulated before anyone noticed.
- A blocking write to a controller that had stopped draining — parked
  forever, at 0% CPU, with a healthy-looking BIG still advertising.
- ISO SDUs with no credit accounting — 200/s into eight buffers, wedging the
  transport inside a second.
- A peripheral advertising from an address it did not own — a scan that
  stalled for fifteen seconds against a dongle that was working perfectly.
- A write sized to the ATT MTU rather than the controller's buffer pool — a
  stalled bulk endpoint, which reads as a dead transport rather than as
  overflow.

None of these is exotic. Each is a state with no exit, and each cost hours
because the symptom was silence.

### A third test category: Faults

Two exist today — **Asserts** (is it correct) and **Data** (how fast, and
where does the time go). The third asks **what happens when it breaks**, and
it is the one that would have caught all five.

**Dongles make it possible in a way phones never will.** With a dongle we own
the controller; Android owns its own stack and will not let a test sever a
link on cue. That is a real argument for keeping dongles in the fleet even
once phones are the more convenient peer.

The Data benchmark already names its phases, so the parameterisation is
obvious: break during **discover / connect / negotiate / transfer**, and
assert that the failure is *reported*, *named for the phase it happened in*,
and *cleaned up*. Injection points, ordered by what they would teach:

| Injection | Models |
|---|---|
| HCI Disconnect at byte N | the ordinary mid-transfer loss |
| Controller reset mid-stream | a peer that vanishes without disconnecting |
| Stop pumping | a stalled host — the shape that hangs instead of erroring |
| Drop the bridge session | the transport dying under a live link |
| Unplug the dongle | the harshest, and the only one needing a hand |

**The bridge is the natural injection point.** `simble --usb --ws` already
sees every packet in both directions, so "close the socket after N ACL
packets" or "stop forwarding at phase X" is a small feature there, needs no
extra hardware, and is scriptable.

There is a second thing such a suite would catch, observed repeatedly and
never yet tested for: **a failed run leaving the dongle in a state that
poisons the next one.** Several runs during this work only succeeded after an
explicit controller reset, which is a cleanup bug wearing the costume of
flaky hardware.

**Sequencing.** Faults should wait until the Data path actually lands bytes
end to end on real RF. A fault suite needs a working baseline to break;
without one, every failure looks like the injection working.

### A note on the visualisation

The Data category renders each run as a stacked bar on a shared time axis.
The better model is **Perfetto's**: tracks and slices, a zoomable axis, and
detail per slice. Two things follow from adopting it:

- A 6 ms in-page run and a 3 s dongle run become readable in one view, which
  a fixed axis cannot manage.
- Phases can **nest**. `negotiate` is really MTU exchange, then service
  discovery, then subscribe — and when a run stalls, which of those three it
  stalled in is exactly the question being asked.

Worth more than imitation: Perfetto ingests a documented JSON trace format,
so **emitting it** would let a benchmark run open in the real Perfetto UI,
with its zooming, selection and measurement for free. That is a larger idea
than restyling bars and probably the better one.

