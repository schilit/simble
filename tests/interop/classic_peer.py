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

Three modes, and the last two are the ones that find bugs:

    python3 tests/interop/classic_peer.py            # one SPP record
    python3 tests/interop/classic_peer.py --records 40
    python3 tests/interop/classic_peer.py --pair

`--records N` registers N extra SDP records so Bumble's answer exceeds the
L2CAP MTU and comes back in **continuation** chunks. A real phone's SDP
database is bigger than one record; a client that only handles the one-shot
case truncates silently.

`--pair` makes the client run **Secure Simple Pairing** and encrypt the link
before it queries SDP. This is the mode that converts "our two ends agree"
into "a foreign stack accepted our SSP": rootcanal runs the pairing between
the two controllers, and every step of it is a question one controller asks
its host and the *other* host has to have answered compatibly —

- simble answers **IO Capability Request** and Bumble reads the reply out of
  its own **IO Capability Response**, so the three bytes after the BD_ADDR
  have to sit where both stacks think they do;
- both hosts get a **User Confirmation Request** carrying the same six
  digits, computed by rootcanal from the ECDH exchange the two controllers
  actually ran — a number neither host chose and neither can fake;
- both get a **Link Key Notification** with the same sixteen octets, and the
  facts asserted below are read back out of *Bumble's* keystore.

With both ends claiming `DisplayYesNo` and asking for MITM protection the
model is **Numeric Comparison**, so the key type rootcanal reports is an
*authenticated* one — and the client is told to require that, which is the
assertion that fails if the model was silently downgraded to Just Works.

`--io-capability` and `--no-mitm` move the client along Core Vol 3, Part C,
Table 5.7 and change what the key type has to be. That is the strongest thing
here: simble's own `association_model()` and rootcanal's independent
implementation have to reach the same answer, and the key type is where a
disagreement shows.

    --pair                              # Numeric Comparison -> authenticated
    --pair --io-capability displayonly  # automatic confirm  -> unauthenticated
    --pair --io-capability none         # Just Works         -> unauthenticated
    --pair --client-no-mitm             # peer still asks    -> authenticated
    --pair --no-mitm                    # nobody asked       -> unauthenticated

The fourth of those is the one worth having. "Either side asking for MITM is
enough" is a rule stated in the prose of 5.2.2.6 and not visible in Table 5.7
at all, so a stack that implemented the table alone would downgrade to Just
Works there and still look correct in every other run.

This needs a controller that models **inquiry**, which rules Bumble out —
but not CI. `--transport rootcanal` starts a standalone upstream rootcanal
(a ~16 MB release binary, no Android SDK and no bazel; see
`rootcanal_link.py`) and puts both ends on it over H4/TCP. `--transport
netsim` is unchanged and stays the default.

Usage:
    scripts/fetch_rootcanal.sh                     # once
    cargo build --example classic_initiator
    python3 tests/interop/classic_peer.py --transport rootcanal --pair

    # or against a live netsimd, which is the only mode covering BIG:
    netsimd --logtostderr --no-shutdown --ws-port 7681
    ~/Library/Android/sdk/emulator/netsim devices   # confirm it is up
    python3 tests/interop/classic_peer.py --pair
"""

import argparse
import asyncio
import contextlib
import os
import sys

from bumble.core import BT_L2CAP_PROTOCOL_ID, UUID
from bumble.device import Device
from bumble.hci import Address
from bumble.pairing import PairingConfig, PairingDelegate
from bumble.rfcomm import Server as RfcommServer, make_service_sdp_records
from bumble.sdp import (
    SDP_PROTOCOL_DESCRIPTOR_LIST_ATTRIBUTE_ID,
    SDP_SERVICE_CLASS_ID_LIST_ATTRIBUTE_ID,
    SDP_SERVICE_RECORD_HANDLE_ATTRIBUTE_ID,
    DataElement,
    ServiceAttribute,
)
from bumble.transport import open_transport

import bumble_link
import rootcanal_link

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


class WatchingDelegate(PairingDelegate):
    """Bumble's half of the pairing conversation, with a notebook.

    It answers exactly as the default delegate does — yes to everything — but
    records what it was *asked*, because that is the half of the exchange the
    client cannot report on. A run where the client says "Numeric Comparison
    happened" and Bumble was never asked to compare numbers is a run where the
    client is describing its own imagination.
    """

    def __init__(self):
        super().__init__(
            io_capability=PairingDelegate.DISPLAY_OUTPUT_AND_YES_NO_INPUT
        )
        self.compared = None
        self.confirmed = 0

    async def compare_numbers(self, number, digits):
        print(f"bumble | asked to compare {number:0{digits}d}", flush=True)
        self.compared = number
        return True

    async def confirm(self, auto=False):
        print(f"bumble | asked to confirm (auto={auto})", flush=True)
        self.confirmed += 1
        return True


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


async def select_controller(stack, transport):
    """`(spec Bumble joins on, extra env the simble client is launched with)`.

    In `netsim` mode both ends join a running `netsimd`: Bumble over
    rootcanal's HCI TCP port, the client over the WebSocket frontend it
    already defaults to, so nothing about that path changes.

    In `rootcanal` mode this process starts its own upstream rootcanal and
    both ends join *it* over H4/TCP. That needs no Android SDK, which is what
    puts the inquiry path — the thing this script exists to exercise, and the
    thing Bumble's controller cannot host at all — inside CI.
    """
    if transport != "rootcanal":
        return HCI, {}
    link = await stack.enter_async_context(rootcanal_link.rootcanal_link())
    # Starting rootcanal proved a controller answers; this proves it answers
    # *inquiry*. Read from the controller's own supported-commands bitmap, so
    # a build that quietly lacks it skips here instead of failing later in a
    # way that reads like a simble bug.
    link.requires("inquiry")
    return link.bumble_transport, {"SIMBLE_HCI": link.hci_spec}


async def main(argv):
    parser = bumble_link.transport_argument(
        argparse.ArgumentParser(description=__doc__)
    )
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
    parser.add_argument(
        "--pair",
        action="store_true",
        help="run Secure Simple Pairing and encrypt the link before SDP",
    )
    parser.add_argument(
        "--io-capability",
        # KeyboardOnly is deliberately absent. Against Bumble's DisplayYesNo
        # it selects Passkey Entry, where Bumble displays six digits and the
        # client is expected to type them — and this harness has no channel
        # by which a person could carry them across. Offering the option
        # would be offering a run that can only fail.
        choices=("displayyesno", "displayonly", "none"),
        default="displayyesno",
        help="what the *client* claims it can show and type",
    )
    parser.add_argument(
        "--no-mitm",
        action="store_true",
        help="clear the MITM-protection-required bit at *both* ends, which "
        "drops the model to Just Works and the key to an unauthenticated one",
    )
    parser.add_argument(
        "--client-no-mitm",
        action="store_true",
        help="clear it on the client only. The key must still come back "
        "authenticated: one side asking is enough, which is the rule a "
        "table-only reading of Core Vol 3 Part C 5.2.2.6 misses",
    )
    args = parser.parse_args(argv)

    # This script cannot run against *Bumble*, and the reason is the whole
    # point of it. `examples/classic_initiator.rs` *starts* with an inquiry —
    # it discovers the peer rather than being told where it is — and Bumble's
    # virtual controller has no `HCI_Inquiry` handler at all, so nothing is
    # ever discoverable to it. Worse, `--inquiry-mode rssi|eir` exists
    # precisely to exercise the two result-event forms rootcanal produces and
    # Bumble does not, which is how the 0x22 and 0x2F handling bugs (and the
    # RSSI form's Class of Device offset) were found. Add the missing
    # Class of Device too: Bumble's `HCI_Connection_Request_Event` carries a
    # hardcoded `class_of_device=0`, so even the paging half could not check
    # the 0x2C0114 this script asserts. A Bumble-hosted run would therefore
    # test strictly less while looking green — so it skips instead.
    bumble_link.requires(args.transport, "inquiry")

    # Inquiry Mode selects the event the *controller* reports results in, and
    # the three layouts are not interchangeable: Class of Device sits one
    # octet further along in the standard form than in the other two.
    inquiry_event = {"standard": "02", "rssi": "22", "eir": "2F"}[args.inquiry_mode]

    async with contextlib.AsyncExitStack() as stack:
        peer_hci, simble_environment = await select_controller(stack, args.transport)
        transport = await stack.enter_async_context(await open_transport(peer_hci))
        # The address given here is Bumble's *LE* identity; a classic BD_ADDR
        # belongs to the controller, so the one that matters is read back
        # from rootcanal after power-on and handed to the client below.
        device = Device.with_hci(
            PEER_NAME, Address("F0:F1:F2:F3:F4:C1/P"), transport.source, transport.sink
        )
        device.classic_enabled = True
        device.class_of_device = PEER_CLASS_OF_DEVICE

        # DisplayYesNo at both ends, both asking for MITM protection, so the
        # controllers select Numeric Comparison rather than Just Works. The
        # delegate answers yes but writes down what it was asked.
        delegate = WatchingDelegate()
        peer_wants_mitm = not args.no_mitm
        device.pairing_config_factory = lambda _connection: PairingConfig(
            sc=True, mitm=peer_wants_mitm, bonding=True, delegate=delegate
        )

        # What Bumble's own stack saw, gathered from its events rather than
        # from anything the client says.
        seen = {"peer": None, "encrypted": False, "link_key": None}

        @device.on("connection")
        def on_connection(connection):
            print(f"bumble | connected: {connection}", flush=True)
            # The client's BD_ADDR, as *Bumble's* controller reports it. Kept
            # in full, `/P` suffix and all, because that is the form
            # `Device.on_link_key` keys the keystore on — and it is not the
            # address printed further down, which is Bumble's own.
            seen["peer"] = str(connection.peer_address)

            @connection.on("connection_encryption_change")
            def on_encryption_change():
                print(
                    f"bumble | encryption now {connection.is_encrypted}", flush=True
                )
                seen["encrypted"] = connection.is_encrypted

            @connection.on("link_key")
            def on_link_key():
                print("bumble | stored a link key for the peer", flush=True)
                seen["link_key"] = True

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
        # Which controller the client joins. Empty in netsim mode, so the
        # client keeps its own netsim default.
        environment.update(simble_environment)
        environment.update(
            SIMBLE_EXPECT_NAME=PEER_NAME,
            SIMBLE_EXPECT_COD=f"{PEER_CLASS_OF_DEVICE:06X}",
            SIMBLE_EXPECT_CHANNEL=str(channel),
            SIMBLE_SPP_PAYLOAD="hello from simble",
            SIMBLE_TIMEOUT=str(args.timeout),
            SIMBLE_INQUIRY_MODE=args.inquiry_mode,
            SIMBLE_EXPECT_INQUIRY_EVENT=inquiry_event,
        )
        if args.pair:
            # Which association model the two controllers pick follows from
            # these two settings and Bumble's DisplayYesNo + MITM. Both ends
            # DisplayYesNo asking for MITM is Numeric Comparison, which makes
            # an *authenticated* key; drop either and it falls to Just Works
            # and an unauthenticated one. Asserting the key type is what
            # catches a silent downgrade — the failure mode where pairing
            # "works" and protects nothing.
            #
            # Against Bumble's DisplayYesNo: DisplayYesNo gives Numeric
            # Comparison, which puts a person in the loop and produces an
            # authenticated key. DisplayOnly cannot answer, so its
            # confirmation is automatic and the key is not — that is Core Vol
            # 3, Part C, Table 5.7, and rootcanal's key type is the
            # third-party confirmation of it. NoInputNoOutput is Just Works
            # outright.
            #
            # And the rule the table alone does not state: **either** side
            # asking for MITM is enough. `--client-no-mitm` clears the bit on
            # the client while Bumble keeps it, and the key still has to come
            # back authenticated. `--no-mitm` clears it at both ends, and only
            # then does the model fall to Just Works.
            expect_authenticated = (
                not args.no_mitm and args.io_capability == "displayyesno"
            )
            environment.update(
                SIMBLE_PAIR="1",
                SIMBLE_IO_CAPABILITY=args.io_capability,
                SIMBLE_MITM=(
                    "0" if (args.no_mitm or args.client_no_mitm) else "1"
                ),
                SIMBLE_EXPECT_AUTHENTICATED_KEY="1" if expect_authenticated else "0",
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

        if args.pair:
            # Bumble's own view of the pairing. None of this comes from the
            # client's output: it is what Bumble's stack was asked and what
            # it stored.
            # Note what is *not* asserted: that Bumble saw an
            # "authentication complete". It never will. Bumble is the
            # responder here, and HCI_Authentication_Complete goes only to the
            # host that issued HCI_Authentication_Requested. The responder's
            # evidence that the link is secure is the link key it was handed
            # and the encryption change it saw — which is exactly what is
            # checked below, and exactly what a responder profile has to key
            # off in real life.
            for ok, message in [
                (
                    delegate.compared is not None or delegate.confirmed > 0,
                    "Bumble's pairing delegate was actually asked to approve "
                    f"(compared={delegate.compared}, confirmed={delegate.confirmed})",
                ),
                (seen["encrypted"], "Bumble saw the link encrypted"),
                (bool(seen["link_key"]), "Bumble stored a link key for simble"),
            ]:
                print(("ok    " if ok else "FAIL  ") + message)
                if not ok:
                    code = code or 1

            keys = (
                await device.keystore.get(seen["peer"]) if seen["peer"] else None
            )
            link_key = keys.link_key if keys else None
            if link_key is None:
                print(
                    f"FAIL  Bumble's keystore has no link key for {seen['peer']}"
                )
                code = code or 1
            else:
                print(
                    f"ok    Bumble's keystore holds {link_key.value.hex()} "
                    f"for {seen['peer']} (authenticated={link_key.authenticated})"
                )
                if link_key.authenticated != expect_authenticated:
                    print(
                        "FAIL  the key type disagrees with the association "
                        f"model the IO capabilities imply (expected "
                        f"authenticated={expect_authenticated})"
                    )
                    code = code or 1

        if code == 0:
            print("PASS — simble's BR/EDR initiator drove a foreign peer")
        else:
            print("FAIL — see the client output above")
        return code


if __name__ == "__main__":
    sys.exit(asyncio.run(main(sys.argv[1:])) or 0)
