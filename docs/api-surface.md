# The public API surface: measurement, boundary, and how it stays drawn

Authoritative source for §4–§7: `lib.rs` — if it and this file disagree, that is
a bug. §1–§3 are a dated measurement (re-derivable via the §1 commands); §8 is a
decision record for a decision not yet made.

`gaps.md` §8 said no boundary had ever been drawn and that the obstacle was
`tests/`: much of the deep surface is `pub` because the integration tests need
it, and Rust cannot spell *"visible to this crate's own tests, not to the
world."* This document is the measurement of where the boundary belongs and the
record of the mechanism that holds it.

---

## 1. How this was measured

Two independent instruments.

**The surface itself** comes from rustdoc's JSON output
(`cargo +nightly rustdoc --lib -- -Z unstable-options --output-format json`),
walked from the crate root through public modules and re-exports — the set a
downstream `use simble::…` can name, which is not `grep -c '^ *pub'` (a `pub`
item inside a private module is not public; a re-export makes one item reachable
by two paths). It counts **7 486 reachable items**, against §8's grep-derived
~3 500; the difference is rustdoc also counts fields, variants, associated
constants and trait-impl methods, all equally part of what a `match` or struct
literal can break on.

**"Nothing uses this"** is rustc's own `dead_code` lint, run against a build with
every module forced private, twice: `cargo check --lib` and `cargo test --lib
--no-run`. An item flagged in both is referenced by neither the library nor its
own unit tests.

**Attribution to `examples/` vs `tests/` vs `src/scripting/`** is a per-file
identifier scan of each consumer. For anything hanging off a type, the *owner
type name* must appear in the same consumer file (which stops `new`, `read`,
`len` matching everything). The bias is toward over-attribution: wrongly calling
an item *used* costs nothing, wrongly calling it *unused* would cut something
load-bearing.

---

## 2. The four-way inventory

All 7 486 reachable items, one bucket each, first match wins in the order shown.

| Bucket | Items | What it means |
|---|---:|---|
| used by `examples/` | 469 | the closest proxy for a real consumer |
| used by `src/scripting/` or the `.rhai`/`web` surface | 504 | reachable from a script |
| used by `src/bin/` (the CLI, over the external `simble::` path) | 91 | a consumer that links the crate from outside |
| **used only by `tests/`** | **2 360** | the reason the surface was this wide |
| used only inside `src/` | 3 364 | never named outside the crate at all |
| **used by nothing anywhere** | **698** | dead |

**2 360 items — 32% of the surface — are public only because `tests/` is a
separate crate.** Another 3 364 are not named outside `src/` even once. Together
that is **76% of the public API that no consumer, real or simulated, has ever
touched.**

### By module (before the change)

| module | examples | scripting | bins | tests-only | internal-only | nothing | total |
|---|---:|---:|---:|---:|---:|---:|---:|
| classic | 144 | 28 | 6 | 857 | 432 | 283 | 1750 |
| profiles | 26 | 199 | 2 | 520 | 488 | 88 | 1323 |
| device | 161 | 85 | 7 | 207 | 441 | 98 | 999 |
| packets | 12 | 3 | 0 | 273 | 510 | 49 | 847 |
| att | 2 | 0 | 0 | 44 | 229 | 9 | 284 |
| obex | 21 | 6 | 0 | 47 | 156 | 14 | 244 |
| transport | 32 | 10 | 33 | 13 | 109 | 5 | 202 |
| devices | 14 | 8 | 12 | 12 | 106 | 41 | 193 |
| df | 1 | 1 | 0 | 81 | 91 | 8 | 182 |
| l2cap | 2 | 1 | 0 | 55 | 106 | 14 | 178 |
| types | 14 | 15 | 3 | 11 | 71 | 52 | 166 |
| controller | 9 | 6 | 0 | 45 | 97 | 2 | 159 |
| scene | 5 | 8 | 20 | 3 | 110 | 1 | 147 |
| cs | 2 | 5 | 1 | 2 | 121 | 3 | 134 |
| scripting | 16 | 4 | 2 | 10 | 69 | 7 | 108 |
| android | 1 | 88 | 1 | 0 | 10 | 5 | 105 |
| smp | 0 | 1 | 0 | 54 | 34 | 2 | 91 |
| gatt | 1 | 17 | 4 | 28 | 31 | 3 | 84 |
| api | 0 | 1 | 0 | 15 | 52 | 3 | 71 |
| gap | 0 | 8 | 0 | 31 | 21 | 4 | 64 |
| audio | 2 | 0 | 0 | 23 | 21 | 3 | 49 |
| client | 1 | 3 | 0 | 11 | 28 | 2 | 45 |
| crypto | 1 | 2 | 0 | 9 | 14 | 0 | 26 |
| service | 1 | 1 | 0 | 9 | 15 | 0 | 26 |
| mcp | 0 | 4 | 0 | 0 | 2 | 2 | 8 |

The `examples` column is the token heuristic and reads high; the **import
statements** are the hard number. Across all 15 example programs there are **58
distinct `use simble::…` paths, from exactly 8 modules**:

| module | import paths |
|---|---:|
| `device` | 31 |
| `transport` | 31 |
| `classic` | 21 |
| `types` | 7 |
| `devices` | 2 |
| `cs` | 1 |
| `controller` | 1 |
| `scripting` | 1 |

`tests/` imports **516 distinct paths across 28 module-level names**. The modules
`tests/` reaches that no example ever does: `android`, `api`, `att`, `audio`,
`crypto`, `df`, `gap`, `gatt`, `l2cap`, `mcp`, `obex`, `packets`, `profiles`,
`service`, `smp`.

---

## 3. The dead bucket, and why most of it should not be deleted

Compiler-confirmed dead in-crate: **1 699 items**, of which **1 427 are publicly
reachable** and 272 are already `pub(crate)`. Cross-referencing:

| | items |
|---|---:|
| public, dead in-crate, and **named nowhere in the tree** | **573** |
| public, dead in-crate, reached only from `tests/` | 463 |
| public, dead in-crate, reached from `examples/` | 228 |
| public, dead in-crate, reached from scripting / `.rhai` / `web` / docs | 156 |
| public, dead in-crate, reached from `src/bin/` | 7 |

573 is the strict floor (any name collision anywhere disqualifies an item); the
owner-aware count in §2 puts it at 698. Call it **~600 items across 65 files.**

**`GapDataType` was the sample, and it is gone.** 63 lines in
`types/hci_types.rs`: a `#[repr(transparent)]` newtype, 15 associated constants
and a `Display` impl naming all fifteen AD types — 17 hand-written items, 21
counting the derives, with zero references anywhere. A third copy of the AD-type
table, and being unused it had drifted: missing `0x2E` and `0x30` while the live
table in `gap::advertising::ad_type` had them.

**The rest is not `GapDataType`, and the shape of the list is the argument.**

| file | dead items | first few |
|---|---:|---|
| `src/classic/avrcp.rs` | 134 | `LIST_PLAYER_APPLICATION_SETTING_ATTRIBUTES`, `SET_BROWSED_PLAYER`, `GET_FOLDER_ITEMS`, `CHANGE_PATH`, … |
| `src/classic/avc.rs` | 72 | `MONITOR`, `PRINTER`, `TUNER`, `CAMERA`, `VENDOR_UNIQUE`, `PLUG_INFO`, … |
| `src/types/hci_types.rs` | 42 | `AdvertisingType`, `OwnAddressType`, `PeerAddressType`, `RESOLVABLE_OR_RANDOM_ADDRESS`, … |
| `src/devices/helpers/hid_reports.rs` | 38 | `LCTRL`, `LALT`, `LMETA`, `KEY_D`…`KEY_Z` |
| `src/classic/avdtp.rs` | 28 | `BAD_HEADER_FORMAT`, `BAD_LENGTH`, `SEP_NOT_IN_USE`, `BAD_RECOVERY_TYPE`, … |
| `src/profiles/ascs.rs` | 17 | `UNSUPPORTED_METADATA`, `REJECTED_METADATA`, `INVALID_METADATA`, … |
| `src/profiles/bap.rs` | 17 | `FREQ_8000`…`FREQ_176400`, `CCID_LIST`, `AUDIO_ACTIVE_STATE` |
| `src/packets/smp.rs` | 12 | `PASSKEY_ENTRY_FAILED`, `OOB_NOT_AVAILABLE`, `REPEATED_ATTEMPTS`, … |
| `src/packets/att.rs` | 9 | `INSUFFICIENT_AUTHENTICATION`, `PREPARE_QUEUE_FULL`, `ATTRIBUTE_NOT_LONG`, … |
| …58 more files | ~200 | |

**444 of the 573 are `constant`s, almost all SIG enumerations** — AVRCP PDU IDs,
AV/C subunit types, AVDTP and ASCS and SMP and ATT error codes, the unimplemented
half of the USB HID keycode page, LC3 sampling frequencies. Unreferenced because
the *profile* is partly implemented, not because the constant is wrong. Deleting
`BAD_RECOVERY_TYPE` buys nothing and costs the next person the table.

So the bucket was **not** bulk-deleted. ~180 of those items live in the nine
modules that are no longer public at all (§4), which removes them from the API
without removing them from the tree. The remaining ~390 sit in supported modules,
a mechanical follow-up for someone who knows which profiles are being finished:
`pub(crate)` on an unreferenced item makes `dead_code` fire, so each is a
delete-or-`#[allow]` decision, and `#[allow(dead_code)]` on a spec table is worse
than leaving it `pub`.

Two entries are behaviour rather than tables:
`src/device/profile_scene.rs` exports a `ProfileScene` + `DeviceSpec` pair that
nothing constructs, and `src/transport/wasm_ws.rs` has three `pub fn`s
(`adv_signature`, `notify_characteristic_value`, `classic_status_json`) that no
page calls.

---

## 4. The boundary

The proposal was: `device`, `devices`, `scene`, `scripting`, `api`, `types`,
`transport` supported; `classic`, `controller`, `cs`, `packets`, `l2cap`, `att`,
`gap`, `profiles`, `df`, `smp`, `crypto`, `obex`, `service` demoted. The
measurement contradicts it in four places, all where the proposal demotes things
consumers import:

- **`classic` is the third-largest example consumer.** Five programs import 21
  distinct paths: `a2dp_sink`, `avrcp_remote`, `hfp_hf_pipe`, `classic_initiator`,
  `classic_discoverable`.
- **`controller` is imported by an example** — `simble::controller::sim::Link` is
  how `in_process_scene` and `netsim_two_devices` join two devices with no netsim.
- **`cs` is imported by an example** (`compute_pbr_distance` in
  `channel_sounding`) and re-exported at the crate root.
- **`profiles` is the single biggest scripting surface**: 199 items reachable
  from Rhai, ten services re-exported at the crate root, its own document
  (`docs/scripting-profile-apis.md`).

The proposal is right about `packets`, `l2cap`, `att`, `gap`, `df`, `smp`,
`crypto` and `obex` — no example, binary or script names any — and it misses
`audio`, in the same position.

**Applied:**

**Supported.** `device`, `devices`, `scene`, `scripting`, `types`, `transport`,
`api`, `service`, `client`, `gatt`, `profiles`, `android`, `classic`,
`controller`, `cs`, `mcp`. Each is imported by an example, a `src/bin/` binary
over the external `simble::` path, or is the scripting layer's public API.
`android` stays because `lib.rs` advertises the Android-shaped API; `gatt`
because `GattDatabase`, `AttributePermissions` and `CharacteristicProperties` are
how you build a device; `mcp` because `src/bin/simble.rs` links it externally.

**Exposed for inspection, no stability promise** — now `pub(crate)` unless the
`testing` feature is on: **`packets`, `att`, `l2cap`, `gap`, `smp`, `crypto`,
`df`, `audio`, `obex`.**

The public surface goes from **7 486 to 5 636 reachable items**, a **1 850-item
(24.7%) cut**, with every test and example compiling unchanged:

| module | before | after |
|---|---:|---:|
| `packets` | 847 | 0 |
| `att` | 284 | 0 |
| `obex` | 244 | 0 |
| `df` | 182 | 0 |
| `l2cap` | 178 | 0 |
| `smp` | 91 | 0 |
| `gap` | 64 | 0 |
| `audio` | 49 | 0 |
| `crypto` | 26 | 0 |
| `types` | 166 | 145 |

`types` loses 21 to the `GapDataType` deletion. The nine gated modules lose
1 965 items, of which **136 come back at the crate root** — `AdvertisingData`,
`AclReassembler`, `CoCChannel`, `CoCManager`, `KeyStore`, `PairingConfig`,
`PairingKey`, `PairingKeys`, `PairingSession`, `SmpRole` are re-exported from
`lib.rs` and rustdoc inlines them there. Re-exporting a public item out of a
private module is how a facade works, and the root spelling is now the supported
one.

---

## 5. The mechanism

Rust cannot `cfg` a visibility. Two candidates:

**(a) `pub(crate)` throughout plus a feature-gated `pub mod for_testing { pub use
… }` re-export.** Rejected. It would rewrite the import path in all 60 files
under `tests/` — `simble::packets::…` becomes `simble::for_testing::packets::…` —
flattens the module structure into one bag, creates two live spellings for the
same item, and nothing stops a downstream user typing the `for_testing` one.

**(b) paired `#[cfg(feature)] pub` / `#[cfg(not(...))] pub(crate)`.** Chosen, at
the module declaration in `lib.rs`, not per item (per item would double ~2 400
declarations). At the `mod` line it is nine macro invocations, zero edits inside
any module, zero edits in `tests/`:

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

The two `allow`s on the closed arm restore the status quo. While these modules
were unconditionally `pub`, neither lint could fire — a `pub` item is reachable
by definition. Making the module crate-private wakes them, and what they report
is almost entirely wire-format types and spec tables that `tests/` genuinely
uses, from a crate rustc cannot see. The open arm keeps both lints, and the open
arm is what every `cargo test` and CI's `--all-features` clippy compile.

**Turning it on automatically** is the self-dev-dependency idiom:

```toml
[dev-dependencies]
simble = { path = ".", features = ["testing"] }
```

Cargo unifies a path dependency's features with the package's own lib target, so
`cargo test`, `cargo bench` and `cargo build --examples` all get `testing` while
`cargo build` and a downstream `[dependencies] simble` do not.

### Two lints that only exist when the door is shut

Clippy suppresses `wrong_self_convention` and `enum_variant_names` on exported
API (`avoid-breaking-exported-api`, on by default). Closing `df` and `gap` woke
both. Neither was fixed by renaming, because both names are the specification's:
`CteType::{AngleOfArrival, AngleOfDepartureOneUs, AngleOfDepartureTwoUs}` is Core
Vol 4 Part E §7.8.80's own vocabulary, and `KeyMaterial::to_bytes(&self)` is a
signature change belonging to a different task. Both carry a scoped `#[allow]`
with the reason written down.

---

## 6. Proof the closed surface is actually checked

CI ran `--all-features` in three of its steps, and the self-dev-dependency puts
`testing` into every test and example build — so left alone, every CI step would
compile the surface wide open and none could tell whether the crate a downstream
user gets still builds. `.github/workflows/ci.yml` gains its own step, placed
**before** anything with `--all-features`:

```yaml
- name: Build and lint the CLOSED public surface (no features)
  run: |
    cargo build --lib --no-default-features
    cargo clippy --lib --no-default-features -- -D warnings
```

**Proof the boundary is real.** A throwaway crate with `[dependencies] simble =
{ path = "…" }` and no features:

```rust
let _ = simble::types::Address::from_be_bytes([0; 6]);   // ok
let _: Option<simble::AdvertisingData> = None;           // ok — root re-export
                                                         //      out of a private module
let _ = simble::packets::hci::HCI_COMMAND_PACKET;        // error[E0603]: module
                                                         //      `packets` is private
```

**Publishability**, verified. `cargo package` (with the verification build)
succeeds: **381 files, 5.6 MiB, 1.4 MiB compressed** — identical to before.
Unpacking the `.crate` shows the self-dev-dependency is gone, because Cargo
strips path-only dev-dependencies when publishing:

```toml
[dev-dependencies.zerocopy]
version = "0.8"
features = ["derive"]
```

`[features]` keeps `lc3` and `testing`; a consumer can see the feature exists and
has no reason to enable it.

### One thing this does not check

`cargo doc --no-deps --no-default-features` reports 16 broken intra-doc links
against the closed surface, versus 6 on `--all-features`. **The 6 are
pre-existing on `main`** (`SdpPdu`, `SCENE_STEPS_PER_TICK`,
`Self::want_profile_version`, `fit_within_legacy_limit`, and a redundant explicit
link target). The other 10 are module docs in *supported* modules linking into
now-private ones, e.g. `classic/a2dp.rs` → `crate::audio::sbc`, `controller/sim.rs`
→ `crate::packets`. CI's doc step is `--all-features`, so none of this is red
today; making the closed doc build a gate means converting those 10 links to code
spans first.

---

## 7. `#[non_exhaustive]` on the 14 spec-discriminant enums

Done — all 14, each with the reason inline:

| file | enums |
|---|---|
| `profiles/bap.rs` | `SamplingFrequency`, `FrameDuration`, `AnnouncementType` |
| `profiles/aics.rs` | `Mute`, `GainMode`, `AudioInputStatus`, `AudioInputType` |
| `profiles/bass.rs` | `PeriodicAdvertisingSyncParams`, `PeriodicAdvertisingSyncState`, `BigEncryption` |
| `profiles/mcp.rs` | `MediaState`, `MediaControlPointOpcode` |
| `profiles/ascs.rs` | `AseState` |
| `types/hci_types.rs` | `AddressType` |

`PeriodicAdvertisingSyncParams` is the PA_Sync parameter of Add/Modify Source,
BASS §3.1.1.4. The other 114 public enums are untouched. `#[non_exhaustive]` only
bites an *external* `match`; `tests/` and `examples/` are external crates, and
all 1 362 tests still pass — so no test was matching these exhaustively.

`profiles/ascs_client.rs` declares a *second* `AseState`, distinct from the one
in `ascs.rs`: no discriminants, a trailing `Unknown(u8)` variant. Deliberately
excluded — and the best existing answer to §8.

---

## 8. The unknown-wire-value policy — stated, not settled

Not decided here; no code changed for it. The crate has **four** answers in the
tree today:

| approach | where | what happens to `0x07` when the SIG defines it and we do not know it |
|---|---|---|
| `Option`, `None` on unknown | `bap.rs::from_u8`, `bass.rs::from_u8` | **destroyed.** The byte is gone; nothing can echo it back or log it. |
| bare `_ =>` | `ascs.rs` | **swallowed silently,** worse than `Option` — the caller is not even told something was unrecognised. |
| newtype + `Display` fallback | `hci_types.rs` (`UNKNOWN (0x07)`) | **preserved.** Round-trips, prints usefully, costs a `match` its exhaustiveness. |
| enum with `Unknown(u8)` | `ascs_client.rs::AseState` | **preserved,** and still an enum: `match` stays exhaustive-checked, the value is carried, a downstream `match` compiles. |

The case for preserving: the most expensive bugs in this project's history were
all *"we lied about what the peer said"* — the CSIS RSI byte order, the `bass.rs`
sync state, the four invented Ranging Service UUIDs. `Option` and bare `_` both
make that class of bug unobservable by construction, discarding the evidence at
the parse boundary.

The case against converting now is `AseState` (`ascs.rs`), whose discriminants
feed the state matrix that just landed; changing its shape and its state machine
in one commit is how you lose the ability to bisect.

`ascs_client.rs` already does the fourth row in-tree, and it composes with
`#[non_exhaustive]` where the newtype does not (a newtype has no variants, so
`#[non_exhaustive]` is meaningless on it; `Unknown(u8)` keeps the enum an enum).
If the policy lands on that shape, `PeriodicAdvertisingSyncState` and
`BigEncryption` are the lowest-risk first conversions — small, freshly written,
`from_u8` callers all in one file.

**Decide the policy, then convert.** Not the other way round.

---

## 9. What was left alone, and why

- **~390 used-by-nothing items in supported modules.** Overwhelmingly SIG
  constant tables for partly-implemented profiles (§3); cutting them needs
  someone who knows which profiles are being finished.
- **`classic`, `controller`, `cs`, `profiles`, `android`, `gatt`, `api`,
  `service`, `client`** — proposed for demotion, kept, each because a consumer
  imports it (§4).
- **The other 114 public enums** — our own state machines. `#[non_exhaustive]`
  there is ceremony.
- **`#[doc(hidden)]` and sealed traits** — still zero of each. Sealing traits is
  a separate, smaller job (only 7 public traits).
- **The 10 pre-existing broken intra-doc links** into now-private modules, and
  the 6 already broken on `main` (§6).
- **`KeyMaterial::to_bytes(&self)`** and `CteType`'s variant prefixes — scoped
  `#[allow]`s, not renames (§5).

## 10. Gates, before and after

| gate | baseline | after |
|---|---|---|
| `cargo test` | 1 362 passed / 0 failed | **1 362 passed / 0 failed** |
| `cargo clippy --all-targets --all-features -- -D warnings` | clean | clean |
| `cargo clippy --lib --no-default-features -- -D warnings` | *(did not exist)* | **clean** |
| `cargo fmt --all -- --check` | clean | clean |
| `python3 scripts/check_hci_command_answers.py` | in sync | in sync |
| catalog `.pass.rhai` / `.fail.rhai` via the CLI | rc=0 | rc=0 |
| `cargo build --target wasm32-unknown-unknown --lib --features lc3` | ok | ok |
| `cargo package` (with verification build) | 5.6 MiB / 1.4 MiB | 5.6 MiB / 1.4 MiB |
