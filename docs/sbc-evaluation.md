# SBC for the A2DP media path — options, licensing, and what was built

**Scope.** SBC is the *mandatory* A2DP codec, so this is not the same kind of
decision LC3 was (`docs/lc3-evaluation.md`, where a codec was wanted only so a
demo page could make noise). Simble could negotiate A2DP — AVDTP signalling,
capability intersection, Set_Configuration, RTP packetization — and then had
nothing to put in the packets. Anything that wants to *be* an A2DP endpoint
rather than describe one needs an encoder and a decoder.

**SBC is implemented from the specification.** Every SBC implementation in
wide use is LGPL, no pure-Rust SBC *encoder* exists on crates.io at all, and
the codec is small enough (~1,100 lines, no dependencies) that the dependency
question mostly evaporates. It lives in `src/audio/sbc.rs`, verified against
bluez's `libsbc` in both directions; the measurements are below.

---

## 1. Licensing is the constraint, and it decides the outcome

Simble is Apache-2.0. The SBC ecosystem is not.

| Implementation | Licence | Usable here? |
|---|---|---|
| bluez `libsbc` (the reference; what Linux and Android's userspace descend from) | **LGPL-2.1-or-later** | No |
| FFmpeg `libavcodec/sbcenc.c`, `sbcdec.c` | **LGPL-2.1-or-later** | No |
| `libsbc` / `libsbc-sys` crates | Cargo says `MIT OR Apache-2.0` — **but that covers only the binding**; the crate vendors bluez's C, which is LGPL | No |
| `msbc-decoder` crate | **LGPL-2.1-or-later**, self-described as "a bit-exact translation of FFmpeg's `sbcdec.c`" | No |
| `mini_sbc` crate | MIT OR Apache-2.0, genuinely independent-looking | Licence is fine; see below |

Two things are worth saying plainly, because both are easy to get wrong:

- **`libsbc-sys` advertising `MIT OR Apache-2.0` is a trap.** The crate's
  Cargo metadata describes the ~100 lines of Rust binding. `sbc/sbc/sbc.c`
  inside the crate carries the bluez header: *"This library is free software;
  you can redistribute it and/or modify it under the terms of the GNU Lesser
  General Public License … version 2.1."* Adding the crate adds LGPL code.
- **`msbc-decoder` is refreshingly honest about the same issue** and is
  therefore correctly labelled LGPL: "A translation is a derivative work, so
  this crate carries the license of the original." That is the right analysis,
  and it is why nothing here was ported from FFmpeg or bluez either.

Nothing in `src/audio/sbc.rs` was copied, translated, or transcribed from any
of those. The algorithm comes from the A2DP specification's Appendix B (the
SBC codec specification), which describes the filterbanks, the bit allocation,
the quantizer and the frame layout in full.

**On the two coefficient tables.** `PROTO_4_40` and `PROTO_8_80` are the
prototype filter coefficients the SBC specification tabulates, to nine
significant figures. They are specification constants — the numbers a
conforming implementation must use — not expression borrowed from any
implementation. They were cross-checked against the MIT-licensed `mini_sbc`
crate's copy of the same tables to confirm no digit was mistyped, and against
libsbc's behaviour: a mistyped coefficient does not produce a bitstream that
matches the reference to within 0.01% of bytes, and a deliberate 0.1%
perturbation of one coefficient fails the interop tests.

---

## 2. The Rust crate landscape

Searching crates.io for "sbc" is mostly noise (single-board computers,
broadcast channels). The audio codec has three entries.

| | `mini_sbc` | `libsbc` / `libsbc-sys` | `msbc-decoder` |
|---|---|---|---|
| Version | 0.1.7 | 0.1.5 / 0.1.2 | 0.1.0 |
| Published | 2023-03-17 | 2021-01-25 / 2019-03-18 | 2026-08-11 |
| Licence | MIT OR Apache-2.0 | binding MIT/Apache, **vendored C LGPL-2.1** | **LGPL-2.1-or-later** |
| Kind | Pure Rust, `no_std` | `cc`-built bindings to bluez C | Pure Rust |
| Encoder | **No** | Yes | **No** |
| Decoder | Yes | Yes | mSBC only (16 kHz, fixed format) |
| `wasm32-unknown-unknown` | **builds** (measured) | **no** — a `-sys` crate on a target with no libc | pure Rust, so presumably; not measured |
| Downloads (recent) | ~5,900 | ~1,200 | 18 |

**There is no pure-Rust SBC encoder on crates.io.** At best a crate could
supply half of what was needed, leaving the encoder to be written from the
specification anyway — and half a codec is the worse outcome, because the
encoder and decoder share the filterbank, allocator and frame layout.

`mini_sbc` is a real, independent decoder — it keeps the specification's
decimal coefficient strings in the source next to the fixed-point tables
generated from them — it is `no_std`, and its licence is compatible. Its
weaknesses: 1,752 lines for a decoder alone against 1,090 for both halves
here, no encoder, no activity since March 2023, **its two tests assert
nothing** — they `println!` the decoded samples and pass unconditionally — and
it already trips a future-incompatibility lint (`#[macro_export(crate)]`,
"will become a hard error in a future release"). An unmaintained crate with no
assertions, for half the job, is not better than writing the codec.

`libsbc-sys` fails on `wasm32-unknown-unknown` for the same structural reason
`lc3-sys` does (`docs/lc3-evaluation.md`): it compiles vendored C through
`cc-rs`, and the target has no libc and no sysroot. That is on top of the
licence problem, which is the disqualifying one.

---

## 3. The oracle

**Bumble does not have an SBC codec.** `bumble/codecs.py` (533 lines) is AAC
LATM/ADTS bit-stream handling — `AudioMuxElement`, `StreamMuxConfig`,
`AudioSpecificConfig` — and nothing else. `bumble/a2dp.py` has `SbcFrame`,
`SbcParser` and `SbcPacketSource`, all of which parse *headers* and
repacketize frames without ever touching a sample; Bumble's `player`/`speaker`
apps read `.sbc` files and hand the frames to the transport. Simble's existing
`SbcFrame::parse` is a faithful port of that header parser.

The oracle therefore comes from elsewhere. **bluez's `libsbc` was built as a
throwaway command-line tool outside this repository**, from the C vendored
inside the `libsbc-sys` crate, and used to encode and decode files. The only
thing that crossed into the repository is the *bytes it produced from simble's
own input signal* — the golden vectors in `tests/sbc_interop_test.rs`. No LGPL
source is vendored, linked, or depended on; simble does not build or run
libsbc, and nothing in CI needs it.

(Building it on arm64 macOS needs `-U__ARM_NEON__`: the vendored copy is old
enough that its `__ARM_NEON__` guard admits 32-bit ARM inline assembly that
arm64 clang cannot assemble.)

---

## 4. Measurements against libsbc

Fifteen configurations — every channel mode, both allocation methods, both
subband counts, all four sampling frequencies, bitpools from 2 to 53 — over
one second (44,100 samples) of deliberately transient-rich audio: percussive
onsets every 5.8 ms, a chirp sweeping the whole band every 512 samples, a
sustained chord, and a silent gap. **A steady tone was avoided on purpose**:
the LC3 bug that prompted this testing method passed every sine-wave assertion
and still measured 11.7 dB against its reference on music.

Both directions were measured. `their-enc/my-dec` is libsbc's bitstream
decoded by simble, scored against libsbc's own decode of it.
`my-enc/their-dec` is simble's bitstream decoded by libsbc, scored against
simble's own decode of it. "quality" is how faithfully each *encoder*
reproduces the source, judged by the same decoder.

| Configuration | their-enc / my-dec | max err | my-enc / their-dec | max err | quality theirs / mine | bitstream bytes differing |
|---|---|---|---|---|---|---|
| mono 8sb 16blk bp32 loudness | 77.1 dB | 5 | 77.2 dB | 5 | 19.7 / 19.7 dB | 31 / 24,768 (0.13%) |
| mono 8sb 16blk bp32 SNR | 77.2 dB | 5 | 77.2 dB | 5 | 22.8 / 22.8 dB | 16 / 24,768 (0.06%) |
| mono 4sb 8blk bp16 loudness | 77.9 dB | 5 | 77.9 dB | 5 | 22.9 / 22.9 dB | 115 / 30,316 (0.38%) |
| mono 4sb 16blk bp24 SNR | 77.9 dB | 5 | 77.9 dB | 5 | 35.5 / 35.5 dB | 112 / 37,206 (0.30%) |
| mono 8sb 4blk bp48 @48 kHz | 76.7 dB | 5 | 76.7 dB | 5 | 37.4 / 37.4 dB | 620 / 44,096 (1.41%) |
| mono 8sb 16blk **bp2** | 74.7 dB | 5 | 74.7 dB | 5 | 0.8 / 0.8 dB | 0 / 4,128 (0.00%) |
| stereo 8sb 16blk bp53 | 76.8 dB | 6 | 76.8 dB | 6 | 15.1 / 15.1 dB | 20 / 40,592 (0.05%) |
| joint stereo 8sb 16blk bp53 | 76.4 dB | 5 | 76.4 dB | 5 | 16.6 / 16.6 dB | 26 / 40,936 (0.06%) |
| joint stereo 8sb 16blk bp35 | 76.7 dB | 6 | 76.7 dB | 6 | 10.7 / 10.7 dB | 9 / 28,552 (0.03%) |
| joint stereo 4sb 12blk bp30 | 77.2 dB | 5 | 77.2 dB | 5 | 20.4 / 20.4 dB | 118 / 49,572 (0.24%) |
| joint stereo 8sb 16blk bp53 SNR | 76.3 dB | 6 | 76.3 dB | 5 | 19.6 / 19.6 dB | 16 / 40,936 (0.04%) |
| dual channel 8sb 16blk bp32 | 76.7 dB | 5 | 76.7 dB | 5 | 18.6 / 18.6 dB | 48 / 48,160 (0.10%) |
| joint stereo 8sb 16blk bp51 @48 kHz | 76.5 dB | 5 | 76.5 dB | 5 | 16.3 / 16.3 dB | 23 / 39,560 (0.06%) |
| joint stereo 8sb 16blk bp26 @16 kHz | 76.7 dB | 6 | 76.7 dB | 6 | 8.9 / 8.9 dB | 2 / 22,360 (0.01%) |
| joint stereo 8sb 16blk bp32 @32 kHz | 76.8 dB | 5 | 76.8 dB | 5 | 10.1 / 10.1 dB | 4 / 26,488 (0.02%) |

**Worst case in either direction: 74.7 dB, largest single-sample error 6 out
of ±32,768.** Every frame length matches libsbc's exactly. Every frame either
implementation produced was accepted and fully consumed by the other — which
also validates the header CRC independently, since simble verifies it and
rejects a frame that fails.

- **The "quality" columns are equal to 0.1 dB in every single row.** Simble's
  encoder makes the same scale-factor, bit-allocation and joint-stereo
  decisions libsbc does, not merely comparable ones.
- **The bitstreams themselves agree to within 0.01%–1.4% of bytes.** The
  residual is simble being `f64` where libsbc is fixed point: a subband sample
  that lands within a rounding step of a quantizer boundary can go either way.
  Where the scale factors were compared directly (via the header CRC, which
  covers the join field and every scale factor) they were identical in 13 of
  15 configurations and differed in at most 23 frames of 1,378 in the others.
- The absolute "quality" numbers are low in places (0.8 dB at bitpool 2, 8.9
  dB at 16 kHz) because the test signal is far denser than music. Both codecs
  do equally badly on it, which is the only claim being made.

### What the CI tests actually cover

`cargo test` cannot run libsbc, so `tests/sbc_interop_test.rs` embeds real
libsbc bitstreams and libsbc's own decode of them. Three configurations,
**chosen by mutation testing**: the codec was deliberately broken seventeen
different ways, and this is the smallest set that catches sixteen of them.

| Configuration | Why it is there | simble vs libsbc, decoded | bitstream agreement |
|---|---|---|---|
| 44.1 kHz joint stereo, 8 subbands, 16 blocks, bp 53 | what a phone and headphones settle on | 76.0 dB, max error 4 | 951 / 952 bytes |
| 44.1 kHz mono, 4 subbands, 8 blocks, bp 12 | the other filterbank; the only configuration found that reaches the bit allocator's *final* top-up loop | 78.6 dB, max error 3 | 576 / 576 bytes |
| 16 kHz mono, 8 subbands, 16 blocks, bp 26 | row 0 of the frequency-dependent loudness table, which 44.1 kHz never indexes | 79.1 dB, max error 2 | 480 / 480 bytes |

Mutations caught: a single loudness-offset entry changed by one; the
joint-stereo decision disabled; joint stereo extended to the top subband;
scale factors one step too large; either of the bit allocator's two top-up
loops removed; stereo given a per-channel bitpool; the CRC seed or polynomial
changed; the synthesis window's sign; either cosine matrix's phase offset; the
quantizer rounding instead of truncating; the dequantizer's half-step; one
prototype coefficient perturbed by 0.1%; the analysis filterbank's state not
carried between blocks.

**The one mutation not caught**: changing `LOUDNESS_OFFSET_4` row 0, which is
16 kHz *with 4 subbands* — a combination no golden vector uses. 32 kHz and
48 kHz rows are likewise only covered by the scratchpad sweep above, not by
CI. Closing that would take five more vector sets; it was judged not worth the
~400 lines.

---

## 5. What was built

`src/audio/sbc.rs` — 1,090 lines of codec plus 336 of unit tests, no
dependencies:

- **Encoder and decoder**, all four channel modes, both subband counts, both
  block-length ranges, both allocation methods, all four sampling frequencies.
- Polyphase analysis and synthesis filterbanks, scale factors, the
  specification's bit allocator (which really does differ between mono/dual
  and stereo/joint, not just in a loop bound), the quantizer, joint-stereo
  decision and reconstruction, header CRC, and MSB-first bit packing.
- `f64` throughout, where the reference implementations are fixed point. That
  is a deliberate simplicity choice; section 4 measures what it costs.

**Not feature-gated**, unlike `lc3`. The reasoning:

- `lc3` is gated because it is a ~7,000-line third-party dependency that only
  the browser demo pages need, and the core library treats ISO SDUs as opaque.
- SBC is different on both counts. It has **no dependencies at all**, and it is
  the codec A2DP makes mandatory — a Bluetooth simulator that can negotiate an
  SBC stream and not produce a valid SBC frame has the same shape of hole LE
  Audio had when ASCS and PACS existed but no CIS carried LC3.
- The cost is small and was measured. Adding the module to the wasm build
  costs **1,464 bytes** as-is, because the linker strips what nothing calls;
  with a `wasm_bindgen` export that actually runs an encode and a decode so
  nothing can be stripped, it costs **27,431 bytes** — 0.6% of the 4.5 MB
  bundle. A feature flag would buy back 27 KB in exchange for a configuration
  where A2DP silently cannot carry audio.

### Two properties worth recording

- **SBC frames are *not* independent**, despite there being no inter-frame
  MDCT overlap. SBC's polyphase filterbank keeps ten blocks of past input on
  the analysis side and twenty of past output on the synthesis side, so a
  decoder handed frame *n* with a zeroed filterbank produces a wrong first
  block. `test_a_decoder_reset_between_frames_corrupts_the_stream` measures
  this rather than asserting it, so the claim cannot quietly come back.
- **The round trip is delayed by `9 × subbands + 1` samples per channel** — 73
  for 8 subbands, 37 for 4. Found empirically, by scanning for the lag that
  maximises SNR through a real round trip; a comparison that ignored it
  reported −3.1 dB for a codec that was actually fine. It is exposed as
  `SbcParameters::filter_delay()`, because anything comparing input PCM with
  output PCM needs it.

---

## 6. AAC — scoped honestly

**Simble does not have an AAC codec and should not get one.** What was
strengthened is the ADTS *framing*, which is tractable and useful; the codec
is neither.

### What implementing AAC from scratch would take

AAC-LC — the profile A2DP uses — is not SBC with more steps. A decoder needs
an MDCT with two window shapes and window-sequence switching (long, start,
eight short, stop), Huffman decoding against eleven spectral codebooks plus a
scale-factor codebook, inverse quantization with a 4/3-power law, scale-factor
bands whose boundaries are tabulated per sampling frequency, mid/side and
intensity stereo, TNS (an LPC filter applied in the frequency domain), and
PNS. An *encoder* needs all of that plus a psychoacoustic model — which is
where AAC's quality actually comes from and which the specification does not
give you. This is tens of thousands of lines and a research problem, against
SBC's ~1,100 lines of fully specified arithmetic.

### Unlike SBC, the crate ecosystem here is *not* empty

The licensing argument that decides SBC does **not** transfer to AAC.

| Crate | Licence | Kind | Maturity |
|---|---|---|---|
| `symphonia-codec-aac` 0.6.1 | MPL-2.0 | Pure Rust **decoder** | 6.7M downloads, 2.7M recent, updated 2026-08-13 |
| `oxideav-aac` 0.1.6 | MIT | Pure Rust decoder **and encoder** | first published 2026-04, 4,669 downloads |
| `rusty_aac` 0.5.0 | Apache-2.0 | Pure Rust decoder **and encoder** | first published 2026-07-29, 2,171 downloads |
| `fdk-aac` / `fdk-aac-sys` | binding MIT; **libfdk-aac itself is under Fraunhofer's bespoke non-free licence** | C bindings | 750k downloads |
| `faad2-sys` | **GPL-2.0** | C bindings | abandoned (2020) |
| `adts-reader` 0.4.0 | MIT/Apache-2.0 | ADTS framing only, no codec | 2017, still maintained |

`symphonia-codec-aac` is real and mature, and it builds for
`wasm32-unknown-unknown` (measured, alongside `mini_sbc`, in a scratch crate).
MPL-2.0 is file-level copyleft and is compatible with an Apache-2.0 consumer.
An AAC decoder is available; the reason not to adopt one is different.

### Why not, then

A2DP requires SBC of every implementation; AAC is optional and negotiated only
when both ends offer it. Simble models the transport, and the transport does
not care what is inside an RTP payload — the ISO path treats LC3 the same way.
Simble can already advertise AAC capability, intersect it, and configure a
stream, and with the framing work below it can carry AAC frames end to end as
opaque payloads. Adding a decoder would let a *simulated sink* listen to AAC,
which is a demo feature, not a protocol one — the same category as LC3, and it
should be handled the same way if it is ever wanted: an optional feature, a
third-party decoder, and a doc that says plainly it is not conformance-tested.

The *encoder* side is where the two new crates should be treated with caution
rather than adopted: both appeared in 2026 with a few thousand downloads, both
claim a full AAC-LC encoder, and `rusty_aac`'s description claims it "beats
FFmpeg on mono music at 128k+". None of that is verified, and an unverified
encoder is a risky dependency. `adts-reader` was not adopted either: it reads
ADTS but does not write it, and simble's parser already lives inside the A2DP
module where the capability structures are.

### What was done instead

`AacFrame` in `src/classic/a2dp.rs` now parses the whole ADTS header rather
than five fields of it, and can write one back. Verified against Bumble in
`tests/adts_interop_test.rs`: Bumble's `AacAudioRtpPacket.to_adts()` wrote the
vectors, Bumble's `AacParser` read them back, and simble has to agree with
both — the parse *and* the re-encode, byte for byte.

Simble's parser was a port of Bumble's, so it inherited Bumble's blind spots.
Three were real bugs:

1. **`protection_absent` was ignored.** The bit is inverted: 0 means a CRC
   *is* present, and two CRC bytes then sit between the 7-byte header and the
   payload. Simble was handing those two bytes to callers as the first two
   bytes of AAC audio. The existing `test_aac_parser` had the wrong expected
   payload length baked into it and passed for exactly that reason.
2. **`number_of_raw_data_blocks_in_frame` was ignored.** A frame carries one
   to four raw data blocks; simble (like Bumble) reported the duration of one.
   A four-block frame was described as playing for a quarter of its real
   length.
3. **Reserved sampling-frequency indices 13–15 were accepted** and reported as
   0 Hz, which makes every downstream duration calculation divide by zero.
   They are now rejected.

Also added: the MPEG version (`ID`) bit, the CRC value when present,
`sample_count`/`duration_us`, `has_program_config_element` (a
`channel_configuration` of 0 means the layout is inside the payload, so a
header-only reader cannot know the channel count), `find_sync` for
re-acquiring a stream after packet loss, and `to_bytes` so a source can emit
ADTS rather than only consume it.

Bugs 1–3 are shared with Bumble as of 0.0.233, so those tests are checked
against ISO/IEC 13818-7 section 6.2 rather than against another
implementation, and they say so.

---

## 7. Notes worth recording

- **`libsbc-sys`'s advertised licence describes only the binding.** A crate's
  Cargo metadata is not a licence audit of what it vendors.
- **The ecosystem is thinner than LC3's**: exactly one pure-Rust SBC decoder
  exists, it has no assertions in its tests, and no pure-Rust encoder exists at
  all — for the *mandatory* codec of the most widely deployed Bluetooth audio
  profile.
- **Golden vectors can look fine and cover almost nothing.** An early set
  covered the first 23 ms of a signal whose energy at that point was entirely
  in subband 0, so the bit allocator was barely exercised; a mutation that
  collapsed a full sweep to 3 dB left them passing. Mutation testing caught
  that, not review, and the test signal was redesigned to be broadband from
  sample zero.
- **A mutation-testing harness can mislead.** An early run reported 0/17
  caught: it classified `error: test failed` as a compile failure, and its
  shell-quoted multi-line replacements silently did not match. Both bugs made
  the suite look *worse* than it was — a script written to grep for success
  would instead have reported a false 17/17.
