# LC3 codec options for the wasm demo devices

**Scope.** LC3 *only* for the example devices simble compiles into wasm — the
browser demo pages where a scripted LE Audio sink receives isochronous SDUs and
the page plays them. Not a general-purpose codec, not on any hot path, not
security-critical. It must build for `wasm32-unknown-unknown` and stay small (the
demo bundle is already ~3.7 MB). The demo needs decoding for a sink and encoding
for a source.

**Decision: `lc3-codec` (pure Rust) behind the optional `lc3` feature.** It is
the only pure-Rust LC3 codec that builds for wasm; the sole C alternative
(`lc3-sys`) cannot target wasm at all. SimBLE wraps its mono encoder and decoder
for the web demo. See the caveats before relying on it for anything but demos.

## Candidates

Searching crates.io for "lc3" is mostly the LC-3 *teaching CPU* (`lc3-vm`,
`lc3as`, …). Exactly two crates implement the audio codec.

| | `lc3-codec` | `lc3-sys` |
|---|---|---|
| Licence | Apache-2.0 | Apache-2.0 |
| Kind | Pure Rust, `#![no_std]` | Unsafe bindings to Google's liblc3 (C) |
| **wasm32-unknown-unknown** | **builds** | **fails** |
| wasm cost | ~80 KB decoder, ~132 KB with encoder | n/a |

`lc3-sys` fails on wasm structurally, not by misconfiguration: it compiles
vendored liblc3 C through `cc-rs`, and `wasm32-unknown-unknown` is a bare target
with no libc/sysroot, so `<math.h>` does not exist (`fatal error: 'math.h' file
not found`). Any `-sys` crate has this problem. It also drags in the `windows_*`
crate family.

## Quality of `lc3-codec`

**Good:** genuinely pure Rust and `#![no_std]` (no C, no build script, no
`std::time` — `SystemTime::now()` panics on wasm); light dependency tree (9
transitive crates, all pure Rust no_std-friendly: `bitvec`, `byteorder`,
`fast-math`, `heapless`, `num-traits`, `spin`, `tap`, `wyz`); its own 37 tests
pass; both encoder and decoder at LC3 v1.0; allocation-light (buffers passed in;
the `alloc` feature, on by default, changes constructor arity).

**Caveats — these decide how far to trust it:**
- **Not conformance-tested.** Its decoder tests assert against golden output the
  author captured from their own implementation, not against ETSI/SIG reference
  vectors. The README says so: "not currently approved or verified in any formal
  way other than the author's own testing against a codec that has been
  validated." Fine for a demo; **not** a basis for claiming conformance or for
  interop testing.
- **Effectively unmaintained.** Last release 2022-06-12, last repo activity
  2023-07-27. Not archived, and LC3 v1.0 is a frozen spec, but expect no fixes.
- Small user base, so bugs are unlikely to have been found by others.

## Alternatives considered

- **Compile Google's liblc3 to wasm and call it from JS.** liblc3 is
  reference-quality (Apache-2.0, actively maintained, what Android uses) and the
  only option correct by construction — but it needs a C→wasm toolchain, and
  neither emscripten nor a wasi-sdk is installed here, plus a separate wasm module
  with its own Pages build step and JS glue. Meaningful new build machinery for a
  demo feature; kept as the fallback below.
- **Do nothing.** simble's ISO layer treats SDU payloads as opaque bytes, so the
  media plane is real today with PCM in the SDUs. Costs nothing and is honest, but
  Android's own audio can never be decoded because Android sends LC3.

## Recommendation

1. **Use `lc3-codec` behind the optional `lc3` feature**, enabled for the wasm
   demo build; SimBLE wraps both encoder and decoder (~132 KB combined wasm cost).
2. **Document what it is** wherever the demos mention LC3: a community pure-Rust
   implementation validated against its author's golden outputs, not SIG vectors.
3. **Keep the SDU path codec-agnostic** — LC3 stays a demo-side concern, opaque
   bytes in the ISO layer, preserving the option to swap in liblc3 later without
   touching the transport.

**Fallback if `lc3-codec` disappoints** — i.e. fails to decode frames Android
actually produces, a live risk since it is not conformance-tested: build liblc3
to wasm as a separate module (install wasi-sdk or emscripten, add a build step,
load it beside the simble module). More machinery, but it is the implementation
Android uses, so interop is not in question.
