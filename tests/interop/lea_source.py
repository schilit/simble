"""Stream real liblc3 audio to a simble LE Audio sink over a real CIS.

A *foreign* LE Audio source driving simble's sink: Bumble connects, discovers
ASCS, walks the ASE through Config Codec -> Config QoS -> Enable, establishes
a real CIS, and streams LC3 frames encoded by Google's liblc3 — the same
implementation Android ships — so the sink decodes foreign audio rather than
its own encoder's output.

**This script used to be a demo, not a test.** It printed four facts and a
human read them; it had no assertion and no non-zero exit anywhere, and it
returned *cleanly* when the sink had no ASCS at all — so the most interesting
failure available exited 0. Every fact it printed is now checked, because a
script that always passes is worse than no script.

What is asserted, and why each one is a *foreign* fact:

1. **The sink has ASCS.** The missing-control-point path now fails loudly.
2. **The ASE state after Enable is `Enabling` (0x03).** The strongest check
   here: it is simble's own ASCS state machine being read back over the air
   by a foreign peer. The value is derived from ASCS §5.3 ("Enable ... the
   ASE transitions to the Enabling state"), which `src/profiles/ascs.rs`
   implements as `AseState::Enabling = 0x03` and `tests/ascs_test.rs` pins in
   simulation — *not* from whatever this script happened to print before.
   This is the exact surface where `bass.rs` was caught reporting
   `SynchronizedToPa` unconditionally; "our code lies to a foreign peer" is
   this project's most expensive bug shape, and only a foreign reader catches
   it.
3. **The CIS really was established** — a valid handle, distinct from the ACL
   handle, rather than merely that `create_cis()` returned.
4. **Real-time pacing held.** Two real bugs were found here and both are
   regressions this now guards: encoding *inside* the send loop pushed each
   iteration past the 10 ms SDU interval and starved the sink of ~12% of its
   audio, and `sleep(0.01)` in a loop accumulated scheduling overshoot until
   the stream drifted steadily behind the clock.

**Not asserted: what the sink received.** This script drives the *browser*
page (`web/audio/`), not a binary, so there is no sink process whose frame
count can be read back — a source that streamed into a void would still pass
items 1-4. `a2dp_peer.py` gets this right because it drives an example binary
that reports its decoded-frame count. Closing it here needs a headless simble
LE-audio sink example (an ASCS server, CIS acceptance and LC3 decode) that
does not exist today; that is a real gap, and it is stated rather than
papered over.

Usage — with `web/audio/` open in a browser on the netsim (WebSocket)
controller and its "Enable sound" button clicked:

    .venv/bin/python tests/interop/lea_source.py CC:1E:57:00:00:08/P
"""

import argparse
import asyncio
import math
import struct
import sys
import time

import lc3
from bumble.device import CigParameters, Device, Peer
from bumble.transport import open_transport

import bumble_link

RATE, DUR_US, FRAME_BYTES = 16000, 10000, 40
ASE_CP = "00002bc6-0000-1000-8000-00805f9b34fb"
SINK_ASE = "00002bc4-0000-1000-8000-00805f9b34fb"

# ASCS §5.3: the Enable operation moves an ASE from QoS Configured to
# Enabling. Streaming (0x04) comes later, from a separate Receiver Start
# Ready operation (§5.4) — so reading Streaming here would be simble
# skipping a state, and reading QoS Configured (0x02) would be Enable having
# silently done nothing. In-tree reference: `AseState` in
# `src/profiles/ascs.rs`, exercised by `tests/ascs_test.rs`.
ASE_STATE_ENABLING = 0x03
ASE_STATE_NAMES = {
    0x00: "Idle",
    0x01: "Codec Configured",
    0x02: "QoS Configured",
    0x03: "Enabling",
    0x04: "Streaming",
    0x05: "Disabling",
    0x06: "Releasing",
}

# Core Vol 4 Pt E §5.4.2: a connection handle is 12 bits, 0x0000-0x0EFF.
MAX_CONNECTION_HANDLE = 0x0EFF

# The SDU interval is 10 ms, so a stream that keeps up delivers 100 SDUs/s.
# The band is ±5%: the starvation bug this guards cost ~12% of the audio, so
# 5% catches it with room to spare while tolerating scheduler jitter on a
# loaded machine.
PACING_TOLERANCE = 0.05


class Verdict:
    """Accumulates checks so a run reports every failure, not just the first."""

    def __init__(self):
        self.failures = 0

    def check(self, condition, message):
        if condition:
            print(f"ok    {message}", flush=True)
        else:
            print(f"FAIL  {message}", flush=True)
            self.failures += 1
        return bool(condition)

    def fatal(self, condition, message):
        """A check whose failure makes everything after it meaningless."""
        if not self.check(condition, message):
            raise SystemExit(self.report())
        return True

    def report(self):
        if self.failures:
            print(f"\nFAIL — {self.failures} check(s) failed", flush=True)
            return 1
        print("\nPASS — a foreign LE Audio source drove simble's sink", flush=True)
        return 0


def render_music(seconds, rate):
    """A plucked-string arpeggio over a I-V-vi-IV progression, with a bass
    line. Sine beeps made it impossible to tell good playback from bad --
    harmonics and note envelopes make artifacts obvious to the ear."""

    def hz(midi):
        return 440.0 * 2 ** ((midi - 69) / 12.0)

    # C major, the familiar pop progression: C - G - Am - F.
    chords = [[60, 64, 67], [55, 59, 62], [57, 60, 64], [53, 57, 60]]
    bass = [36, 31, 33, 29]
    melody = [72, 76, 74, 72, 79, 76, 74, 71]

    total = int(seconds * rate)
    buf = [0.0] * total
    beat = rate // 4                 # 16th notes at 240 bpm feel
    bar = beat * 8

    def pluck(start, midi, amp, decay, harmonics=(1.0, 0.45, 0.22)):
        f = hz(midi)
        length = min(int(rate * decay * 3), total - start)
        if length <= 0:
            return
        for i in range(length):
            t = i / rate
            env = math.exp(-t / decay)
            v = 0.0
            for h, ha in enumerate(harmonics, start=1):
                v += ha * math.sin(2 * math.pi * f * h * t)
            buf[start + i] += amp * env * v

    n_bars = total // bar + 1
    for b in range(n_bars):
        chord = chords[b % len(chords)]
        pluck(b * bar, bass[b % len(bass)], 0.30, 0.55, (1.0, 0.30))
        for step in range(8):        # arpeggio
            at = b * bar + step * beat
            if at >= total:
                break
            pluck(at, chord[step % 3] + (12 if step >= 4 else 0), 0.16, 0.28)
        for half in range(2):        # melody, one note per half bar
            at = b * bar + half * beat * 4
            if at >= total:
                break
            pluck(at, melody[(b * 2 + half) % len(melody)], 0.22, 0.42)

    peak = max(1e-9, max(abs(v) for v in buf))
    scale = 0.72 / peak              # headroom, so nothing clips
    return [int(max(-32768, min(32767, v * scale * 32767))) for v in buf]


async def main():
    parser = bumble_link.transport_argument(
        argparse.ArgumentParser(description=__doc__)
    )
    parser.add_argument(
        "target", nargs="?", default="CC:1E:57:00:00:06/P", help="the simble sink"
    )
    parser.add_argument("seconds", nargs="?", type=int, default=20)
    parser.add_argument(
        "--pcm",
        action="store_true",
        help="send raw int16 instead of LC3 — a control for isolating codec "
        "bugs from audio-graph bugs. Set the page's codec selector to match.",
    )
    args = parser.parse_args()

    # Bumble's controller models CIG/CIS and ISO data paths, so the *source*
    # half of this would run — but the peer here is the browser page
    # (`web/audio/`), not a binary this script can launch onto a hosted link.
    # Until a headless simble LE-audio sink example exists there is nothing
    # to point at, so say so rather than skip silently or pass emptily.
    if args.transport == "bumble":
        print(
            "SKIP — this script's peer is the browser page `web/audio/`, not a\n"
            "      binary, so there is nothing to attach to a hosted Bumble\n"
            "      link. Bumble's controller does model CIG/CIS and ISO data\n"
            "      paths, so this becomes convertible as soon as a headless\n"
            "      simble LE-audio sink example exists.\n"
            "      Run it with --transport netsim against the browser page.",
            flush=True,
        )
        return bumble_link.SKIP

    sdu_bytes = 320 if args.pcm else FRAME_BYTES
    verdict = Verdict()

    t = await open_transport("tcp-client:127.0.0.1:6402")
    d = Device.with_hci("lea-source", "F0:F1:F2:F3:F4:D1", t.source, t.sink)
    d.cis_enabled = True
    await d.power_on()
    conn = await d.connect(args.target)
    print(f"connected to {args.target}, acl handle {hex(conn.handle)}")

    peer = Peer(conn)
    await peer.discover_services()
    for s in peer.services:
        await s.discover_characteristics()
    cp = peer.get_characteristics_by_uuid(ASE_CP)
    ase = peer.get_characteristics_by_uuid(SINK_ASE)

    # Was `if not cp: print(...); return` — a clean exit 0 on the single most
    # interesting failure this script can see.
    verdict.fatal(bool(cp), "the sink publishes an ASE Control Point (ASCS)")
    verdict.fatal(bool(ase), "the sink publishes a Sink ASE characteristic")

    # Configure the endpoint the way Android does.
    await cp[0].write_value(bytes([0x01,0x01, 0x01,0x02,0x02, 0x06,0,0,0,0, 0x10,
        0x02,0x01,0x03, 0x02,0x02,0x01, 0x05,0x03,0x01,0,0,0, 0x03,0x04,0x28,0x00]),
        with_response=True); await asyncio.sleep(0.5)
    await cp[0].write_value(bytes([0x02,0x01, 0x01, 0x01,0x01, 0x10,0x27,0x00, 0x00,
        0x02, 0x28,0x00, 0x02, 0x0A,0x00, 0x40,0x9C,0x00]), with_response=True)
    await asyncio.sleep(0.5)
    await cp[0].write_value(bytes([0x03,0x01, 0x01, 0x04, 0x03,0x02,0x04,0x00]),
        with_response=True); await asyncio.sleep(0.5)

    state = await ase[0].read_value()
    observed = state[1]
    verdict.check(
        observed == ASE_STATE_ENABLING,
        f"the ASE is in Enabling (0x{ASE_STATE_ENABLING:02X}) after Enable, "
        f"per ASCS §5.3 — read back 0x{observed:02X} "
        f"({ASE_STATE_NAMES.get(observed, 'unknown')})",
    )

    # Real CIS.
    handles = await d.setup_cig(CigParameters(
        cig_id=1,
        cis_parameters=[CigParameters.CisParameters(cis_id=1, max_sdu_c_to_p=sdu_bytes, max_sdu_p_to_c=0)],
        sdu_interval_c_to_p=DUR_US, sdu_interval_p_to_c=DUR_US,
        max_transport_latency_c_to_p=10, max_transport_latency_p_to_c=10))
    links = await d.create_cis([(handles[0], conn)])
    link = links[0]
    # `create_cis` returning is not the same as a CIS existing: assert a
    # usable handle, and one that is not just the ACL handle echoed back.
    verdict.fatal(
        link is not None and 0 <= link.handle <= MAX_CONNECTION_HANDLE,
        f"the CIS has a valid connection handle: {hex(link.handle)}",
    )
    verdict.check(
        link.handle != conn.handle,
        f"the CIS handle {hex(link.handle)} is distinct from the ACL handle "
        f"{hex(conn.handle)}",
    )
    await link.setup_data_path(direction=0)
    print("ISO data path open — streaming liblc3 audio")

    # Encode everything up front: encoding inside the send loop pushed each
    # iteration past the 10 ms SDU interval, so the sink was starved of about
    # 12% of its audio and underran continuously (scratchy playback).
    enc = lc3.Encoder(DUR_US, RATE)
    n = enc.get_frame_samples()
    print(f"rendering {args.seconds}s of music at {RATE} Hz...")
    samples = render_music(args.seconds, RATE)
    frames = []
    for f in range(len(samples) // n):
        raw = struct.pack(f"<{n}h", *samples[f * n:(f + 1) * n])
        frames.append(raw if args.pcm else enc.encode(raw, FRAME_BYTES, bit_depth=16))
    kind = "raw PCM" if args.pcm else "liblc3"
    print(f"encoded {len(frames)} {kind} frames, streaming in real time...")
    verdict.fatal(len(frames) > 0, f"there is audio to stream: {len(frames)} frames")

    # Pace against a fixed deadline rather than sleeping a fixed amount:
    # sleep(0.01) in a loop accumulates every scheduling overshoot, so the
    # stream drifts steadily behind the clock it is supposed to track.
    interval = DUR_US / 1_000_000
    start = time.monotonic()
    for i, frame in enumerate(frames):
        link.write(frame)
        delay = start + (i + 1) * interval - time.monotonic()
        if delay > 0:
            await asyncio.sleep(delay)
    elapsed = time.monotonic() - start
    rate = len(frames) / elapsed
    expected = 1.0 / interval
    print(f"=== streamed {len(frames)} frames ({len(frames) * interval:.1f}s of audio) "
          f"in {elapsed:.1f}s wall clock -> {rate:.1f} SDUs/s")
    verdict.check(
        abs(rate - expected) / expected <= PACING_TOLERANCE,
        f"the stream kept real time: {rate:.1f} SDUs/s, within "
        f"{PACING_TOLERANCE:.0%} of the {expected:.0f}/s a {interval * 1000:.0f} ms "
        f"SDU interval requires",
    )
    await asyncio.sleep(1)
    return verdict.report()


if __name__ == "__main__":
    sys.exit(asyncio.run(asyncio.wait_for(main(), 180)))
