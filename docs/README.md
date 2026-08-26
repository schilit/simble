# docs/

What each file is, and — the part that matters — **which contract it is
under**. A *living* document is worthless the moment it is stale. A *decision
record* is allowed to be stale: it records what was decided and why, and is
superseded rather than edited. Mixing the two is how a confident, dated,
detailed document ends up believed after it stopped being true.

Every file below carries that status in its own first lines too, so a reader
who arrives via a link rather than this index still knows which it is.

## Living — must match the tree; a mismatch is a bug

| File | What it is |
|---|---|
| `gaps.md` | What is missing or faked, and where each gap is already declared in code or UI. Re-derivable — it carries the commands to re-derive it. |
| `test-strategy.md` | What the tests here can and cannot prove; where the oracle gaps are. |
| `peripheral-support.md` | What it would take to emulate each peripheral type Android supports, and what is scriptable versus library-only. |
| `api-surface.md` | Which modules are supported API and which are only exposed for inspection, how the `testing` feature keeps `tests/` from forcing the surface open, and the measurement both came from. Its §4–§7 must match `lib.rs` and `ci.yml`; its §1–§3 are a dated, re-derivable snapshot. |

## Reference — describes a format or surface as it is

| File | What it is |
|---|---|
| `scene-format.md` | The scene JSON format. If it disagrees with `src/scene/`, the code is right. |
| `usb-controllers.md` | Running SimBLE on real hardware: choosing a controller, what each tier can prove, flashing an nRF52840, and the Channel Sounding situation. |

## Decision records — point-in-time by design, still useful

These say *why* a choice was made. That reasoning does not expire even when
the code moves, and it is expensive to reconstruct. Read them as history.

| File | Records |
|---|---|
| `sig-as-oracle.md` | What the Bluetooth SIG publishes, which of it a script can consume, and the licensing position. |
| `bdd-evaluation.md` | Whether BDD is worth it: no as a runner, worth trying as an audited specification. |
| `scripting-profile-apis.md` | Spec: profile APIs for scripts in Android's shape. 17 of 20 profiles have no script binding. |
| `sbc-evaluation.md` | SBC options, licensing, and what was built for the A2DP media path. |
| `lc3-evaluation.md` | LC3 options for the wasm demo devices. |
| `rfcomm-comparison.md` | simble vs Bumble vs Zephyr. Carries its own status header: the five gaps it identifies are fixed. |
| `decisions-2026-08-23.md` | Two choices that would otherwise live only in commit messages: why L2CAP dispatch stays keyed on PSM (and why `(psm, cid)` was rejected), and why `tests/`' `run_until` ticks before it checks. |
| `phone-as-backend.md` | The phone as a first-class backend: the script runs *on the device*, not a remote-control client, so it measures without a network in the loop and can be diffed against the simulator. Transport measured against a Pixel 9 Pro. Supersedes `android-rpc-peer.md`. |
| `android-rpc-peer.md` | **Superseded by `phone-as-backend.md`.** Kept for its boundary analysis: what the Android API can and cannot reach — GATT yes, everything below it no — and why the script vocabulary already matches. Its v1/v2 staging and its recommendation are obsolete. |
| `bumble-bridging-evaluation.md` | Whether Bumble can bridge the in-page, netsim and dongle controllers into one ether. It cannot — its cross-process link was deleted upstream and its "L2CAP bridge" is a Bluetooth↔TCP gateway. Records what each layer really offers, why a physical radio cannot be joined by software, and a measured experiment showing rootcanal's phy socket is the facility that does exist. |

## Stale — annotated, kept for the parts that hold

| File | Caveat |
|---|---|
| `HANDOFF-2026-08-22.md` | **Section 3 is false** (it says CIS and LC3 do not exist; both do). Kept for Sections 1, 2 and 5 — what landed, the eight-bug Android pairing chain, and the lessons. Banner at the top of the file says the same. |

## Why nothing has been deleted

Every file here is either current or a record of reasoning. A stale *conclusion*
is worth annotating; a stale *investigation* is worth keeping, because the next
person to ask "why SBC and not something else" should find the answer rather
than repeat the work. The failure mode to avoid is not clutter — it is a
confident, dated, detailed document that a reader believes. That is what the
banner on `HANDOFF-2026-08-22.md` is for.
