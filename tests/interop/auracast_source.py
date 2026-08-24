"""Point simble's **BIS receiver** at a *Bumble* Auracast broadcast source.

`src/device/big_receiver.rs`'s unit tests drive the receiver with events this
crate's own broadcaster produced. That proves the state machine and nothing
about the wire: both ends are simble's, so they agree by construction. This is
the check that counts — Bumble builds the extended advertisement, the periodic
advertising train, the BASE and the BIGInfo that simble's receiver has to
parse, and encodes the LC3 it has to decode with Google's liblc3.

Bumble's own `auracast transmit` app broadcasts on netsim; simble's
`auracast_sink` example joins the same ether, scans, syncs to the periodic
train, reads the BASE, joins the BIG and counts what arrives. Its exit status
is the verdict: zero only if SDUs actually reached it.

Usage:
    ~/Library/Android/sdk/emulator/netsim devices   # confirm netsimd is up
    .venv/bin/python tests/interop/auracast_source.py
"""

import asyncio
import math
import os
import struct
import sys
import tempfile
import wave


import bumble_link

# Bumble joins netsim over its HCI TCP port; the simble sink joins over the
# WebSocket frontend. Both land on the same rootcanal ether.
HCI = os.environ.get("SIMBLE_NETSIM_HCI", "tcp-client:127.0.0.1:6402")
NETSIM_WS = os.environ.get("SIMBLE_NETSIM_WS", "ws://127.0.0.1:7681/v1/websocket/bt")
SINK_BINARY = os.environ.get(
    "SIMBLE_SINK_BINARY", "target/debug/examples/auracast_sink"
)
SINK_ADDRESS = "CC:1E:57:00:0B:02"
BROADCAST_ID = 0xABCDEF
# Auracast is a BIG — a Broadcast Isochronous Group — and Bumble's virtual
# controller implements no BIG commands at all: no `LE Create BIG` for the
# source side, no `LE BIG Create Sync` for the sink side, and no
# `LE Periodic Advertising Create Sync` to reach the train in the first
# place. It models periodic advertising *parameters, data and enable*, which
# is enough to transmit a train and not remotely enough to join one. So
# there is no Bumble-hosted run to have here, in either direction, and the
# script says so and skips rather than exiting 0 having broadcast into a
# void.
TRANSPORT, ARGV = bumble_link.transport_from_argv()
bumble_link.requires(TRANSPORT, "big")
SECONDS = int(ARGV[0]) if ARGV else 20

# 440 Hz left, 554.37 Hz right: two different notes, so a decoder that mixes
# the BISes up produces the wrong tone in the wrong channel rather than
# something that merely sounds plausible.
LEFT_HZ, RIGHT_HZ, SAMPLE_RATE = 440.0, 554.37, 48000


def write_tone(path, seconds):
    """A stereo int16 WAV for Bumble's encoder to read."""
    with wave.open(path, "wb") as w:
        w.setnchannels(2)
        w.setsampwidth(2)
        w.setframerate(SAMPLE_RATE)
        frames = bytearray()
        for i in range(SAMPLE_RATE * seconds):
            t = i / SAMPLE_RATE
            left = int(0.3 * 32767 * math.sin(2 * math.pi * LEFT_HZ * t))
            right = int(0.3 * 32767 * math.sin(2 * math.pi * RIGHT_HZ * t))
            frames += struct.pack("<hh", left, right)
        w.writeframes(bytes(frames))


async def pump(prefix, stream):
    async for line in stream:
        print(f"{prefix} |", line.decode(errors="replace").rstrip(), flush=True)


async def build_sink():
    """Rebuild the example with the codec on, so the SDU count is backed by
    frames that actually decoded."""
    build = await asyncio.create_subprocess_exec(
        "cargo", "build", "--example", "auracast_sink", "--features", "lc3"
    )
    if await build.wait() != 0:
        raise SystemExit("cargo build failed")


async def main():
    await build_sink()
    with tempfile.TemporaryDirectory() as workdir:
        wav = os.path.join(workdir, "tone.wav")
        write_tone(wav, SECONDS + 10)

        source = await asyncio.create_subprocess_exec(
            sys.executable,
            "-m",
            "bumble.apps.auracast",
            "transmit",
            HCI,
            "--broadcast-id",
            str(BROADCAST_ID),
            "--broadcast-name",
            "Bumble Auracast",
            "--input",
            f"file:{wav}",
            stdout=asyncio.subprocess.DEVNULL,
            stderr=asyncio.subprocess.DEVNULL,
        )
        # Give Bumble time to bring the advertising set, the periodic train
        # and the BIG up before anything goes looking for them.
        await asyncio.sleep(6)
        if source.returncode is not None:
            print("FAIL — bumble auracast transmit exited early")
            return 1
        print(f"bumble auracast transmit running, broadcast_id 0x{BROADCAST_ID:06X}")

        sink = await asyncio.create_subprocess_exec(
            SINK_BINARY,
            f"{NETSIM_WS}?name=simble-auracast-sink&address={SINK_ADDRESS}",
            f"{BROADCAST_ID:06X}",
            str(SECONDS),
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.STDOUT,
        )
        assert sink.stdout is not None
        reader = asyncio.create_task(pump("sink", sink.stdout))
        code = await sink.wait()
        await reader

        source.terminate()
        await source.wait()

        print(f"\nsimble sink exited {code}")
        if code == 0:
            print("PASS — simble's BIS receiver decoded a foreign Auracast source")
        else:
            print("FAIL — see the sink output above")
        return code


if __name__ == "__main__":
    sys.exit(asyncio.run(main()) or 0)
