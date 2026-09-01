# `run_until` in tests ticks first, then checks

**Context.** Seven definitions of `run_until` existed across the tree. They were
not seven drifted copies of one function — they were **two incompatible loop
semantics sharing a name**, and the split ran exactly along the `src/`–`tests/`
line.

*Shape A, all four `src/` copies* — check, then tick:

```rust
for _ in 0..steps { if done(self) { return true; } self.tick(); }
done(self)          // one extra evaluation after the loop
```

*Shape B, all three `tests/` copies* — tick, then check:

```rust
for _ in 0..ticks { self.tick(); if done(self) { return true; } }
false               // never re-evaluated
```

**Decision.** `tests/common/mod.rs` provides **Shape B**.

**Why.** Shape A evaluates its predicate at t=0, so it can return `true` having
never ticked. Adopting it would let three protocol-ladder e2e suites pass with
**zero ticks** whenever a predicate happened to hold at entry — a false pass in
tests whose entire purpose is driving a protocol forward.

**Consequences, including the cost.** Shape B's `false` genuinely conflates
"budget exhausted" with "condition never true", where Shape A's trailing
`done(self)` at least reports the final state. That ambiguity is the price, and
it is paid down two ways: `#[must_use]`, and a doc comment stating that `false`
means budget exhausted. Three call sites were silently discarding the return
value; one of them (`classic_security_test.rs:377`) was discarding a real
predicate that had never been checked.

**Not adopted, and worth revisiting.** `wasm_ws.rs`'s copy returns `usize` —
ticks consumed — explicitly so "a test can assert progress rather than merely
eventual success". That is the strongest contract of the seven: it distinguishes
"true at tick 3" from "true at tick 39", which no `bool` version can. Adopting
it would rewrite every `assert!` call site, so it was left alone.

**Also deliberately not shared: `tick` itself.** `classic_security` runs
start → `link.tick()` → pump (the medium moves *before* the hosts answer);
`broadcast_e2e` and `ranging_e2e` pump all hosts → `link.tick()` (the medium
moves *after*). That ordering is a real property of each scene, and collapsing
it would change what those tests exercise. Only the loop is shared.
