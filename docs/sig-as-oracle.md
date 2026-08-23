# The Bluetooth SIG as a machine-checkable oracle

*Investigated 2026-08-23. What the SIG publishes, which of it a script can
consume, and what it is licensed for.*

The premise: every test in this repository has simble on **both ends**, so two
copies of the same misunderstanding always agree. Outside references are the
only thing that can disagree with us — and they have, repeatedly. This is a
survey of which SIG material can serve that role.

---

## Verdict

| Form | Machine-checkable? | Covers |
|---|---|---|
| Core spec **HTML** (per-command "Event(s) generated", transition tables) | **Yes, today, cheaply** | The command-answer bug class |
| **TCRL** test-case catalogue (XLSX) | As an *index*, not an oracle | Which behaviours are worth testing |
| **TS** documents (PDF), **ICS** (PDF), **PTS** | No — prose steps, member tooling | Nothing without hand transcription |

---

## The finding that matters

**The Core Specification is published as browsable HTML, ungated.**

    https://www.bluetooth.com/wp-content/uploads/Files/Specification/HTML/Core_v6.3/out/en/index-en.html

Not a PDF — DocBook-generated HTML with real `<table>` elements. And inside
it, one thing is regular enough to parse mechanically:

> Every HCI command section in Vol 4 Part E ends with the literal string
> `Event(s) generated (unless masked away):`

**321 such blocks in Core 6.0; 319 parse to an answer kind in 6.3.** A ~30-line
script yields `{command → Command Complete | Command Status, follow-up events}`.
Distribution in 6.3: **258 Command Complete, 57 Command Status only, 1
conditional.**

> **Corrected when the script was actually written** (`scripts/check_hci_command_answers.py`):
> **339 opcodes, 278 Command Complete, 61 Command Status only.** The first
> count was per *section*; several sections carry a `[v1]`/`[v2]` summary
> table with two opcodes in it — LE Extended Create Connection, LE Generate
> DHKey, LE Set Extended Advertising Parameters and others — and those are
> separate opcodes a controller must answer separately. Two traps: the
> published HTML breaks long names with soft hyphens (U+00AD), so
> `HCI_Command_­Status` does not match until they are stripped — miss that and
> the table silently halves; and `bluetooth.com` answers urllib's default
> User-Agent with 403.

That single derived table covers the bug class that hit this project **four
times in one week**:

| Bug | Contract it violated |
|---|---|
| `BigReceiver::terminate()` reported `Receiving` forever | §7.8.107: Terminate Sync generates Command Complete — nothing handled it |
| `CsInitiator` hung on a refused command | §7.8.133/§7.8.141: Command Status first, completion subevent only on success |
| `sim.rs` answered an unknown CS config with silence | Every command produces exactly one answer, never none |
| `LE CS Remove Config` sent no completion event | §7.8.138: Command Status **then** LE CS Config Complete, action 0x00 |

The fourth was found *by* this method and fixed in `9d10663`.

### Closed — and what the lint found that nobody predicted

`scripts/check_hci_command_answers.py` exists, and `sim.rs`'s catch-all no
longer answers a Command-Status command with a Command Complete: it consults
`COMMAND_STATUS_OPCODES`, the derived 61-opcode table, and answers anything in
it with a Command Status carrying `UNKNOWN_HCI_COMMAND`. **19 of the 61 now
have real arms; the other 42 get the right shape and no modelled behaviour.**

Three things the estimate above got wrong, all found by running the derivation
rather than reading:

- **61, not 57.** The `[v1]`/`[v2]` opcode pairs were missed.
- **The "17 latent" list was wrong in both directions.** It named LE
  Periodic Advertising Create Sync's neighbours but not the command itself
  (already handled, correctly); it listed LE Read Local P-256 Public Key, LE
  Generate DHKey, LE Subrate Request and LE Read Remote Transmit Power Level
  without noticing that 42 commands — not 17 — had no arm. The estimate was
  built from a hand-scan of names that looked familiar.
- **Nothing was implemented with the wrong event type.** The lint checks every
  explicit match arm, not just the missing ones, and all 43 arms in `sim.rs`
  emitted exactly the kind the spec assigns. The bug was entirely in the
  catch-all — which is the more interesting result, because it means the
  failure mode was never "someone got a command wrong", it was "nobody got the
  *default* right".

The cross-check paid for itself in confidence rather than corrections: Bumble
covers 197 of the 339 commands and **agrees with the scraped table on every
one**. A scrape that agrees with an independently maintained implementation on
197 rows is a scrape worth trusting on the other 142.

---

## Also structured: state transition tables

ASCS §3.2 **Table 3.2** is a literal 21-row table of `(ASE Type, Current State,
ASE Control Operation, Initiating Device, Next State)`, preceded by:

> "ASE state machine transitions that are not shown in Table 3.2 are invalid
> transitions and shall not occur."

Extractable with ~20 lines of regex over the published HTML. Compared against
`src/profiles/ascs.rs` it confirms all seven guards are correct — **and exposes
three gaps**:

- `AseState::Releasing` is declared (`ascs.rs:146`) and **never assigned**. The
  spec's terminal state is unreachable; `on_release` shortcuts to Idle.
- The `Released` operation (Table 3.2's last two rows) is unimplemented.
- Neither link-loss rule exists: CIS loss in Streaming/Disabling → QoS
  Configured; ACL loss in any state → Releasing.

That is "a state with no entrance" — the mirror image of the bugs above.

---

## What is *not* usable

- **ICS** is PDF-only and states feature *presence*, not behaviour. It says
  "supports command X", never "X is answered by Command Status". It has no row
  at all for BIG Terminate Sync or CS Procedure Enable.
- **IXIT** *is* machine-readable XLSX — and is test *fixture parameters*
  (`TSPX_TARGET_LATENCY`, `TSPX_CODEC_ID`), not obligations.
- **TS documents** are freely downloadable (verified: `GATT.TS.p28` 291pp,
  `HCI.TS.p37` 409pp, no login) and regular at the skeleton — Test Purpose /
  Reference / Initial Condition / Test Procedure / Expected Outcome, 245 unique
  case IDs in GATT.TS. But **Expected Outcome is English**, and in HCI.TS it
  branches. Transcribable case by case; not parseable.
- **PTS** requires member registration, Windows, and a dongle. `mmi2grpc`
  carries **no verdicts** — PTS holds them — so it is not an oracle without
  PTS. Its public branch is nearly empty.
- **Avatar** is smaller than its reputation: four test files, largely
  happy-path. An interop harness, not a conformance suite. Overlaps what
  `tests/interop/` already gets from Bumble at far lower cost.
- **MSCs** are vector graphics. Text extraction recovers labels, not arrows.

### The TCRL is a coverage ledger, not an oracle

`TCRL-pkg103.zip` (6.6 MB, ungated) contains `Core.TCRL.p50.xlsx` with one
sheet per layer — CS, GAP, ATT-GATT, HCI, L2CAP, LL, SM — each a table of
`TCID | Description | ... | Category`. No procedures. It names **every
behavioural obligation the SIG tests**, which makes it a to-do list.

Immediately relevant given `src/smp/pairing.rs`: `SM/CEN/JW/BI-01-C` (Just
Works failure), `BI-04-C` (AuthReq RFU), `BI-06-C` and `SM/CEN/PKE/BI-03-C`
(abort when confirms match), `SM/CEN/KDU/BI-01-C`/`BI-04-C` (invalid public
key), `SM/CEN/PKE/BI-02-C` (interrupted passkey). Every one names a failure
path that exists and is untested.

Two adjacent gaps the spec states outright:

- **Vol 3 Part H §3.4's 30-second SMP timer does not exist** in `pairing.rs` —
  no timer of any kind.
- `step()` has **no `self.failed` guard**, so SMP PDUs are still processed
  after Pairing Failed, against §3.4's "No further SMP commands shall be sent…
  A new Pairing process shall only be performed on a new physical link."

---

## Licensing — the workable position

| Artifact | Access | Redistribute? |
|---|---|---|
| Core Spec (PDF/HTML) | Public, no login | **No.** Cite and link. |
| TS / ICS PDFs | Public, no login | **No.** "Bluetooth SIG Proprietary… does not grant any license to any intellectual property" |
| TCRL / IXIT XLSX | Public, no login | Same notice |
| PTS | Member registration | No |
| Bumble, Avatar, Pandora protos, mmi2grpc | Public | Yes, Apache-2.0, with attribution |

**Fetch at check time, compare, never vendor.** This is the position
`scripts/check_sig_assigned_numbers.py` already takes. A command name, "answered
by Command Status", a state name, a transition tuple, a TCID — these are
*facts*, not expression. A script that downloads the HTML, derives a table,
diffs it against the code and prints drift redistributes nothing. Vendoring a
complete transcription of Table 3.2 is a thin-compilation argument better not
had.

Do not copy `mmi2grpc`'s habit of pasting PTS MMI strings into docstrings —
that is Google's risk posture with a SIG membership behind it.

---

## What no oracle will ever cover

The one-octet ATT PDU panic was **not** catchable from SIG material, and the
reason is instructive: Vol 3 Part F §3.3 puts the malformed-PDU duty on the
**server**. The only client-side rule is about *supporting* response PDUs, not
surviving malformed ones. GATT.TS contains **zero** occurrences of "malformed",
"invalid PDU" or "truncated"; the TCRL's ATT-GATT sheet has server-side
unsupported-request cases and nothing client-side.

**Conformance testing assumes a conformant peer.** Robustness against
adversarial input is out of scope for the SIG, permanently — which is the
honest justification for this project's own axis sweeps and fuzz tests.

---

## Ranked, if picking one thing

1. ~~**HCI command→answer lint.**~~ **Done** —
   `scripts/check_hci_command_answers.py`, wired into `ci.yml` beside the
   assigned-numbers check. The caveat predicted here was real: follow-up event
   names in the prose are inconsistent (`LE_CS_­Config_­Complete` with soft
   hyphens, no `HCI_` prefix), so the script derives the *answer kind* — which
   is the part that hangs a host — and leaves follow-up extraction alone. It
   checks three things: the derived table against Bumble, `sim.rs`'s table
   against the derived one, and every explicit match arm's answer kind against
   the spec.
2. **ASCS Table 3.2 transition check.** 21 rows, one HTML table, ~20 lines.
   Cheapest win available.
3. **TCRL as a coverage ledger.** No automation; a to-do list naming exactly
   which behaviours are worth testing. The SM sheet is the immediate payoff.
4. **Hand-transcribe individual TS cases, selectively.** `HCI/BIS/BI-08-C`
   reads almost as pseudocode and is a ready-made regression test for the BIG
   failure modes. Transcribe the *behaviour*, in your own words.
5. **Pandora / Avatar** — skip as a conformance oracle.
6. **PTS / pts-bot** — skip. Worst cost-to-catch ratio on the list.
