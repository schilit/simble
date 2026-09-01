# Two decisions

## 1. L2CAP handler dispatch stays keyed on PSM; channel identity is passed through

**Context.** `ClassicHost::handle_channel_data` resolved a handler with
`self.handlers.iter_mut().find(|h| h.psm() == psm)` — one PSM, one handler. Two
profiles broke that assumption at once:

- **Classic HID** needs *two* PSMs distinguished: Control `0x0011` and Interrupt
  `0x0013`.
- **A2DP/AVDTP** is the mirror image: *one* PSM (`0x0019`) carrying two channels
  with different roles — signalling, then media transport.

**Decision.** Handler lookup stays keyed on PSM. `ProtocolHandler` gained five
**defaulted** methods so channel identity reaches the handler that needs it:
`psms()`, `on_channel_data(HandlerChannel, ..)`, `poll_channel_output(..)`,
`on_channel_open(..)`/`on_channel_lost(..)`, and `poll_channel_requests()`.

**Rejected: keying the host's table on `(psm, cid)`.** It fails at exactly the
moment it would have to work. When a second `0x0019` connection request
arrives, the **host** has no way to know what role that channel plays. Only the
profile knows, and only because an AVDTP `OPEN` just succeeded. Routing
decisions belong where the knowledge is.

**Consequences.**
- All three pre-existing handlers (`SdpHandler`, `RfcommHandler`,
  `SdpQueryHandler`) needed **zero edits** — the defaults preserve them, and 17+
  tests plus the Car page kept working untouched.
- A multi-channel handler must key its own per-channel state on CID, and
  `poll_channel_output` is called once *per channel*, so it must answer for the
  channel it was asked about and no other. That is real bookkeeping pushed into
  the handler (`A2dpSource` keeps a map from CID to queued SDUs).
- One behaviour change fell out: `on_channel_closed()` now fires only when a
  handler's **last** channel goes. Previously any channel closing on that PSM
  ended the session — which would have made an AVDTP media channel closing kill
  the signalling session.

---

## 2. `run_until` in `tests/` ticks first, then checks

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
