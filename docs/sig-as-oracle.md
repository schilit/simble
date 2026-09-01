# The Bluetooth SIG as a machine-checkable oracle

What the SIG publishes, which of it a script can consume, and what it is licensed
for. The premise: every test in this repository has simble on **both ends**, so
two copies of the same misunderstanding always agree. Outside references are the
only thing that can disagree — and they have, repeatedly.

## Verdict

| Form | Machine-checkable? | Covers |
|---|---|---|
| Core spec **HTML** (per-command "Event(s) generated", transition tables) | **Yes, today, cheaply** | The command-answer bug class |
| **TCRL** test-case catalogue (XLSX) | As an *index*, not an oracle | Which behaviours are worth testing |
| **TS** docs (PDF), **ICS** (PDF), **PTS** | No — prose steps, member tooling | Nothing without hand transcription |

## The Core spec as HTML

**The Core Specification is published as browsable HTML, ungated** — DocBook
HTML with real `<table>` elements, not a PDF:

    https://www.bluetooth.com/wp-content/uploads/Files/Specification/HTML/Core_v6.3/out/en/index-en.html

One thing is regular enough to parse mechanically: every HCI command section in
Vol 4 Part E ends with `Event(s) generated (unless masked away):`. A ~30-line
script (`scripts/check_hci_command_answers.py`) yields
`{command → Command Complete | Command Status, follow-up events}`. In 6.3: **339
opcodes, 278 Command Complete, 61 Command Status only.** (A naive per-section
count reports fewer; several sections carry a `[v1]`/`[v2]` summary table with two
opcodes each.) Two parsing traps: the HTML breaks long names with soft hyphens
(U+00AD), so `HCI_Command_­Status` does not match until they are stripped — miss
that and the table silently halves; and `bluetooth.com` answers urllib's default
User-Agent with 403.

That derived table covers the bug class that hit this project **four times in one
week**:

| Bug | Contract it violated |
|---|---|
| `BigReceiver::terminate()` reported `Receiving` forever | §7.8.107: Terminate Sync generates Command Complete — nothing handled it |
| `CsInitiator` hung on a refused command | §7.8.133/§7.8.141: Command Status first, completion subevent only on success |
| `sim.rs` answered an unknown CS config with silence | Every command produces exactly one answer, never none |
| `LE CS Remove Config` sent no completion event | §7.8.138: Command Status **then** LE CS Config Complete, action 0x00 |

The fourth was found *by* this method and fixed in `9d10663`. `sim.rs`'s catch-all
now consults `COMMAND_STATUS_OPCODES` (the derived 61-opcode table) and answers
anything in it with a Command Status carrying `UNKNOWN_HCI_COMMAND`; **19 of the
61 have real arms, the other 42 get the right shape and no modelled behaviour.**
Nothing was ever implemented with the wrong event type — all 43 explicit arms
emitted the kind the spec assigns, so the failure was only in the default. The
cross-check paid off in confidence: Bumble covers 197 of the 339 commands and
**agrees with the scraped table on every one**.

## Also structured: state transition tables

ASCS §3.2 **Table 3.2** is a literal 21-row table of `(ASE Type, Current State,
ASE Control Operation, Initiating Device, Next State)`, preceded by "transitions
that are not shown … are invalid transitions and shall not occur." Extractable
with ~20 lines of regex. Compared against `src/profiles/ascs.rs` it confirms all
seven guards and exposes three gaps:

- `AseState::Releasing` is declared (`ascs.rs:146`) and **never assigned** — the
  terminal state is unreachable; `on_release` shortcuts to Idle.
- The `Released` operation (Table 3.2's last two rows) is unimplemented.
- Neither link-loss rule exists: CIS loss in Streaming/Disabling → QoS
  Configured; ACL loss in any state → Releasing.

That is "a state with no entrance" — the mirror image of the bugs above.

## What is *not* usable

- **ICS** — PDF-only; states feature *presence*, not behaviour ("supports command
  X", never "X is answered by Command Status").
- **IXIT** — machine-readable XLSX, but test *fixture parameters*
  (`TSPX_TARGET_LATENCY`), not obligations.
- **TS documents** — freely downloadable (`GATT.TS.p28` 291pp, `HCI.TS.p37`
  409pp, no login), regular at the skeleton (Test Purpose / Reference / Initial
  Condition / Procedure / Expected Outcome, 245 case IDs in GATT.TS) — but
  Expected Outcome is English and in HCI.TS branches. Transcribable case by case,
  not parseable.
- **PTS** — member registration, Windows, a dongle. `mmi2grpc` carries no
  verdicts (PTS holds them), so it is not an oracle without PTS.
- **Avatar** — four test files, largely happy-path; an interop harness, not a
  conformance suite, and overlaps what `tests/interop/` already gets from Bumble.
- **MSCs** — vector graphics; text extraction recovers labels, not arrows.

### The TCRL is a coverage ledger, not an oracle

`TCRL-pkg103.zip` (6.6 MB, ungated) contains `Core.TCRL.p50.xlsx`, one sheet per
layer (CS, GAP, ATT-GATT, HCI, L2CAP, LL, SM), each `TCID | Description | … |
Category`. No procedures — but it names **every behavioural obligation the SIG
tests**, so it is a to-do list. Immediately relevant to `src/smp/pairing.rs`:
`SM/CEN/JW/BI-01-C` (Just Works failure), `BI-04-C` (AuthReq RFU), `BI-06-C` and
`SM/CEN/PKE/BI-03-C` (abort when confirms match), `SM/CEN/KDU/BI-01-C`/`BI-04-C`
(invalid public key), `SM/CEN/PKE/BI-02-C` (interrupted passkey) — each a failure
path that exists and is untested. Two adjacent gaps the spec states outright:

- **Vol 3 Part H §3.4's 30-second SMP timer does not exist** in `pairing.rs`.
- `step()` has **no `self.failed` guard**, so SMP PDUs are processed after
  Pairing Failed, against §3.4.

## Licensing — the workable position

| Artifact | Access | Redistribute? |
|---|---|---|
| Core Spec (PDF/HTML) | Public, no login | **No.** Cite and link. |
| TS / ICS PDFs | Public, no login | **No.** "Bluetooth SIG Proprietary… does not grant any license to any intellectual property" |
| TCRL / IXIT XLSX | Public, no login | Same notice |
| PTS | Member registration | No |
| Bumble, Avatar, Pandora protos, mmi2grpc | Public | Yes, Apache-2.0, with attribution |

**Fetch at check time, compare, never vendor** — the position
`scripts/check_sig_assigned_numbers.py` already takes. A command name, "answered
by Command Status", a state name, a transition tuple, a TCID are *facts*, not
expression; a script that downloads the HTML, derives a table, diffs it and
prints drift redistributes nothing. Do not copy `mmi2grpc`'s habit of pasting PTS
MMI strings into docstrings — that is Google's risk posture with a SIG membership
behind it.

## What no oracle will ever cover

The one-octet ATT PDU panic was **not** catchable from SIG material: Vol 3 Part F
§3.3 puts the malformed-PDU duty on the *server*; the only client-side rule is
about *supporting* response PDUs, not surviving malformed ones. GATT.TS contains
zero occurrences of "malformed", "invalid PDU" or "truncated". **Conformance
testing assumes a conformant peer** — robustness against adversarial input is out
of scope for the SIG, permanently, which is the honest justification for this
project's own axis sweeps and fuzz tests.

## Ranked, if picking one thing

1. ~~**HCI command→answer lint.**~~ **Done** —
   `scripts/check_hci_command_answers.py`, wired into `ci.yml`. Follow-up event
   names in the prose are inconsistent (soft hyphens, no `HCI_` prefix), so the
   script derives the *answer kind* (the part that hangs a host) and leaves
   follow-up extraction alone. It checks the derived table against Bumble,
   `sim.rs`'s table against the derived one, and every explicit arm's answer kind.
2. **ASCS Table 3.2 transition check.** 21 rows, one HTML table, ~20 lines.
   Cheapest win available.
3. **TCRL as a coverage ledger.** No automation; a to-do list. The SM sheet is
   the immediate payoff.
4. **Hand-transcribe individual TS cases, selectively.** `HCI/BIS/BI-08-C` reads
   almost as pseudocode — a ready-made regression test for the BIG failure modes.
   Transcribe the *behaviour*, in your own words.
5. **Pandora / Avatar** — skip as a conformance oracle.
6. **PTS / pts-bot** — skip. Worst cost-to-catch ratio on the list.
