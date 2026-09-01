# The public API boundary

Which modules are supported API, which are exposed only so the crate's own tests
can reach them, and how that boundary is kept from drifting. **Authoritative
source: `lib.rs`** — if it and this file disagree, that is a bug.

The problem this solves: `tests/` is a separate crate, so much of the deep
wire-format surface had to be `pub` purely for the tests to name it — and Rust
cannot spell *"visible to this crate's own tests, but not to the world."* Without
a boundary, ~76% of the public surface was plumbing no real consumer had ever
touched.

## Supported vs. inspection-only

**Supported API** (each is imported by an example, a binary over the external
`simble::` path, or is the scripting layer's public surface): `device`, `devices`,
`scene`, `scripting`, `types`, `transport`, `api`, `service`, `client`, `gatt`,
`profiles`, `android`, `classic`, `controller`, `cs`, `mcp`.

**Exposed for inspection, no stability promise** — `pub(crate)` unless the
`testing` feature is on: `packets`, `att`, `l2cap`, `gap`, `smp`, `crypto`, `df`,
`audio`, `obex`. No example, binary, or script names any of them; only `tests/`
does.

The handful of plumbing types a consumer genuinely needs are re-exported at the
crate root, and the root spelling is the supported one: `AdvertisingData`,
`AclReassembler`, `CoCChannel`, `CoCManager`, `KeyStore`, `PairingConfig`,
`PairingKey`, `PairingKeys`, `PairingSession`, `SmpRole`. Re-exporting a public
item out of a private module is how the facade works.

## How the boundary is enforced

Rust cannot `cfg` a visibility, so the inspection-only modules are declared with a
macro in `lib.rs` that flips `pub` ↔ `pub(crate)` on the `testing` feature — at the
`mod` line, not per item:

```rust
macro_rules! plumbing_mod {
    ($name:ident) => {
        #[cfg(feature = "testing")]
        pub mod $name;
        #[cfg(not(feature = "testing"))]
        #[allow(unused_imports, dead_code)]
        pub(crate) mod $name;
    };
}
```

The two `allow`s on the closed arm restore the status quo: making a module
crate-private wakes `dead_code`/`unused_imports` on wire-format types that only
`tests/` uses, from a crate rustc cannot see. The open arm keeps both lints.

`testing` is turned on automatically for the crate's own tests, examples, and
benches — but never for a downstream consumer — via the self-dev-dependency idiom
(Cargo unifies a path dep's features with the package's own lib target):

```toml
[dev-dependencies]
simble = { path = ".", features = ["testing"] }
```

Cargo strips path-only dev-dependencies when publishing, so this does not leak into
the published crate.

## Proof it stays drawn

`cargo test` and CI's `--all-features` steps all compile the surface wide open, so
none of them would notice the closed surface breaking. CI has its own step,
**before** anything with `--all-features`:

```yaml
- name: Build and lint the CLOSED public surface (no features)
  run: |
    cargo build --lib --no-default-features
    cargo clippy --lib --no-default-features -- -D warnings
```

A downstream crate with `[dependencies] simble` and no features sees exactly the
supported surface — `simble::types::Address` and the crate-root re-exports resolve;
`simble::packets::…` is `error[E0603]: module 'packets' is private`.

One gap: `cargo doc --no-default-features` reports broken intra-doc links from
supported modules into the now-private ones (e.g. `classic/a2dp.rs` →
`crate::audio::sbc`). CI's doc step is `--all-features`, so it is not red today;
making the closed doc build a gate means converting those links to plain code
spans first.

## `#[non_exhaustive]` on spec-discriminant enums

The 14 enums whose discriminants are assigned by the Bluetooth SIG (so a future
value is the spec's to add, not ours) carry `#[non_exhaustive]`, which forces an
external `match` to keep a wildcard arm:

| file | enums |
|---|---|
| `profiles/bap.rs` | `SamplingFrequency`, `FrameDuration`, `AnnouncementType` |
| `profiles/aics.rs` | `Mute`, `GainMode`, `AudioInputStatus`, `AudioInputType` |
| `profiles/bass.rs` | `PeriodicAdvertisingSyncParams`, `PeriodicAdvertisingSyncState`, `BigEncryption` |
| `profiles/mcp.rs` | `MediaState`, `MediaControlPointOpcode` |
| `profiles/ascs.rs` | `AseState` |
| `types/hci_types.rs` | `AddressType` |

The other 114 public enums are our own state machines, where `#[non_exhaustive]`
would be ceremony.

## Open: the unknown-wire-value policy

Not settled. The tree has four answers for what happens to a wire value the SIG has
defined but the code does not yet know:

| approach | where | unknown value `0x07` becomes |
|---|---|---|
| `Option`, `None` on unknown | `bap.rs`, `bass.rs` `from_u8` | **destroyed** — nothing can echo or log it |
| bare `_ =>` | `ascs.rs` | **swallowed silently** — caller not even told |
| newtype + `Display` fallback | `hci_types.rs` (`UNKNOWN (0x07)`) | **preserved**, but a `match` loses exhaustiveness |
| enum with `Unknown(u8)` | `ascs_client.rs` `AseState` | **preserved**, and `match` stays exhaustive-checked |

The case for preserving: the most expensive bugs in this project's history were all
*"we lied about what the peer said"* (the CSIS RSI byte order, the `bass.rs` sync
state, the invented Ranging Service UUIDs). `Option` and bare `_` discard the
evidence at the parse boundary. The `Unknown(u8)` shape is the one that preserves
the value and composes with `#[non_exhaustive]`. Decide the policy, then convert —
not the other way round.
