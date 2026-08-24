"""Point *Bumble's* AVRCP at simble's, in both directions, over netsim.

Simble's AVRCP is 4 300 lines that no foreign stack had ever spoken to.
`tests/avrcp_test.rs` drives two simble `avrcp::Protocol`s back to back, which
proves the codec agrees with itself; this repo has a documented history of
both halves agreeing on something a real stack rejected.

Two phases, and they are not the same test:

**Phase 1 — Bumble controls simble.** `examples/avrcp_remote.rs` joins netsim
as a discoverable, connectable phone publishing an AVRCP Target SDP record.
Bumble pages it, opens PSM 0x0017, and drives it: PLAY, PAUSE, a
GetCapabilities, a GetPlayStatus, a GetElementAttributes, a
RegisterNotification. Every assertion here is made **on Bumble's side**, out
of objects Bumble parsed from simble's bytes — the track title, the play
status, the supported event list. The simble process asserts the mirror fact
(that AV/C operation IDs it never constructed arrived) and its exit status is
the other half of the verdict.

**Phase 2 — simble controls Bumble.** Bumble hosts an AVRCP *target* with a
`Delegate`, page-scanning. Simble pages it, opens AVCTP and sends
GetCapabilities, SetAbsoluteVolume and a PASS THROUGH. The fact asserted is
foreign state: `delegate.volume` on the Bumble side has to become the number
simble sent. Nothing in simble can move that field except bytes Bumble
understood.

What phase 2 also documents is a **limit of the oracle**: Bumble's AVRCP
target implements exactly three commands — GetCapabilities,
SetAbsoluteVolume and RegisterNotification — and answers everything else
REJECTED(INVALID_PARAMETER). So simble's GetPlayStatus and
GetElementAttributes *cannot* be verified against Bumble in that direction,
and the REJECTED is the expected answer rather than a failure. Said out loud
because a run that quietly skipped them would look like a run that passed
them.

Neither phase exercises **fragmentation**. Bumble's `send_avrcp_response` and
its AVCTP `send_message` both carry a literal `# TODO: fragmentation`, and its
controller never sends `RequestContinuingResponse`, so a spec-correct
fragmented response from simble would be dropped on Bumble's floor. The
metadata here is deliberately kept inside one AV/C frame;
`tests/avrcp_continuation_test.rs` covers the fragmented path in simulation.

Usage:
    netsimd --logtostderr --no-shutdown --ws-port 7681
    ~/Library/Android/sdk/emulator/netsim devices   # confirm it is up
    cargo build --example avrcp_remote
    .venv/bin/python tests/interop/avrcp_peer.py
"""

import argparse
import asyncio
import os
import sys

from bumble import avc, avrcp
from bumble.core import PhysicalTransport
from bumble.device import Device
from bumble.hci import Address
from bumble.transport import open_transport

# Bumble joins netsim over its HCI TCP port; the simble side joins over the
# WebSocket frontend. Both land on the same rootcanal ether.
HCI = os.environ.get("SIMBLE_NETSIM_HCI", "tcp-client:127.0.0.1:6402")
BINARY = os.environ.get("SIMBLE_AVRCP_BINARY", "target/debug/examples/avrcp_remote")

# The address simble puts on the air, and the one Bumble pages in phase 1.
SIMBLE_ADDRESS = os.environ.get("SIMBLE_ADDR", "F0:DE:C0:00:0A:1C")
SIMBLE_NAME = "simble-player"

# Bumble's *requested* classic address for phase 2. rootcanal hands out its
# own BD_ADDR per session and ignores this, so the address simble is told to
# page is read back out of `device.public_address` after power-on — the same
# thing `classic_peer.py` has to do.
BUMBLE_ADDRESS = "F0:F1:F2:F3:F4:AC"

# The track simble is told to serve. Deliberately not a placeholder: a title
# read back on the Bumble side has to be one that could only have come across
# the link.
TITLE = "Careful With That Axe"
ARTIST = "Simble Ensemble"

# The volume phase 2 asks Bumble to set. Not 0, not 127, not the default —
# a number that cannot be an uninitialised field.
VOLUME = 0x53


async def stream_output(process, prefix):
    assert process.stdout is not None
    async for line in process.stdout:
        print(f"{prefix} |", line.decode(errors="replace").rstrip(), flush=True)


def fail(message):
    print(f"FAIL — {message}", flush=True)
    return False


# ---------------------------------------------------------------------------
# Phase 1: Bumble's controller drives simble's target
# ---------------------------------------------------------------------------


async def phase_one(transport, args):
    """Bumble pages simble's AVRCP target and drives its media player."""
    environment = dict(os.environ)
    environment.update(
        AVRCP_ROLE="target",
        AVRCP_EXPECT_KEYS="44,46",  # PLAY, PAUSE
        AVRCP_TITLE=TITLE,
        AVRCP_ARTIST=ARTIST,
        AVRCP_TIMEOUT_SECS=str(args.timeout),
        SIMBLE_ADDR=SIMBLE_ADDRESS,
        SIMBLE_NAME=SIMBLE_NAME,
    )
    simble = await asyncio.create_subprocess_exec(
        BINARY,
        env=environment,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.STDOUT,
    )
    reader = asyncio.create_task(stream_output(simble, "simble"))
    # Give it time to reach netsim and enable page scan.
    await asyncio.sleep(3)

    ok = True
    try:
        device = Device.with_hci(
            "Bumble AVRCP Controller",
            Address("F0:F1:F2:F3:F4:AB/P"),
            transport.source,
            transport.sink,
        )
        device.classic_enabled = True
        device.sdp_service_records = {
            0x00010001: avrcp.ControllerServiceSdpRecord(
                0x00010001
            ).to_service_attributes()
        }
        await device.power_on()
        print(f"bumble | paging {SIMBLE_ADDRESS}", flush=True)
        connection = await device.connect(
            SIMBLE_ADDRESS, transport=PhysicalTransport.BR_EDR
        )
        print(f"bumble | connected: {connection}", flush=True)

        protocol = avrcp.Protocol()
        await protocol.connect(connection)
        print("bumble | AVCTP control channel open", flush=True)

        # --- facts only simble's target can answer ---

        events = await asyncio.wait_for(protocol.get_supported_events(), timeout=10)
        print(f"bumble | simble supports events: {[e.name for e in events]}", flush=True)
        if avrcp.EventId.PLAYBACK_STATUS_CHANGED not in events:
            ok = fail("simble did not advertise PLAYBACK_STATUS_CHANGED")

        status = await asyncio.wait_for(protocol.get_play_status(), timeout=10)
        print(f"bumble | simble play status: {status}", flush=True)
        if status.play_status != avrcp.PlayStatus.PLAYING:
            ok = fail(f"expected PLAYING, Bumble parsed {status.play_status!r}")
        if status.song_length != 213000:
            ok = fail(f"expected a 213000 ms track, Bumble parsed {status.song_length}")

        attributes = await asyncio.wait_for(
            protocol.get_element_attributes(0, []), timeout=10
        )
        titles = {
            attribute.attribute_id: attribute.attribute_value
            for attribute in attributes
        }
        print(f"bumble | simble track metadata: {titles}", flush=True)
        if titles.get(avrcp.MediaAttributeId.TITLE) != TITLE:
            ok = fail(f"Bumble read the title as {titles.get(avrcp.MediaAttributeId.TITLE)!r}")
        if titles.get(avrcp.MediaAttributeId.ARTIST_NAME) != ARTIST:
            ok = fail("Bumble read the wrong artist")

        # --- a notification registration, and the CHANGED that follows ---

        # `monitor_playback_status` yields a PlayStatus per notification: the
        # INTERIM snapshot first, then one per CHANGED.
        monitor = protocol.monitor_playback_status()
        first = await asyncio.wait_for(monitor.__anext__(), timeout=10)
        print(f"bumble | interim playback status: {first!r}", flush=True)
        if first != avrcp.PlayStatus.PLAYING:
            ok = fail(f"the INTERIM snapshot said {first!r}, expected PLAYING")

        # --- transport keys, which is what a remote control is for ---

        for key, name in (
            (avc.PassThroughFrame.OperationId.PLAY, "PLAY"),
            (avc.PassThroughFrame.OperationId.PAUSE, "PAUSE"),
        ):
            for pressed in (True, False):
                response = await asyncio.wait_for(
                    protocol.send_key_event(key, pressed), timeout=10
                )
                print(
                    f"bumble | {name} {'press' if pressed else 'release'} -> "
                    f"{response.response.name}",
                    flush=True,
                )
                if response.response != avc.ResponseFrame.ResponseCode.ACCEPTED:
                    ok = fail(f"simble did not ACCEPT {name}")

        # PAUSE was the last key; simble's player must have moved.
        changed = await asyncio.wait_for(monitor.__anext__(), timeout=10)
        print(f"bumble | playback status changed to: {changed!r}", flush=True)
        if changed != avrcp.PlayStatus.PAUSED:
            ok = fail(
                "simble's CHANGED notification did not say PAUSED after Bumble's PAUSE"
            )

        code = await asyncio.wait_for(simble.wait(), timeout=args.timeout)
        print(f"bumble | simble exited {code}", flush=True)
        if code != 0:
            ok = fail("the simble target's own checks failed")
        await connection.disconnect()
    except Exception as error:  # noqa: BLE001 - the verdict is the exit status
        ok = fail(f"Bumble raised {error!r}")
    finally:
        if simble.returncode is None:
            simble.kill()
        await reader
    return ok


# ---------------------------------------------------------------------------
# Phase 2: simble's controller drives Bumble's target
# ---------------------------------------------------------------------------


async def phase_two(transport, args):
    """Bumble hosts an AVRCP target; simble pages it and sets its volume."""
    delegate = avrcp.Delegate(
        [
            avrcp.EventId.VOLUME_CHANGED,
            avrcp.EventId.PLAYBACK_STATUS_CHANGED,
        ]
    )
    device = Device.with_hci(
        "Bumble AVRCP Target",
        Address(f"{BUMBLE_ADDRESS}/P"),
        transport.source,
        transport.sink,
    )
    device.classic_enabled = True
    device.sdp_service_records = {
        0x00010002: avrcp.TargetServiceSdpRecord(0x00010002).to_service_attributes()
    }
    protocol = avrcp.Protocol(delegate)
    protocol.listen(device)
    await device.power_on()
    await device.set_discoverable(True)
    await device.set_connectable(True)
    peer_address = str(device.public_address).split("/")[0]
    print(f"bumble | AVRCP target listening at {peer_address}", flush=True)

    environment = dict(os.environ)
    environment.update(
        AVRCP_ROLE="controller",
        AVRCP_PEER=peer_address,
        AVRCP_EXPECT_VOLUME=str(VOLUME),
        AVRCP_TIMEOUT_SECS=str(args.timeout),
        SIMBLE_ADDR="F0:DE:C0:00:0A:1D",
        SIMBLE_NAME="simble-head-unit",
    )
    simble = await asyncio.create_subprocess_exec(
        BINARY,
        env=environment,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.STDOUT,
    )
    reader = asyncio.create_task(stream_output(simble, "simble"))

    ok = True
    try:
        code = await asyncio.wait_for(simble.wait(), timeout=args.timeout + 10)
        print(f"bumble | simble exited {code}", flush=True)
        if code != 0:
            ok = fail("simble's controller did not get what it asked for")
        # The foreign fact: Bumble's own delegate holds the volume simble
        # sent. Nothing in the simble process can write this field.
        print(f"bumble | delegate volume is now {delegate.volume}", flush=True)
        if delegate.volume != VOLUME:
            ok = fail(
                f"Bumble's delegate volume is {delegate.volume}, expected {VOLUME}"
            )
    except asyncio.TimeoutError:
        ok = fail("simble's controller never exited")
    finally:
        if simble.returncode is None:
            simble.kill()
        await reader
    return ok


async def main(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--timeout", type=int, default=45, help="seconds before a phase gives up"
    )
    parser.add_argument(
        "--phase",
        choices=("1", "2", "both"),
        default="both",
        help="which direction to run",
    )
    args = parser.parse_args(argv)

    results = {}
    if args.phase in ("1", "both"):
        async with await open_transport(HCI) as transport:
            results["Bumble controller -> simble target"] = await phase_one(
                transport, args
            )
    if args.phase in ("2", "both"):
        async with await open_transport(HCI) as transport:
            results["simble controller -> Bumble target"] = await phase_two(
                transport, args
            )

    print("\n--- verdict ---")
    for label, passed in results.items():
        print(f"{'PASS' if passed else 'FAIL'} — {label}")
    return 0 if results and all(results.values()) else 1


if __name__ == "__main__":
    result = asyncio.run(main(sys.argv[1:]))
    sys.stdout.flush()
    # `os._exit` rather than `sys.exit`: closing the netsim HCI transport can
    # block for ever once rootcanal has dropped the far end, and the verdict
    # is already decided. Nothing is buffered past the flush above.
    os._exit(result)
