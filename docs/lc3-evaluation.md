# LC3 codec options for the wasm demo devices

**Scope.** This evaluates LC3 *only* for the example devices simble compiles
into wasm — the browser demo pages where a scripted LE Audio sink receives
isochronous SDUs and the page plays them. Not a general-purpose codec for
library users, not on any hot path, not security-critical. It must build for
`wasm32-unknown-unknown` and stay small, because it ships inside a demo
bundle that is already ~3.7 MB. Decoding is the priority; encoding is a
nice-to-have.

**Recommendation: use `lc3-codec` (pure Rust), decoder only, behind an
optional cargo feature.** It builds for wasm cleanly and costs ~80 KB. See
the caveats before relying on it for anything but demos.

---

## Candidates

Searching crates.io for "lc3" is mostly noise: the LC-3 *teaching CPU*
(`lc3-vm`, `lc3-ensemble`, `lc3as`, `lc3core`, …) dominates the results.
Exactly two crates implement the audio codec.

| | `lc3-codec` | `lc3-sys` |
|---|---|---|
| Version | 0.2.0 | 0.1.0 |
| Published | 2022-06-12 | 2024-01-03 |
| Repo last pushed | 2023-07-27 | — |
| Licence | Apache-2.0 | Apache-2.0 |
| Kind | Pure Rust, `#![no_std]` | Unsafe bindings to Google's liblc3 (C) |
| Downloads (total / recent) | 4,357 / 442 | 2,037 / 12 |
| GitHub | 31 stars, 4 forks, 1 open issue, not archived | — |
| **wasm32-unknown-unknown** | **builds** | **fails** |
| Size delta (decoder) | **+80,606 bytes** | n/a |
| Size delta (decoder + encoder) | +131,540 bytes | n/a |

### Build evidence

Measured in a scratch crate outside the repo (`cdylib`, `opt-level = "z"`,
`lto = true`), calling the codec for real so nothing is stripped:

| Build | wasm bytes |
|---|---|
| Baseline (one trivial exported fn) | 379 |
| + `lc3-codec` decoder, actually invoked | 80,985 |
| + decoder **and** encoder | 131,919 |

The artifact was checked to be genuine, not a stripped shell: valid wasm
magic, `decode_probe` present in the export table, and float math linked in.

`lc3-sys` fails on wasm, verbatim:

```
cargo:warning=liblc3/src/fastmath.h:23:10: fatal error: 'math.h' file not found
error occurred in cc-rs: command did not execute successfully (status code exit status: 1):
  LC_ALL="C" "clang" "-Oz" ... "--target=wasm32-unknown-unknown" ... "-c" "liblc3/src/attdet.c"
```

This is structural, not a configuration mistake: `lc3-sys` compiles vendored
liblc3 C through `cc-rs`, and `wasm32-unknown-unknown` is a bare target with
no libc and no sysroot, so `<math.h>` does not exist. Making it work would
require a wasi-sdk or emscripten sysroot and a different target triple.
`lc3-sys` also drags in the `windows_*` crate family, which is dead weight
here.

---

## Quality assessment of `lc3-codec`

**Good:**
- **Genuinely pure Rust and `#![no_std]`.** No C, no build script, no
  `std::time`, no threads, no filesystem or network use — checked by grep,
  which matters because simble was bitten this session by
  `SystemTime::now()` panicking on `wasm32-unknown-unknown`.
- **Light, clean dependency tree** — 9 transitive crates (`bitvec`,
  `byteorder`, `fast-math`, `heapless`, `num-traits`, `spin`, `tap`, `wyz`,
  and friends), all pure Rust and no_std-friendly.
- **Its own test suite passes**: 37 tests, `cargo test --release`, 0 failures.
- **Both encoder and decoder** are implemented, at the LC3 v1.0 revision.
- Written for embedded use, so it is allocation-light: buffers are passed in
  by the caller. The `alloc` feature is on by default and changes the
  constructor arity (`Lc3Decoder::new(num_channels, …)` with `alloc`;
  `Lc3Decoder::<N>::new(…)` without).

**Caveats — these decide how far to trust it:**
- **Not conformance-tested.** Its decoder tests assert against *golden
  output the author captured from their own implementation*, not against
  ETSI/Bluetooth SIG reference test vectors. The README is honest about
  this: the codec is "not currently approved or verified in any formal way
  other than the author's own testing against a codec that has been
  validated." Fine for a simulator demo; **not** a basis for claiming
  conformance or for interop testing where the codec must be above
  suspicion.
- **Effectively unmaintained.** Last release 2022-06-12, last repo activity
  2023-07-27. Not archived and it does not need to change often (LC3 v1.0 is
  a frozen spec), but expect no fixes.
- Small user base (31 stars, ~440 recent downloads), so bugs are unlikely to
  have been found by others.

---

## The alternatives considered

**Compile Google's liblc3 to wasm directly and call it from JS.** liblc3 is
the reference-quality implementation (Apache-2.0, 254 stars, actively
maintained — last pushed 2025-05-23) and is what Android itself uses, so it
is the only option that would be *correct by construction*. But it needs a C
→ wasm toolchain, and **neither emscripten nor a wasi-sdk is installed on
this machine** (checked). It would also live as a separate wasm module
loaded alongside `simble_bg.wasm`, with its own build step in the Pages
workflow and glue on the JS side. That is a meaningful amount of new build
machinery for a demo feature.

**Do nothing.** simble's ISO layer already treats SDU payloads as opaque
bytes, so the media plane is real today with PCM in the SDUs, and the
speaker page says so plainly. This costs nothing and is not dishonest —
but it means Android's own audio can never be decoded, because Android
sends LC3.

---

## Recommendation

1. **Adopt `lc3-codec`, decoder only, behind an optional feature** (e.g.
   `lc3 = ["dep:lc3-codec"]`), enabled for the wasm demo build. 80 KB on a
   3.7 MB bundle is ~2%, which is affordable, and the demos gain the ability
   to decode real LC3 frames. Add the encoder later only if a demo needs to
   *source* audio to a phone; it roughly doubles the cost (+131 KB total).
2. **Document what it is.** Wherever the demos mention LC3, say the codec is
   a community pure-Rust implementation validated against its author's
   golden outputs, not against SIG conformance vectors. Given how much of
   this session was spent correcting overstated capabilities, this matters.
3. **Keep the SDU path codec-agnostic.** Do not let LC3 leak into the ISO
   layer; it stays opaque bytes, and the codec is a demo-side concern. That
   preserves the option to swap in liblc3 later without touching the
   transport.

**Fallback if `lc3-codec` disappoints in practice** — i.e. it fails to decode
frames Android actually produces, which is a live risk given it is not
conformance-tested: build **liblc3 to wasm** as a separate module (install
wasi-sdk or emscripten, add a build step, load it beside the simble module).
That is more build machinery but it is the implementation Android uses, so
interop is not in question.

---

## Surprises worth noting

- **`lc3-sys` is unusable for this target, and would be even if maintained.**
  Any `-sys` crate is, for `wasm32-unknown-unknown` — the failure is the
  missing libc, not the crate's quality.
- **The size cost is far lower than expected** — a full LC3 decoder in 80 KB
  of wasm. Bundle size is not a reason to avoid this.
- **The ecosystem is thin**: exactly one viable pure-Rust LC3 codec exists,
  it is four years old, and it has ~31 stars. For a Bluetooth-adjacent
  project this is the kind of dependency worth vendoring or forking if it
  ever becomes load-bearing beyond demos.
- The crate's API changes shape with the `alloc` feature (constructor arity
  and generics differ), which is an easy thing to trip over when wiring it
  up.
