"""Point *Bumble's* Auracast receiver at simble's **BIS broadcaster**.

The mirror of `auracast_source.py`, and the harder direction: here simble
builds the extended advertisement, the BASE, the periodic advertising train
and the BIG, and a foreign stack has to make sense of all of it. Nothing in
this crate's own tests can catch a BASE whose LTVs are in the wrong order or
a BIS whose channel allocation is wrong — simble's parser and simble's
encoder share the same misunderstanding. Bumble does not.

simble's `auracast_source` example broadcasts LC3 on netsim; Bumble's own
`auracast receive` app joins the same ether, scans, syncs, prints the BASE it
read, joins the BIG and decodes with Google's liblc3 into a raw float32 PCM
file. The verdict is threefold:

  * Bumble reported the codec configuration simble published;
  * Bumble's own packet counter moved, so SDUs crossed;
  * the decoded PCM's dominant frequency is 440 Hz on the left and 554 Hz on
    the right — the two tones simble encoded, one per BIS. A stream that
    merely arrives proves the transport; the right tone in the right channel
    proves the BASE's per-BIS channel allocation was understood.

The example is rebuilt with `--features lc3` first, deliberately: without the
codec its SDUs carry a byte pattern that LC3 decodes to *silence without an
error*, so a stale binary would sync, count packets and quietly prove nothing.

Usage:
    ~/Library/Android/sdk/emulator/netsim devices   # confirm netsimd is up
    .venv/bin/python tests/interop/auracast_sink.py
"""

import asyncio
import math
import os
import re
import struct
import sys
import tempfile

ANSI = re.compile(r"\x1b\[[0-9;]*m")

HCI = os.environ.get("SIMBLE_NETSIM_HCI", "tcp-client:127.0.0.1:6402")
NETSIM_WS = os.environ.get("SIMBLE_NETSIM_WS", "ws://127.0.0.1:7681/v1/websocket/bt")
SOURCE_BINARY = os.environ.get(
    "SIMBLE_SOURCE_BINARY", "target/debug/examples/auracast_source"
)
SOURCE_ADDRESS = "CC:1E:57:00:0B:01"
BROADCAST_ID = 0xABCDEF
SECONDS = int(sys.argv[1]) if len(sys.argv) > 1 else 20

LEFT_HZ, RIGHT_HZ, SAMPLE_RATE = 440, 554, 48000


def dominant_hz(samples, rate=SAMPLE_RATE):
    """The strongest frequency in a window, by direct correlation. A full FFT
    would need numpy; this only has to tell two notes a whole tone apart."""
    window = samples[rate // 10 : rate // 10 + rate // 2]
    best_hz, best_magnitude = 0, 0.0
    for hz in range(200, 900):
        real = sum(v * math.cos(2 * math.pi * hz * i / rate) for i, v in enumerate(window))
        imaginary = sum(
            v * math.sin(2 * math.pi * hz * i / rate) for i, v in enumerate(window)
        )
        magnitude = math.hypot(real, imaginary)
        if magnitude > best_magnitude:
            best_hz, best_magnitude = hz, magnitude
    return best_hz


async def build_source():
    """Rebuild the example with the codec on. Sharing `target/debug` with a
    featureless build means the binary on disk is whichever was compiled
    last, and the featureless one broadcasts filler."""
    build = await asyncio.create_subprocess_exec(
        "cargo", "build", "--example", "auracast_source", "--features", "lc3"
    )
    if await build.wait() != 0:
        raise SystemExit("cargo build failed")


async def main():
    await build_source()
    with tempfile.TemporaryDirectory() as workdir:
        pcm_path = os.path.join(workdir, "received.pcm")

        source = await asyncio.create_subprocess_exec(
            SOURCE_BINARY,
            f"{NETSIM_WS}?name=simble-auracast-src&address={SOURCE_ADDRESS}",
            f"{BROADCAST_ID:06X}",
            str(SECONDS + 15),
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.STDOUT,
        )
        assert source.stdout is not None
        source_lines = []

        async def pump_source():
            async for line in source.stdout:
                text = line.decode(errors="replace").rstrip()
                source_lines.append(text)
                if not text.startswith("sent "):
                    print("source |", text, flush=True)

        source_reader = asyncio.create_task(pump_source())
        # Let the advertising set, the periodic train and the BIG come up
        # before anything goes looking for them.
        await asyncio.sleep(6)
        if source.returncode is not None:
            print("FAIL — simble auracast_source exited early")
            return 1

        receiver = await asyncio.create_subprocess_exec(
            sys.executable,
            "-m",
            "bumble.apps.auracast",
            "receive",
            HCI,
            str(BROADCAST_ID),
            "--output",
            f"file:{pcm_path}",
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.STDOUT,
        )
        assert receiver.stdout is not None
        # `auracast receive` runs until the BIG terminates, and rewrites one
        # progress line with \r, so read raw rather than line by line.
        captured = bytearray()

        async def pump_receiver():
            while True:
                chunk = await receiver.stdout.read(4096)
                if not chunk:
                    return
                captured.extend(chunk)

        receiver_reader = asyncio.create_task(pump_receiver())
        await asyncio.sleep(SECONDS)
        receiver.terminate()
        await receiver.wait()
        await receiver_reader
        source.terminate()
        await source.wait()
        await source_reader

        output = ANSI.sub("", captured.decode(errors="replace")).replace("\r", "\n")
        for line in output.splitlines():
            if line.strip() and not line.startswith("RECEIVED"):
                print("bumble |", line, flush=True)

        failures = []

        # 1. Did Bumble understand what simble published?
        for expected in (
            "Broadcast ID:   11259375",
            "Sampling Frequency: 48000 hz",
            "Frame Duration:     10000 µs",
            "Frame Size:         100 bytes",
            "FRONT_LEFT",
            "FRONT_RIGHT",
        ):
            if expected not in output:
                failures.append(f"bumble never reported {expected!r} from the BASE")

        # 2. Did SDUs actually cross?
        counters = re.findall(r"RECEIVED: (\d+) bytes in (\d+) packets", output)
        packets = int(counters[-1][1]) if counters else 0
        print(f"\nbumble decoded {packets} stereo frames")
        if packets < 100:
            failures.append(f"only {packets} frames reached bumble")
        if "LC3 decoding error" in output:
            failures.append("bumble reported LC3 decoding errors")

        # 3. Is it the right audio, in the right channel?
        raw = b""
        if os.path.exists(pcm_path):
            with open(pcm_path, "rb") as f:
                raw = f.read()
        count = len(raw) // 4
        if count < SAMPLE_RATE:
            failures.append(f"only {count} PCM samples were written")
        else:
            samples = struct.unpack(f"<{count}f", raw[: count * 4])
            left = dominant_hz(samples[0::2])
            right = dominant_hz(samples[1::2])
            print(f"decoded left channel {left} Hz, right channel {right} Hz")
            if abs(left - LEFT_HZ) > 3:
                failures.append(f"left channel is {left} Hz, expected {LEFT_HZ}")
            if abs(right - RIGHT_HZ) > 3:
                failures.append(f"right channel is {right} Hz, expected {RIGHT_HZ}")

        if failures:
            for failure in failures:
                print("FAIL —", failure)
            return 1
        print("PASS — a foreign receiver decoded simble's Auracast broadcast")
        return 0


if __name__ == "__main__":
    sys.exit(asyncio.run(main()) or 0)
