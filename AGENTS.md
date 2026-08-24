# AGENTS.md — Guide for coding agents working on Simble

Simble is a zero-copy, memory-safe virtual Bluetooth (BLE + Classic) host stack
and device simulation engine in pure Rust. Its primary client is Android's
netsim. It is inspired by Google's Python [Bumble](https://github.com/google/bumble),
credited in README.md's Acknowledgments. Where a test or module is genuinely
derived from Bumble's, saying so where it is derived is useful: it tells the
next reader which foreign implementation to diff against when the two disagree.

## Module conventions

- Flat `pub mod` tree in `src/lib.rs` with hand-picked re-exports.
  Never `pub use module::*`.
- **Packet layer** (`src/packets/`, packet definitions elsewhere):
  - Structs are `#[repr(C)]` with the full six zerocopy derives:
    `FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout` plus
    `Copy, Clone, Debug, PartialEq, Eq`.
  - Parsing shape: `parse(bytes) -> Option<(Ref<...>, &[u8])>` via
    `Ref::from_prefix`.
  - Opcode/constant tables live in nested `pub mod` blocks.
- **Error handling**: `Option`-based parsing at the packet layer;
  `SimbleError` (thiserror) only at the device/manager layers.
- **GATT profiles**: structs of `u16` handles with
  `register(db: &mut GattDatabase, ...) -> Self`.
- **State machines**: plain synchronous structs with explicit state enums and
  `receive(...) -> (outgoing, events)` shapes.
  **No async anywhere. No tokio.**

## The MCP surface

`src/mcp.rs` is SimBLE's agent-first frontend (`simble mcp`, JSON-RPC over
stdio), alongside the web (wasm) and native (library + CLI) ones. Two
consequences for changes here:

- An MCP client launches the **release** binary, so `cargo build --release`
  after touching tools — a stale binary silently serves the old tool list.
- Tool descriptions and the `EXAMPLES` table are user-facing documentation:
  an agent reads them instead of this repo. Keep them accurate, and keep the
  tool list in `README.md`, `HANDOFF.md` and the `mcp` module doc in sync.

## Layering rule

Host-stack code (`src/classic/`, `src/gatt/`, `src/smp/`, `src/l2cap/`,
`src/profiles/`) and controller-layer code (`src/controller/` — e.g. LMP) are
deliberately separate. Before placing new work, check which layer it belongs
to.

## Comments

- No WHAT comments — code says what it does.
- Only non-obvious WHY, with Bluetooth spec citations (Vol/Part/Section) where
  relevant.

## Dependencies

Keep near-zero. Currently: `serde`, `serde_json`, `thiserror`, `zerocopy`,
`rhai` — all pure Rust.

- New dependencies need explicit justification.
- No C/FFI-backed crates. No async runtimes.

## Testing

- Integration tests in `tests/*.rs`, against the public API only.
- Do **not** create a `tests/mod.rs`. Cargo compiles every `tests/*.rs` as its
  own test binary already; declaring them as modules builds a second binary
  that re-runs them all. One existed and was deleted for exactly that reason —
  see `docs/test-strategy.md`.
- Each file starts with a module doc comment describing what it covers
  functionally.
- A test belongs in exactly one place. Inline `#[cfg(test)]` is for private
  internals; anything reachable through the public API goes in `tests/`. Do not
  keep a copy in both — the copies drift, and the inline one has historically
  been the weaker.
- Unit tests live in a **sibling file**, not inline. `foo.rs` ends with

  ```rust
  #[cfg(test)]
  #[path = "foo_tests.rs"]
  mod tests;
  ```

  and the bodies go in `foo_tests.rs` next to it. They are still compiled as
  part of the module, so `super::*` and private access work unchanged. Name it
  `<module>_tests.rs`, or `<module>_<modname>.rs` when a file has more than one
  test module. Keeping them inline makes `cargo llvm-cov` count test bodies as
  production lines — see `docs/test-strategy.md`.

## Verification loop (every change must pass)

```sh
cargo build --all-targets --all-features    # zero warnings
cargo test --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
cargo dupes report --exclude-tests          # see DRY tooling below
```

## DRY tooling

Run at the end of any multi-file work:

```sh
cargo dupes report --exclude-tests   # install: cargo install cargo-dupes
similarity-rs ./src --skip-test      # install: cargo install similarity-rs
```

- Fix real duplicates, or `cargo dupes ignore` with a documented reason.
- Structurally-identical-but-semantically-distinct protocol code
  (per-opcode packet types, per-state enums) is expected and fine.

## Concurrent-agent etiquette

This repo is often worked by several agents at once:

- Stay strictly within your assigned files.
- Module declarations (`mod.rs`, `lib.rs`) are wired by the coordinator, not by
  task agents. `tests/*.rs` needs no wiring at all.
- Expect transiently broken full-crate builds from concurrent work; verify
  with targeted `cargo build --lib` / `cargo test --test <name>` instead of
  full-crate commands.
