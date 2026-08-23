# docs/

What each file is, and whether it describes the present.

## Current — read these as true

| File | What it is |
|---|---|
| `scene-format.md` | The scene JSON format. Reference. |
| `test-strategy.md` | What the tests here can and cannot prove; where the oracle gaps are. |
| `sig-as-oracle.md` | What the Bluetooth SIG publishes, which of it a script can consume, and the licensing position. |
| `bdd-evaluation.md` | Whether BDD is worth it: no as a runner, worth trying as an audited specification. |
| `gaps.md` | What is missing or faked, and where each gap is already declared in code or UI. Re-derivable — it carries the commands. |

## Decision records — point-in-time by design, still useful

These say *why* a choice was made. That reasoning does not expire even when
the code moves, and it is expensive to reconstruct. Read them as history.

| File | Records |
|---|---|
| `sbc-evaluation.md` | SBC options, licensing, and what was built for the A2DP media path. |
| `lc3-evaluation.md` | LC3 options for the wasm demo devices. |
| `rfcomm-comparison.md` | simble vs Bumble vs Zephyr. Carries its own status header: the five gaps it identifies are fixed. |
| `peripheral-support.md` | What it would take to emulate each peripheral type Android supports. |

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
