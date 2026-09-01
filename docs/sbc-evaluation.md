# SBC for the A2DP media path — options, licensing, and what was built

**Scope.** SBC is the *mandatory* A2DP codec. To *be* an A2DP endpoint rather
than just negotiate one — AVDTP signalling, capability intersection,
Set_Configuration, RTP packetization all work — simble has to put real audio in
the packets, which needs a real encoder and decoder.

**Decision: SBC is implemented from the specification**, in `src/audio/sbc.rs`
(1,090 lines of codec + 336 of unit tests, no dependencies), verified against
bluez's `libsbc` in both directions. Two reasons: every SBC implementation in
wide use is LGPL, and no pure-Rust SBC *encoder* exists on crates.io at all. The
codec is small enough (~1,100 lines, no deps) that the build-in-from-spec cost is
low.

## 1. Licensing is the constraint, and it decides the outcome

Simble is Apache-2.0; the SBC ecosystem is not.

| Implementation | Licence | Usable here? |
|---|---|---|
| bluez `libsbc` (the reference; Linux/Android userspace descend from it) | **LGPL-2.1-or-later** | No |
| FFmpeg `libavcodec/sbcenc.c`, `sbcdec.c` | **LGPL-2.1-or-later** | No |
| `libsbc` / `libsbc-sys` crates | Cargo says `MIT OR Apache-2.0`, **but that covers only the binding**; the crate vendors bluez's C (LGPL) | No |
| `msbc-decoder` crate | **LGPL-2.1-or-later** — "a bit-exact translation of FFmpeg's `sbcdec.c`" | No |
| `mini_sbc` crate | MIT OR Apache-2.0, genuinely independent | Licence fine; decoder only, see §2 |

A crate's advertised licence describes only its binding, not the C it vendors:
`libsbc-sys`'s `sbc/sbc/sbc.c` carries the bluez LGPL header. And a translation
is a derivative work carrying the original's licence — which is why nothing here
was ported from FFmpeg or bluez.

Nothing in `src/audio/sbc.rs` was copied, translated, or transcribed. The
algorithm comes from the A2DP specification's Appendix B (the SBC codec spec).
The two coefficient tables `PROTO_4_40` / `PROTO_8_80` are specification
constants (the numbers a conforming implementation must use), cross-checked
against `mini_sbc`'s copy to confirm no digit was mistyped.

## 2. The Rust crate landscape

**There is no pure-Rust SBC encoder on crates.io.** The three audio-codec
entries are `mini_sbc` (MIT/Apache, pure-Rust `no_std` **decoder only**,
unmaintained), `libsbc`/`libsbc-sys` (bluez C bindings — LGPL, and fails
`wasm32-unknown-unknown` as any `-sys` crate does), and `msbc-decoder` (LGPL,
mSBC-only). At best a crate supplies half the
job, leaving the encoder to be written from the spec anyway — and the encoder and
decoder share the filterbank, allocator and frame layout, so half a codec is the
worse outcome.

## 3. The oracle

**Bumble has no SBC codec.** `bumble/a2dp.py`'s `SbcFrame`/`SbcParser` parse
*headers* and repacketize without touching a sample; simble's existing
`SbcFrame::parse` is a faithful port of that header parser.

The oracle is **bluez's `libsbc`, built as a throwaway CLI tool outside this
repository** from the C vendored in `libsbc-sys`, used to encode and decode
files. The only thing that crossed into the repo is the *bytes it produced from
simble's own input* — the golden vectors in `tests/sbc_interop_test.rs`. No LGPL
source is vendored, linked, or built; nothing in CI needs it.

## 4. Measurements against libsbc

Simble's codec was measured against libsbc across all fifteen configurations
(every channel mode, both allocation methods, both subband counts, all four
sampling frequencies, bitpools 2–53), in both directions — libsbc's bitstream
decoded by simble, and simble's decoded by libsbc — on broadband transient-rich
audio.

**Result: bit-exact parity.** Worst case in either direction is 74.7 dB SNR with
a largest single-sample error of 6 of ±32,768; every frame length matches libsbc
exactly, and every frame either produced was accepted and fully consumed by the
other (which also validates the header CRC). Encoder quality matches libsbc's to
within 0.1 dB in every configuration — the same scale-factor, bit-allocation, and
joint-stereo decisions — and the bitstreams themselves agree to within 0.01%–1.4%
of bytes, the residual being simble's `f64` against libsbc's fixed point.

### What CI covers

`cargo test` cannot run libsbc, so `tests/sbc_interop_test.rs` embeds real libsbc
bitstreams and libsbc's decode of them. Three configurations, **chosen by
mutation testing** — the smallest set catching 16 of 17 deliberate breakages:

| Configuration | Why it is there | simble vs libsbc, decoded | bitstream agreement |
|---|---|---|---|
| 44.1 kHz joint stereo, 8 subbands, 16 blocks, bp 53 | what a phone and headphones settle on | 76.0 dB, max error 4 | 951 / 952 bytes |
| 44.1 kHz mono, 4 subbands, 8 blocks, bp 12 | the other filterbank; only config reaching the allocator's *final* top-up loop | 78.6 dB, max error 3 | 576 / 576 bytes |
| 16 kHz mono, 8 subbands, 16 blocks, bp 26 | row 0 of the frequency-dependent loudness table, which 44.1 kHz never indexes | 79.1 dB, max error 2 | 480 / 480 bytes |

## 5. What was built

`src/audio/sbc.rs` — 1,090 lines of codec plus 336 of unit tests, no
dependencies:

- **Encoder and decoder**, all four channel modes, both subband counts, both
  block-length ranges, both allocation methods, all four sampling frequencies.
- Polyphase analysis/synthesis filterbanks, scale factors, the specification's
  bit allocator (which really does differ between mono/dual and stereo/joint),
  the quantizer, joint-stereo decision and reconstruction, header CRC, MSB-first
  bit packing.
- `f64` throughout where the references are fixed point — a deliberate simplicity
  choice; §4 measures what it costs.

**Not feature-gated**, unlike `lc3`. `lc3` is gated because it is a ~7,000-line
third-party dependency only the browser demo needs; SBC has **no dependencies at
all** and is the codec A2DP makes mandatory, so gating it would only create a
build where A2DP silently cannot carry audio. Its cost in the wasm bundle is well
under 1%.

Two properties recorded as tests rather than prose: **SBC frames are not
independent** (the polyphase filterbank keeps 10 blocks of past input / 20 of
past output, so a decoder reset mid-stream corrupts the first block —
`test_a_decoder_reset_between_frames_corrupts_the_stream`), and **the round trip
is delayed by `9 × subbands + 1` samples per channel** (73 for 8 subbands, 37 for
4), exposed as `SbcParameters::filter_delay()` for anything comparing input PCM
with output PCM.

## 6. AAC — deferred

**Simble does not have an AAC codec and should not get one now.** A2DP requires
SBC of every implementation; AAC is optional and negotiated only when both ends
offer it, and simble models the transport, which does not care what is inside an
RTP payload (the ISO path treats LC3 the same way). Simble can already advertise,
intersect and configure AAC and carry frames end to end as opaque payloads;
adding a decoder would only let a *simulated sink* listen — a demo feature, the
same category as LC3, to be handled the same way (optional feature, third-party
crate, doc saying it is not conformance-tested) if ever wanted.

Unlike SBC, the crate ecosystem here is not empty, so the licensing argument does
not transfer:

| Crate | Licence | Kind |
|---|---|---|
| `symphonia-codec-aac` | MPL-2.0 | pure-Rust decoder, mature; builds for wasm32 |
| `oxideav-aac` | MIT | pure-Rust decoder + encoder, new/unproven |
| `rusty_aac` | Apache-2.0 | pure-Rust decoder + encoder, new/unproven |
| `fdk-aac` / `fdk-aac-sys` | binding MIT; libfdk-aac itself Fraunhofer non-free | C bindings |
| `faad2-sys` | GPL-2.0 | C bindings, abandoned |
| `adts-reader` | MIT/Apache-2.0 | ADTS framing only, no codec |

A mature MPL-2.0 decoder (`symphonia-codec-aac`) is available; the reason not to
adopt one is that it buys only a demo feature. The pure-Rust encoders are recent
and their full-encoder claims are unverified, so an encoder is not adopted.

### What was done instead

`AacFrame` in `src/classic/a2dp.rs` now parses the whole ADTS header (not five
fields of it) and can write one back, verified against Bumble in
`tests/adts_interop_test.rs` (Bumble's `to_adts()` wrote the vectors, its
`AacParser` read them back, both directions byte for byte). Fixing three real
bugs inherited from the Bumble port:

1. **`protection_absent` ignored.** The bit is inverted (0 means a CRC *is*
   present); two CRC bytes then sit between the 7-byte header and payload, and
   simble was handing them to callers as the first two bytes of audio.
2. **`number_of_raw_data_blocks_in_frame` ignored.** A frame carries one to four
   blocks; simble reported the duration of one.
3. **Reserved sampling-frequency indices 13–15 accepted** and reported as 0 Hz,
   dividing every downstream duration by zero. Now rejected.

Also added: the MPEG version (`ID`) bit, the CRC value when present,
`sample_count`/`duration_us`, `has_program_config_element`, `find_sync` for
re-acquiring after packet loss, and `to_bytes`. Bugs 1–3 are shared with Bumble
0.0.233, so those tests are checked against ISO/IEC 13818-7 §6.2 and say so.
