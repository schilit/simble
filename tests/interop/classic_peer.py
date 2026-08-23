"""Point simble's BR/EDR **initiator** at a *Bumble* classic device.

Everything simble sends on the BR/EDR initiator path — HCI Inquiry, Remote
Name Request, Create Connection, the L2CAP client handshake, the SDP query
and the RFCOMM initiator — has only ever been answered by simble's own
simulated controller and simble's own responder. Two simble endpoints agree
with each other by construction; this repo has a documented history of both
halves agreeing on something a real stack rejected.

So: Bumble hosts a discoverable, connectable classic device on netsim with a
name, a Class of Device, an SPP record in its SDP database and an RFCOMM
echo server. `examples/classic_initiator.rs` joins the same rootcanal ether,
inquires, finds it, reads its name, pages it, queries SDP, opens the DLC on
the channel the *peer's* record named, writes, checks the echo and
disconnects. Its exit status is the verdict.

Every fact asserted is a fact only Bumble knows:

- the **name** comes from Bumble's `HCI_Write_Local_Name`, returned over the
  air by rootcanal in answer to simble's Remote Name Request;
- the **Class of Device** comes from Bumble's `HCI_Write_Class_Of_Device`,
  carried in the inquiry result;
- the **RFCOMM channel** is allocated by Bumble's `rfcomm.Server.listen()`
  and read back out of the SDP record Bumble serialised. It is deliberately
  *not* 3 — the channel every simble example hardcodes — so a client that
  guessed instead of reading the answer fails here and passes everywhere
  else.

Two modes, and the second is the one that finds bugs:

    python3 tests/interop/classic_peer.py            # one SPP record
    python3 tests/interop/classic_peer.py --records 40

`--records N` registers N extra SDP records so Bumble's answer exceeds the
L2CAP MTU and comes back in **continuation** chunks. A real phone's SDP
database is bigger than one record; a client that only handles the one-shot
case truncates silently.

Usage:
    netsimd --logtostderr --no-shutdown --ws-port 7681
    ~/Library/Android/sdk/emulator/netsim devices   # confirm it is up
    cargo build --example classic_initiator
    python3 tests/interop/classic_peer.py
"""

import argparse
import asyncio
import os
import sys

from bumble.core import BT_L2CAP_PROTOCOL_ID, UUID
from bumble.device import Device
from bumble.hci import Address
from bumble.rfcomm import Server as RfcommServer, make_service_sdp_records
from bumble.sdp import (
    SDP_PROTOCOL_DESCRIPTOR_LIST_ATTRIBUTE_ID,
    SDP_SERVICE_CLASS_ID_LIST_ATTRIBUTE_ID,
    SDP_SERVICE_RECORD_HANDLE_ATTRIBUTE_ID,
    DataElement,
    ServiceAttribute,
)
from bumble.transport import open_transport

# Bumble joins netsim over its HCI TCP port; the simble initiator joins over
# the WebSocket frontend. Both land on the same rootcanal ether.
HCI = os.environ.get("SIMBLE_NETSIM_HCI", "tcp-client:127.0.0.1:6402")
CLIENT_BINARY = os.environ.get(
    "SIMBLE_CLIENT_BINARY", "target/debug/examples/classic_initiator"
)

# The name and Class of Device Bumble puts on the air. 0x2C0114 is a computer
# major class with networking + object-transfer services: nothing like the
# 0x240404 headset every simble example uses, so an assertion that passes
# cannot be simble reading back its own constant.
PEER_NAME = "Bumble SPP Peer"
PEER_CLASS_OF_DEVICE = 0x2C0114

# The RFCOMM server channel to listen on. Not 3 — see the module docstring.
PEER_RFCOMM_CHANNEL = 7

# Serial Port Profile, the service class simble's SDP query searches for.
SPP_UUID = UUID.from_16_bits(0x1101, "SerialPort")


def filler_record(handle, channel):
    """A well-formed SPP record that is not the one the echo server listens
    on. It has to carry the Serial Port class or Bumble's `match_services`
    would leave it out of the answer entirely — the point is to make that
    answer too big for one response, and enough of these do it."""
    return [
        ServiceAttribute(
            SDP_SERVICE_RECORD_HANDLE_ATTRIBUTE_ID,
            DataElement.unsigned_integer_32(handle),
        ),
        ServiceAttribute(
            SDP_SERVICE_CLASS_ID_LIST_ATTRIBUTE_ID,
            DataElement.sequence([DataElement.uuid(SPP_UUID)]),
        ),
        ServiceAttribute(
            SDP_PROTOCOL_DESCRIPTOR_LIST_ATTRIBUTE_ID,
            DataElement.sequence(
                [
                    DataElement.sequence([DataElement.uuid(BT_L2CAP_PROTOCOL_ID)]),
                    DataElement.sequence(
                        [
                            DataElement.uuid(UUID.from_16_bits(0x0003, "RFCOMM")),
                            DataElement.unsigned_integer_8(channel),
                        ]
                    ),
                ]
            ),
        ),
    ]


class EchoPort:
    """Bumble's end of the serial port: whatever simble writes comes back."""

    def __init__(self):
        self.received = []
        self.opened = asyncio.Event()

    def attach(self, dlc):
        print(f"bumble | RFCOMM DLC open: {dlc}", flush=True)
        self.opened.set()
        dlc.sink = lambda data: self.on_data(dlc, data)

    def on_data(self, dlc, data):
        print(f"bumble | RFCOMM received {len(data)} bytes: {data!r}", flush=True)
        self.received.append(bytes(data))
        dlc.write(data)


async def main(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--records",
        type=int,
        default=0,
        help="extra SDP records, to force a continuation-state SDP answer",
    )
    parser.add_argument(
        "--inquiry-mode",
        choices=("standard", "rssi", "eir"),
        default="standard",
        help="which inquiry-result event form to ask the controller for",
    )
    parser.add_argument(
        "--timeout", type=int, default=45, help="seconds before the client gives up"
    )
    args = parser.parse_args(argv)

    # Inquiry Mode selects the event the *controller* reports results in, and
    # the three layouts are not interchangeable: Class of Device sits one
    # octet further along in the standard form than in the other two.
    inquiry_event = {"standard": "02", "rssi": "22", "eir": "2F"}[args.inquiry_mode]

    async with await open_transport(HCI) as transport:
        # The address given here is Bumble's *LE* identity; a classic BD_ADDR
        # belongs to the controller, so the one that matters is read back
        # from rootcanal after power-on and handed to the client below.
        device = Device.with_hci(
            PEER_NAME, Address("F0:F1:F2:F3:F4:C1/P"), transport.source, transport.sink
        )
        device.classic_enabled = True
        device.class_of_device = PEER_CLASS_OF_DEVICE

        port = EchoPort()
        rfcomm_server = RfcommServer(device)
        channel = rfcomm_server.listen(port.attach, channel=PEER_RFCOMM_CHANNEL)
        if channel != PEER_RFCOMM_CHANNEL:
            print(f"FAIL — could not listen on RFCOMM channel {PEER_RFCOMM_CHANNEL}")
            return 1

        records = {
            0x00010001: make_service_sdp_records(0x00010001, channel, SPP_UUID),
        }
        # Filler records share the Serial Port class so they match the same
        # search and swell the same answer; they name channels the server
        # does not listen on, so picking one instead of the real record is a
        # failure the run will catch.
        for index in range(args.records):
            handle = 0x00020000 + index
            records[handle] = filler_record(handle, 20 + (index % 10))
        device.sdp_service_records = records

        await device.power_on()
        await device.set_discoverable(True)
        await device.set_connectable(True)

        peer_address = str(device.public_address).split("/")[0]
        print(
            f"bumble | {PEER_NAME} discoverable at {peer_address}, "
            f"CoD {PEER_CLASS_OF_DEVICE:#08x}, SPP on RFCOMM channel {channel}, "
            f"{len(records)} SDP record(s)",
            flush=True,
        )

        environment = dict(os.environ)
        environment.update(
            SIMBLE_EXPECT_NAME=PEER_NAME,
            SIMBLE_EXPECT_COD=f"{PEER_CLASS_OF_DEVICE:06X}",
            SIMBLE_EXPECT_CHANNEL=str(channel),
            SIMBLE_SPP_PAYLOAD="hello from simble",
            SIMBLE_TIMEOUT=str(args.timeout),
            SIMBLE_INQUIRY_MODE=args.inquiry_mode,
            SIMBLE_EXPECT_INQUIRY_EVENT=inquiry_event,
        )
        client = await asyncio.create_subprocess_exec(
            CLIENT_BINARY,
            peer_address,
            env=environment,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.STDOUT,
        )

        async def pump_output():
            assert client.stdout is not None
            async for line in client.stdout:
                print("client |", line.decode(errors="replace").rstrip(), flush=True)

        reader = asyncio.create_task(pump_output())
        try:
            code = await asyncio.wait_for(client.wait(), timeout=args.timeout + 15)
        except asyncio.TimeoutError:
            client.kill()
            code = 1
            print("FAIL — the client never exited")
        await reader

        print(f"\nclient exited {code}")
        # Bumble's own view, which is the half the client cannot fake: the
        # bytes really did arrive at a foreign RFCOMM server.
        if not port.received:
            print("FAIL — Bumble's RFCOMM server never received anything")
            code = code or 1
        else:
            print(f"ok    Bumble's RFCOMM server received {port.received!r}")

        if code == 0:
            print("PASS — simble's BR/EDR initiator drove a foreign peer")
        else:
            print("FAIL — see the client output above")
        return code


if __name__ == "__main__":
    sys.exit(asyncio.run(main(sys.argv[1:])) or 0)
