"""Point a *Bumble* A2DP source at simble's A2DP sink over netsim.

Simble's AVDTP acceptor and its SBC decoder have only ever been driven by
simble's own initiator. Two simble endpoints agree with each other by
construction; this repo has a documented history of both halves agreeing on
something a real stack rejected.

So: `examples/a2dp_sink.rs` joins netsim as a discoverable, connectable
speaker publishing an Audio Sink SDP record, and Bumble pages it, runs the
AVDTP initiator sequence — Discover, Get_All_Capabilities, Set_Configuration,
Open, the *second* L2CAP channel on PSM 0x0019, Start — and streams RTP/SBC
into it. The sink's exit status is the verdict.

Every fact asserted is one only Bumble decides:

- the **SBC operating point** is chosen by Bumble's `MediaCodecCapabilities`
  and carried in Bumble's Set_Configuration; simble's sink answers whatever
  it is sent, so the configuration it ends up in came from the peer;
- the **media transport channel** is a second L2CAP connection Bumble opens
  on a PSM that already has one. Nothing on the wire says it is the media
  channel — simble binds it because an OPEN just succeeded. If that rule is
  wrong, media lands on an unattached CID and no frame ever decodes;
- the **RTP framing and the A2DP payload header** — sequence numbers, the
  frame count nibble, and fragmentation across packets when a frame does not
  fit the MTU — are all Bumble's `MediaPacketPump`.

The audio itself is **libsbc's**, not simble's and not Bumble's. The frames
come out of `tests/sbc_interop_test.rs`, where they are recorded as the
bitstream bluez's libsbc produced for a known signal. So a frame that decodes
at the far end is three independent implementations agreeing: libsbc wrote
it, Bumble packetised it, simble decoded it.

Two ways to run it, and they cover different things (see `bumble_link.py`):

    # the default — both ends on a live netsimd's rootcanal
    netsimd --logtostderr --no-shutdown --ws-port 7681
    ~/Library/Android/sdk/emulator/netsim devices   # confirm it is up
    cargo build --example a2dp_sink
    .venv/bin/python tests/interop/a2dp_peer.py

    # no netsim, no Android SDK — this process hosts the controller and link
    .venv/bin/python tests/interop/a2dp_peer.py --transport bumble

Nothing here needs inquiry — Bumble pages the sink at an address it is told —
so the `bumble` mode runs the whole sequence above, not a reduced one. What
it does not carry over is rootcanal's habit of dying on malformed HCI instead
of answering with an error status, and rootcanal's own ACL scheduling.
"""

import argparse
import asyncio
import contextlib
import os
import re
import sys
import tempfile

from bumble.a2dp import (
    A2DP_SBC_CODEC_TYPE,
    SbcMediaCodecInformation,
    SbcPacketSource,
    make_audio_source_service_sdp_records,
)
from bumble.avdtp import (
    AVDTP_AUDIO_MEDIA_TYPE,
    MediaCodecCapabilities,
    MediaPacketPump,
    Protocol,
)
from bumble.core import PhysicalTransport
from bumble.device import Device
from bumble.hci import Address
from bumble.transport import open_transport

import bumble_link

# Bumble joins netsim over its HCI TCP port; the simble sink joins over the
# WebSocket frontend. Both land on the same rootcanal ether.
HCI = os.environ.get("SIMBLE_NETSIM_HCI", "tcp-client:127.0.0.1:6402")
SOURCE_ADDRESS = "F0:F1:F2:F3:F4:C2"
SINK_BINARY = os.environ.get("SIMBLE_SINK_BINARY", "target/debug/examples/a2dp_sink")

# The address the simble sink puts on the air, and the one Bumble pages.
SINK_ADDRESS = os.environ.get("SIMBLE_ADDR", "F0:DE:C0:00:0C:0B")
SINK_NAME = "simble-speaker"

# Where the libsbc vectors live, and the name of the one to stream.
VECTOR_SOURCE = "tests/sbc_interop_test.rs"
VECTOR_NAME = "LIBSBC_JOINT_STEREO_FRAMES"


def libsbc_frames():
    """The libsbc-produced SBC bitstream recorded in the interop test.

    Read out of the Rust source rather than duplicated here, so there is
    exactly one copy of the vector and it cannot drift from the test that
    proves what it is.
    """
    with open(VECTOR_SOURCE, "r", encoding="utf-8") as source:
        text = source.read()
    match = re.search(
        rf"const {VECTOR_NAME}: \[u8; (\d+)\] = \[(.*?)\];", text, re.DOTALL
    )
    if not match:
        raise SystemExit(f"{VECTOR_NAME} not found in {VECTOR_SOURCE}")
    declared = int(match.group(1))
    data = bytes(int(byte, 16) for byte in re.findall(r"0x([0-9A-Fa-f]{2})", match.group(2)))
    if len(data) != declared:
        raise SystemExit(f"{VECTOR_NAME}: read {len(data)} bytes, declared {declared}")
    # Sanity: every frame must start with the SBC sync word, or the vector
    # was mis-parsed and the whole run would be meaningless.
    if data[0] != 0x9C:
        raise SystemExit(f"{VECTOR_NAME} does not begin with the SBC sync word")
    return data


def codec_capabilities():
    """44.1 kHz joint stereo, 16 blocks, 8 subbands, loudness, bitpool 53 —
    the operating point the libsbc vector was encoded at, and the one every
    phone picks. Bumble puts this in Set_Configuration; simble's sink has to
    accept it and then decode frames that match it."""
    return MediaCodecCapabilities(
        media_type=AVDTP_AUDIO_MEDIA_TYPE,
        media_codec_type=A2DP_SBC_CODEC_TYPE,
        media_codec_information=SbcMediaCodecInformation(
            sampling_frequency=SbcMediaCodecInformation.SamplingFrequency.SF_44100,
            channel_mode=SbcMediaCodecInformation.ChannelMode.JOINT_STEREO,
            block_length=SbcMediaCodecInformation.BlockLength.BL_16,
            subbands=SbcMediaCodecInformation.Subbands.S_8,
            allocation_method=SbcMediaCodecInformation.AllocationMethod.LOUDNESS,
            minimum_bitpool_value=2,
            maximum_bitpool_value=53,
        ),
    )


@contextlib.asynccontextmanager
async def bumble_source(mode):
    """Yields `(device, environment)` for the requested transport.

    `device` is Bumble's A2DP source, not yet powered on; `environment` is
    what the simble sink has to be launched with to reach the same ether. In
    `bumble` mode that is a `$SIMBLE_HCI` pointing at the controller this
    process publishes; in `netsim` mode it is just our own environment,
    because the sink finds netsim on its default port.
    """
    if mode == "bumble":
        async with bumble_link.hosted_link(SINK_ADDRESS) as hosted:
            print(f"bumble | controller+link hosted at {hosted.hci_spec}", flush=True)
            device = hosted.attach("Bumble A2DP Source", f"{SOURCE_ADDRESS}/P")
            device.classic_enabled = True
            yield device, hosted.environment()
        return

    async with await open_transport(HCI) as transport:
        device = Device.with_hci(
            "Bumble A2DP Source",
            Address(f"{SOURCE_ADDRESS}/P"),
            transport.source,
            transport.sink,
        )
        device.classic_enabled = True
        yield device, dict(os.environ)


async def main(argv):
    parser = bumble_link.transport_argument(
        argparse.ArgumentParser(description=__doc__)
    )
    parser.add_argument(
        "--frames",
        type=int,
        default=40,
        help="whole SBC frames the sink must decode before it passes",
    )
    parser.add_argument(
        "--seconds", type=float, default=6.0, help="how long to stream for"
    )
    parser.add_argument(
        "--timeout", type=int, default=45, help="seconds before the sink gives up"
    )
    args = parser.parse_args(argv)

    vector = libsbc_frames()
    # 8 frames of 119 bytes; repeat until there is comfortably more than the
    # sink is asked to decode, so the run is not a race against the pump.
    repeats = max(1, (args.frames * 8) // (len(vector) // 119) + 1)
    stream = vector * repeats
    sbc_file = tempfile.NamedTemporaryFile(suffix=".sbc", delete=False)
    sbc_file.write(stream)
    sbc_file.close()
    print(
        f"vector | {len(vector)} bytes of libsbc SBC from {VECTOR_SOURCE}, "
        f"repeated {repeats}x -> {len(stream)} bytes",
        flush=True,
    )

    code = 1
    # Bound before the `try`: the sink is now launched *inside* it (in
    # `bumble` mode it cannot start until the controller has a port), so a
    # failure before that point must not turn into a NameError in the
    # handlers and hide the real error.
    sink = None
    reader = None
    try:
        # The transport comes up first: in `bumble` mode it is what publishes
        # the controller the sink has to be told to join.
        async with bumble_source(args.transport) as (device, base_environment):
            # Start the sink before paging it: Bumble pages, so the sink has
            # to be page-scanning already or the page times out.
            environment = dict(base_environment)
            environment.update(
                A2DP_EXPECT_FRAMES=str(args.frames),
                A2DP_TIMEOUT_SECS=str(args.timeout),
                SIMBLE_ADDR=SINK_ADDRESS,
                SIMBLE_NAME=SINK_NAME,
            )
            sink, reader = await bumble_link.run_simble(
                SINK_BINARY, environment=environment, prefix="sink  "
            )
            # Give the sink time to reach its controller and enable page scan.
            await bumble_link.settle()

            device.sdp_service_records = {
                0x00010001: make_audio_source_service_sdp_records(0x00010001)
            }
            await device.power_on()
            print(f"bumble | paging {SINK_ADDRESS}", flush=True)
            connection = await device.connect(
                SINK_ADDRESS, transport=PhysicalTransport.BR_EDR
            )
            print(f"bumble | connected: {connection}", flush=True)

            protocol = await Protocol.connect(connection)
            endpoints = await protocol.discover_remote_endpoints()
            for endpoint in endpoints:
                print(f"bumble | remote endpoint: {endpoint}", flush=True)

            remote_sink = protocol.find_remote_sink_by_codec(
                AVDTP_AUDIO_MEDIA_TYPE, A2DP_SBC_CODEC_TYPE
            )
            if remote_sink is None:
                print("FAIL — Bumble found no SBC sink on simble", flush=True)
                raise SystemExit(1)
            print(f"bumble | selected sink SEID {remote_sink.seid}", flush=True)

            with open(sbc_file.name, "rb") as data:

                async def read(byte_count):
                    return data.read(byte_count)

                packet_source = SbcPacketSource(read, protocol.l2cap_channel.peer_mtu)
                pump = MediaPacketPump(packet_source.packets)
                source = protocol.add_source(codec_capabilities(), pump)
                bumble_stream = await protocol.create_stream(source, remote_sink)
                await bumble_stream.start()
                print("bumble | streaming", flush=True)
                # The sink exits the moment it has decoded what it was asked
                # for, taking its L2CAP channels with it. That is a *pass*,
                # so wait for it rather than for the clock, and tolerate a
                # teardown against a peer that has already gone.
                try:
                    code = await asyncio.wait_for(sink.wait(), timeout=args.seconds)
                except asyncio.TimeoutError:
                    code = None
                # A stop/close against a peer that has already gone waits
                # for a response that will never come, so it gets a deadline
                # of its own. Nothing here is part of the verdict.
                try:
                    await asyncio.wait_for(bumble_stream.stop(), timeout=2)
                    await asyncio.wait_for(bumble_stream.close(), timeout=2)
                    print("bumble | stream closed", flush=True)
                except (Exception, asyncio.TimeoutError) as teardown:  # noqa: BLE001
                    print(
                        f"bumble | peer already gone at teardown: {teardown!r}",
                        flush=True,
                    )

        if code is None:
            code = await asyncio.wait_for(sink.wait(), timeout=args.timeout)
    except asyncio.TimeoutError:
        if sink is not None:
            sink.kill()
        print("FAIL — the sink never exited", flush=True)
        code = 1
    except Exception as error:  # noqa: BLE001 - the verdict is the exit status
        print(f"FAIL — Bumble raised {error!r}", flush=True)
        if sink is not None and sink.returncode is None:
            sink.kill()
        code = 1
    finally:
        if reader is not None:
            await reader
        os.unlink(sbc_file.name)

    print(f"\nsink exited {code}")
    if code == 0:
        print("PASS — a foreign A2DP source streamed libsbc audio into simble\'s sink")
    return code


if __name__ == "__main__":
    result = asyncio.run(main(sys.argv[1:]))
    sys.stdout.flush()
    # `os._exit` rather than `sys.exit`: closing the netsim HCI transport can
    # block for ever once rootcanal has dropped the far end, and the verdict
    # is already decided. Nothing is buffered past the flush above.
    os._exit(result)
