"""Stream real liblc3 audio to a simble LE Audio sink over a real CIS."""
import asyncio, math, struct, sys, time
import lc3
from bumble.transport import open_transport
from bumble.device import Device, Peer, CigParameters

TARGET = sys.argv[1] if len(sys.argv) > 1 else 'CC:1E:57:00:00:06/P'
RATE, DUR_US, FRAME_BYTES = 16000, 10000, 40
SECONDS = int(sys.argv[2]) if len(sys.argv) > 2 else 20
# 'pcm' sends raw int16 instead of LC3 -- a control for isolating codec bugs
# from audio-graph bugs. Set the page's codec selector to match.
PCM_MODE = 'pcm' in sys.argv[3:]
SDU_BYTES = 320 if PCM_MODE else FRAME_BYTES
ASE_CP = '00002bc6-0000-1000-8000-00805f9b34fb'
SINK_ASE = '00002bc4-0000-1000-8000-00805f9b34fb'

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
    t = await open_transport('tcp-client:127.0.0.1:6402')
    d = Device.with_hci('lea-source', 'F0:F1:F2:F3:F4:D1', t.source, t.sink)
    d.cis_enabled = True
    await d.power_on()
    conn = await d.connect(TARGET)
    print(f'connected to {TARGET}, acl handle {hex(conn.handle)}')

    peer = Peer(conn)
    await peer.discover_services()
    for s in peer.services:
        await s.discover_characteristics()
    cp = peer.get_characteristics_by_uuid(ASE_CP)
    ase = peer.get_characteristics_by_uuid(SINK_ASE)
    if not cp:
        print('!! no ASE control point — the sink has no ASCS'); return
    print('found ASCS: control point + sink ASE')

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
    print('ASE state after Enable:', hex(state[1]))

    # Real CIS.
    handles = await d.setup_cig(CigParameters(
        cig_id=1,
        cis_parameters=[CigParameters.CisParameters(cis_id=1, max_sdu_c_to_p=SDU_BYTES, max_sdu_p_to_c=0)],
        sdu_interval_c_to_p=DUR_US, sdu_interval_p_to_c=DUR_US,
        max_transport_latency_c_to_p=10, max_transport_latency_p_to_c=10))
    links = await d.create_cis([(handles[0], conn)])
    link = links[0]
    print('CIS established:', hex(link.handle))
    await link.setup_data_path(direction=0)
    print('ISO data path open — streaming liblc3 audio')

    # Encode everything up front: encoding inside the send loop pushed each
    # iteration past the 10 ms SDU interval, so the sink was starved of about
    # 12% of its audio and underran continuously (scratchy playback).
    enc = lc3.Encoder(DUR_US, RATE)
    n = enc.get_frame_samples()
    print(f'rendering {SECONDS}s of music at {RATE} Hz...')
    samples = render_music(SECONDS, RATE)
    frames = []
    for f in range(len(samples) // n):
        raw = struct.pack(f'<{n}h', *samples[f * n:(f + 1) * n])
        frames.append(raw if PCM_MODE else enc.encode(raw, FRAME_BYTES, bit_depth=16))
    kind = 'raw PCM' if PCM_MODE else 'liblc3'
    print(f'encoded {len(frames)} {kind} frames, streaming in real time...')

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
    print(f'=== streamed {len(frames)} frames ({len(frames) * interval:.1f}s of audio) '
          f'in {elapsed:.1f}s wall clock -> {len(frames) / elapsed:.1f} SDUs/s')
    await asyncio.sleep(1)

asyncio.run(asyncio.wait_for(main(), 120))
