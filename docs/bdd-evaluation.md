# Is BDD worth it here?

Status: evaluation. The specification experiment (§Question 2) is proposed,
not built.

**As a test runner, no. As a specification that a tool audits for gaps,
probably yes.** These are two questions that happen to share a file format;
they do not share a justification.

| Question | Verdict | Basis |
|---|---|---|
| Should `.feature` files *execute*, via cucumber or a Rhai step registry? | **No** | Measured; see below |
| Should `.feature` files exist as a *specification*, audited for missing scenarios? | **Worth trying** | Argued; not yet tested |

---

## Question 1: Gherkin as a test runner — no

Three forms were considered.

### (a) Classic Gherkin with Rust step definitions

Measured on a prototype that re-expressed one existing 25-line test:

| Part | Lines |
|---|---|
| feature file | 16 |
| duplicated scene harness | 111 |
| feature parser | 42 |
| step registry + matcher | 74 |
| step definitions (9 steps) | ~140 |

Only the parser and matcher (116 lines) amortise. Step definitions do not:
there are **103 assertions across the 21 device-to-device tests**, and the
interesting ones are one-offs — a float window sweep over RAS wire bytes, a
handle-disjointness cross product, `base_bytes() == periodic_advertising_data()[4..]`.
Four of the 21 compare against a snapshot taken mid-test, which Gherkin has no
syntax for.

Using the `cucumber` crate instead: **84 new crates**, roughly doubling a
dependency graph whose only dev-dependency was `zerocopy`. It is also
async-first against a `SceneEngine` that is deliberately synchronous with no
sleeps.

Two ergonomic losses showed up immediately in the prototype: panics report the
step-definition file and line rather than the feature file, and the whole
feature is one `#[test]`, so a single scenario cannot be run or filtered by
name without another dependency.

### (b) Step definitions registered in Rhai

    given("a peripheral named %s", |name| { ... });
    when("a %d happens", |n| { ... });

This dissolves the two-language objection cleanly — steps would be in the same
language as the devices, on the same engine, exercised by the same CLI and the
same web Testing page, and an agent could emit scenario *and* steps together
with no recompilation.

It fails on **capability**:

- `src/scripting/bindings.rs` registers **6 types**, all GATT/Android shapes.
- Grepping the whole scripting module for `HciEvent`, `COMMAND_STATUS`,
  `CsState`, `ReceiverState`, `BigReceiver` returns **0 hits**.
- There is no raw-packet escape hatch.

The observable vocabulary is `ScriptEvent`: `service_added`, `connected`,
`disconnected`, `characteristic_read/write`, `descriptor_read/write`,
`mtu_changed`, `notification_sent`, `services_discovered`,
`subscription_changed`, `characteristic_changed`, `operation_failed`.

Nothing there can name an HCI opcode, distinguish Command Status from Command
Complete, or refer to a BIS handle. Reaching the right layer would mean
binding what the e2e tests touch — **15 types and 33 methods** — which is
*more* Rust glue than (a), in a dynamically typed language, for tests whose
content is byte layouts and enum states.

### (c) Neither

What was recommended and what was built instead: axis sweeps over total
functions (`tests/malformed_att_test.rs`), and the state×event matrix
described below.

### The decisive test: would it have caught the bugs?

| Bug | Gherkin? |
|---|---|
| `BigReceiver::terminate()` left a receiver reporting `Receiving` forever | Same, not better. Mutation-tested: reverting the fix fails the Rust test and the Gherkin scenario **identically**. Found by a *second consumer* (the web Broadcast page), not by a notation. |
| `CsInitiator` ignored Command Status, hanging forever on refusal | No. Rhai cannot express "Command Status" at all. |
| `sim.rs` answered an unknown CS config with silence | No. The test pokes raw bytes off `poll_controller_packet`; a step definition would do the same byte-poking and be reusable zero times. |
| One-octet ATT PDU panicked a central | No, definitively. Not expressible: 4 352 (opcode, length) pairs, and a Rhai `write()` goes through the typed client and cannot emit a bare PDU. |

**0 of 4 caught earlier or more reliably. 1 of 4 caught identically. 2 of 4
not expressible.**

---

## Question 2: Gherkin as an audited specification — worth trying

This is aimed at a problem the project demonstrably has: every serious bug in
the week before this was written was a **missing scenario**:

- Nobody wrote *"when the receiver leaves the BIG"*.
- Nobody wrote *"when the controller refuses the command"*.
- Nobody wrote *"when the confirm value is wrong"* — `PairingSession::fail()`
  had an execution count of **zero**, and both end-to-end pairing tests passed
  with the man-in-the-middle defence entirely removed.
- `AseState::Releasing` is declared and never assigned: the spec's terminal
  ASCS state is unreachable.

None of these is a wrong assertion. Each is an assertion nobody thought to
write. That is precisely what a gap analysis over a feature file is for: a
`Feature: LE Audio Broadcast` containing only happy-path scenarios invites the
question *"where is termination, refusal, loss?"* — asked by a tool, before
the bug rather than after.

The relevant tool already exists outside this repo: **BDD Design Auditor**, an
MCP server that scans `.feature` files and produces a module view, a coverage
dashboard and an AI gap analysis.

### Why the capability objection does not apply here

It killed (b) because Rhai steps cannot see HCI sequencing — which is where
*our* bugs live. But a `.feature` file read by humans and an auditor is not
executing anything, so it is not limited by what any binding can observe. It
can say:

    Scenario: A listener that was never told the broadcast code refuses to join
      Given an encrypted broadcast source
      When a listener with no broadcast code discovers it
      Then it must refuse to synchronise
      And it must not receive audio

That is legible to a customer, auditable for gaps, and traceable to a Rust
test — without a runner in between.

---

## The shape proposed

**`.feature` files are the specification. Rust and `.rhai` are the execution.
Nothing sits between them but a naming convention.**

- `features/*.feature` — intent, in customer language. No step definitions, no
  runner, no dependency.
- The auditor consumes them for gap analysis.
- Each scenario names the test that covers it; each test names its scenario.
  A convention, checkable by a script, not a framework.

What this deliberately does **not** do: claim the feature file executes, add a
cucumber dependency, or route assertions through a step registry.

### Risks worth stating

- **Drift.** A specification nothing executes will rot. The mitigation is the
  same convention: a script that reports scenarios with no matching test and
  tests with no matching scenario. If that check is not built, this becomes
  decoration — the same failure as `scripts/check_sig_assigned_numbers.py`,
  which was written and then referenced by nothing for a day.
- **Level confusion.** Scenarios should stay at the level a user cares about.
  "When the controller returns Command Status 0x0C" is not a scenario, it is
  an implementation detail with a Gherkin costume on.
- **Unfalsifiable value.** Gap analysis is only worth it if it finds a gap we
  did not already know about. That is the experiment below, and it should be
  allowed to fail.

---

## The experiment

`features/le-audio-broadcast.feature` is a first specification, written to
describe **what shipped**, including the failure scenarios that were missing
before they became bugs.

Run it through the auditor. If the gap analysis surfaces something real that is
not already in the backlog, this is worth continuing. If it only restates what
the feature file says, it is not.

---

## What was recommended instead

1. **Axis sweeps for total functions.** `tests/malformed_att_test.rs` is the
   pattern — every opcode × every truncation, rather than five hand-picked
   samples. Its value was measured, not asserted: reintroducing the historical
   bug shape (`att.len() >= 3` → `!att.is_empty()`) is caught by exactly one
   test in the repository while all 1 438 others pass. This is an *internal
   robustness* tool and does nothing for user-authored tests — a distinction
   worth keeping straight.
2. **A state×event matrix with an exhaustiveness rule**: for every command a
   host sends, one row per answer the controller may give — Command Complete,
   Command Status refusal, and the completion subevent. This is the one thing
   that would have *forced* three of the four bugs into the open, and the SIG
   publishes the contract to derive it from (see `docs/sig-as-oracle.md`).
3. **Keep writing device-to-device tests in the existing house style.** They
   found two of the four bugs as a side effect of being written.
4. **Extract the scene harness to `tests/common/`** — 111 duplicated lines
   that the next e2e file will pay again, independent of any of this.

## The user-authoring surface that already exists

Worth knowing before building anything: `catalog/tests/*.rhai` run in CI *and*
in the browser Testing page, and there is already a behavioural primitive:

    wait_for "characteristic_changed" {
        assert(event.value.len() == 2, "measurement is flags + bpm");
    }

That is a When/Then pair in real Rhai syntax. Three concrete thinnesses:

- ~~`wait_for` appears in **zero** of the three shipped example tests~~ — a
  primitive nobody has seen demonstrated may as well not exist. *Closed:
  `catalog/tests/monitor.pass.rhai` and the `checked_thermostat` catalog entry
  both use it.*
- ~~`assert_over`, the temporal assertion, is **MCP-only**.~~ A script author
  can say "this happened" but not "this stayed true". *Closed:
  `crate::scripting::monitor` puts the same window-and-operator semantics on
  the script surface.*
- There is no Given. Setup is imperative, though `catalog/scenes/*.json` is
  already the declarative half and could serve as one. *Half-closed:
  `catalog::device("hrm")` is a one-line Given for a single device — the
  declarative topology is still only in the scene files.*

Filling out that vocabulary is likely better value than any layer above it.

### What the filled-out vocabulary means for step registration

The two primitives now on the script surface are the reason not to build
Gherkin-style step registration (`when('a %d happens', { ... })`). A BDD layer
earns its keep by supplying a *vocabulary* of reusable temporal steps on top of
a framework that only knows about single moments — that is what "Then the heart
rate should stay below 200 for 5 seconds" buys. Here
`assert_over(hrm, uuid, "<", 200, 5.0)` already *is* that sentence,
machine-checked, with the offending sample and its timestamp in the failure
message, and `wait_for "..." { ... }` is already a When/Then pair in real
syntax rather than a regex over English. Step registration on top would add a
parser, a registry and an indirection between the failure and the line that
caused it, in exchange for prose.

The gap that remains is the Given: `catalog::device(name)` covers one device by
name, `catalog/scenes/*.json` covers topology, and nothing yet connects the two
from inside a script. That is a *composition* problem, not a syntax one, and a
step registry would not touch it.
