# Is BDD worth it here?

Status: evaluation. The specification experiment (Question 2) is proposed, not
built.

**As a test runner, no. As a specification that a tool audits for gaps, probably
yes.** Two questions that share a file format but not a justification.

| Question | Verdict | Basis |
|---|---|---|
| Should `.feature` files *execute*, via cucumber or a Rhai step registry? | **No** | Measured; see below |
| Should `.feature` files exist as a *specification*, audited for missing scenarios? | **Worth trying** | Argued; not yet tested |

## Question 1: Gherkin as a test runner — no

The decisive test is whether it would have caught the bugs. It would not:

| Bug | Gherkin? |
|---|---|
| `BigReceiver::terminate()` left a receiver reporting `Receiving` forever | Same, not better. Mutation-tested: reverting the fix fails the Rust test and the Gherkin scenario identically. Found by a *second consumer* (the web Broadcast page), not a notation. |
| `CsInitiator` ignored Command Status, hanging forever on refusal | No. Rhai cannot express "Command Status" at all. |
| `sim.rs` answered an unknown CS config with silence | No. The test pokes raw bytes off `poll_controller_packet`; a step definition would do the same and be reusable zero times. |
| One-octet ATT PDU panicked a central | No, definitively. Not expressible: 4,352 (opcode, length) pairs, and a Rhai `write()` cannot emit a bare PDU. |

**0 of 4 caught earlier or more reliably, 1 identically, 2 not expressible.** The
three forms behind that verdict:

- **Classic Gherkin + Rust steps.** Measured on a prototype re-expressing one
  25-line test: only the parser and matcher (116 lines) amortise; step
  definitions do not — the interesting assertions (103 across the 21
  device-to-device tests) are one-offs, and four compare against a mid-test
  snapshot Gherkin has no syntax for. The `cucumber` crate adds **84 crates** and
  is async-first against a deliberately synchronous `SceneEngine`.
- **Steps registered in Rhai.** Dissolves the two-language objection but fails on
  capability: `src/scripting/bindings.rs` registers 6 types, all GATT/Android;
  grepping the scripting module for `HciEvent`, `COMMAND_STATUS`, `CsState`,
  `ReceiverState`, `BigReceiver` returns 0 hits; no raw-packet escape hatch. The
  observable vocabulary (`ScriptEvent`) cannot name an HCI opcode, distinguish
  Command Status from Command Complete, or refer to a BIS handle — exactly where
  our bugs live. Reaching that layer means binding 15 types and 33 methods, more
  glue than the Rust option.
- **Neither** — what was built instead: axis sweeps over total functions
  (`tests/malformed_att_test.rs`) and the state×event matrix below.

## Question 2: Gherkin as an audited specification — worth trying

Aimed at a problem the project demonstrably has: every serious bug in the week
before this was a **missing scenario**, not a wrong assertion —

- Nobody wrote "when the receiver leaves the BIG".
- Nobody wrote "when the controller refuses the command".
- Nobody wrote "when the confirm value is wrong" — `PairingSession::fail()` had
  an execution count of zero, and both pairing tests passed with the
  man-in-the-middle defence entirely removed.
- `AseState::Releasing` is declared and never assigned: the spec's terminal ASCS
  state is unreachable.

Each is an assertion nobody thought to write — precisely what gap analysis over a
feature file is for. The capability objection that killed the Rhai option does
not apply: a `.feature` file read by humans and an auditor executes nothing, so it
is not limited by what a binding can observe. It can say "a listener that was
never told the broadcast code refuses to synchronise" — legible to a customer,
auditable for gaps, traceable to a Rust test, with no runner between.

The tool exists outside this repo: **BDD Design Auditor**, an MCP server that
scans `.feature` files and produces a module view, coverage dashboard and AI gap
analysis.

## The shape proposed

**`.feature` files are the specification. Rust and `.rhai` are the execution.
Nothing sits between them but a naming convention.**

- `features/*.feature` — intent, in customer language. No step definitions, no
  runner, no dependency.
- The auditor consumes them for gap analysis.
- Each scenario names the test that covers it and vice versa — a convention,
  checkable by a script.

It does **not** claim the feature file executes, add a cucumber dependency, or
route assertions through a step registry.

**Risks:** *drift* (a spec nothing executes rots — mitigated only by a script
reporting scenarios with no test and tests with no scenario; without it this
becomes decoration, like `scripts/check_sig_assigned_numbers.py` was for a day);
*level confusion* ("When the controller returns Command Status 0x0C" is an
implementation detail in a Gherkin costume, not a scenario); *unfalsifiable
value* (gap analysis is worth it only if it finds a gap not already known — the
experiment must be allowed to fail).

**The experiment.** `features/le-audio-broadcast.feature` describes what shipped,
including the failure scenarios that were missing before they became bugs. Run it
through the auditor: if the gap analysis surfaces something real not already in
the backlog, continue; if it only restates the feature file, stop.

## What was recommended instead

1. **Axis sweeps for total functions.** `tests/malformed_att_test.rs` — every
   opcode × every truncation. Value measured: reintroducing the historical bug
   shape (`att.len() >= 3` → `!att.is_empty()`) is caught by exactly one test
   while all 1,438 others pass. An *internal robustness* tool, not for
   user-authored tests.
2. **A state×event matrix with an exhaustiveness rule**: for every command a host
   sends, one row per answer the controller may give (Command Complete, Command
   Status refusal, completion subevent). The one thing that would have *forced*
   three of the four bugs into the open; the SIG publishes the contract to derive
   it from (`docs/sig-as-oracle.md`).
3. **Keep writing device-to-device tests in the house style** — they found two of
   the four bugs as a side effect.
4. **Extract the scene harness to `tests/common/`** — 111 duplicated lines the
   next e2e file will pay again.

## The user-authoring surface that already exists

`catalog/tests/*.rhai` run in CI *and* in the browser Testing page, and there is
already a behavioural primitive:

    wait_for "characteristic_changed" {
        assert(event.value.len() == 2, "measurement is flags + bpm");
    }

That is a When/Then pair in real Rhai syntax. Two thinnesses have since closed —
`wait_for` is now demonstrated (`catalog/tests/monitor.pass.rhai`, the
`checked_thermostat` entry), and `assert_over`'s temporal window is now on the
script surface (`crate::scripting::monitor`), not MCP-only. The remaining gap is
the **Given**: `catalog::device(name)` covers one device, `catalog/scenes/*.json`
covers topology, and nothing yet connects the two from inside a script. That is a
*composition* problem, not a syntax one — a step registry would not touch it.

This is the reason not to build Gherkin-style step registration:
`assert_over(hrm, uuid, "<", 200, 5.0)` already *is* "the heart rate should stay
below 200 for 5 seconds", machine-checked with the offending sample and timestamp
in the failure message. Step registration on top would add a parser, a registry
and an indirection between the failure and the line that caused it, in exchange
for prose. Filling out the vocabulary — the Given especially — is better value
than any layer above it.
