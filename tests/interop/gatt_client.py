"""Point simble's **scripted** GATT client at a *Bumble* peripheral.

`tests/central_script_test.rs` runs a scripted central against a scripted
peripheral. That proves the scripting surface and nothing about the wire:
both endpoints are simble's, so they agree with each other by construction.
This is the check that counts — Bumble is an independent implementation of
the same spec, and it builds the ATT responses simble's client has to parse.

Bumble hosts a Heart Rate peripheral; the scripted client
(`examples/scripted_central.rs`, running the catalog's `hrm_client` script)
joins the same ether, connects, discovers, subscribes and asserts on the
notifications it receives. Its exit status is the verdict.

The peripheral is built with a Characteristic User Description descriptor
placed *before* the CCCD deliberately: `value_handle + 1` is the common
layout, not a rule, and a client that assumes it subscribes to the wrong
handle. Bumble lays its descriptors out itself, so this is a foreign
witness to the descriptor discovery, not simble marking its own homework.

Two ways to run it, and they cover different things (see `bumble_link.py`):

    # the default — both ends on a live netsimd's rootcanal
    ~/Library/Android/sdk/emulator/netsim devices   # confirm netsimd is up
    cargo build --example scripted_central
    .venv/bin/python tests/interop/gatt_client.py

    # no netsim, no Android SDK — this process hosts the controller and link
    .venv/bin/python tests/interop/gatt_client.py --transport bumble

Nothing on the LE side of this script needs anything Bumble's controller
does not model, so the `bumble` mode is a full run, not a reduced one: the
same connection, the same discovery, the same descriptor layout, the same
notifications. What it does *not* carry over from netsim is rootcanal's
habit of dying on malformed HCI rather than answering with an error status.
"""

import argparse
import asyncio
import os
import struct
import sys

from bumble.device import Device
from bumble.gatt import (
    GATT_CHARACTERISTIC_USER_DESCRIPTION_DESCRIPTOR,
    GATT_HEART_RATE_MEASUREMENT_CHARACTERISTIC,
    GATT_HEART_RATE_SERVICE,
    Characteristic,
    Descriptor,
    Service,
)
from bumble.transport import open_transport

import bumble_link

# Bumble joins netsim over its HCI TCP port; the simble client joins over the
# WebSocket frontend. Both land on the same rootcanal ether.
HCI = os.environ.get("SIMBLE_NETSIM_HCI", "tcp-client:127.0.0.1:6402")
PERIPHERAL_ADDRESS = "F0:F1:F2:F3:F4:D2"
BUMBLE_ADDRESS = PERIPHERAL_ADDRESS + "/P"
CLIENT_ADDRESS = "F0:DE:C0:00:00:C1"
CLIENT_BINARY = os.environ.get(
    "SIMBLE_CLIENT_BINARY", "target/debug/examples/scripted_central"
)
CLIENT_SCRIPT = os.environ.get("SIMBLE_CLIENT_SCRIPT", "hrm_client")


def heart_rate_service():
    """A Heart Rate service whose CCCD is *not* at value_handle + 1."""
    measurement = Characteristic(
        GATT_HEART_RATE_MEASUREMENT_CHARACTERISTIC,
        Characteristic.Properties.READ | Characteristic.Properties.NOTIFY,
        Characteristic.READABLE,
        bytes([0x00, 72]),
        [
            Descriptor(
                GATT_CHARACTERISTIC_USER_DESCRIPTION_DESCRIPTOR,
                Descriptor.READABLE,
                b"bpm",
            )
        ],
    )
    return Service(GATT_HEART_RATE_SERVICE, [measurement]), measurement


async def drive(device, measurement, seconds, environment):
    """Powers the peripheral on, runs the simble client against it, and
    notifies a changing heart rate for as long as it runs."""
    await device.power_on()
    await device.start_advertising(auto_restart=True)
    print(f"bumble peripheral advertising as {PERIPHERAL_ADDRESS}", flush=True)

    client, reader = await bumble_link.run_simble(
        CLIENT_BINARY,
        CLIENT_SCRIPT,
        PERIPHERAL_ADDRESS,
        str(seconds),
        CLIENT_ADDRESS,
        environment=environment,
        prefix="client",
    )

    async def beat():
        """Notify a changing heart rate, so the client's assertions have
        something to be true or false about."""
        bpm = 60
        while True:
            await asyncio.sleep(0.25)
            bpm = 60 + (bpm + 1) % 40
            await device.notify_subscribers(measurement, struct.pack("BB", 0x00, bpm))

    beats = asyncio.create_task(beat())
    code = await client.wait()
    beats.cancel()
    await reader

    print(f"\nscripted client exited {code}")
    if code == 0:
        print("PASS — simble's scripted central drove a foreign peripheral")
    else:
        print("FAIL — see the client output above")
    return code


async def main():
    parser = bumble_link.transport_argument(argparse.ArgumentParser())
    parser.add_argument("seconds", nargs="?", type=int, default=15)
    args = parser.parse_args()

    service, measurement = heart_rate_service()

    if args.transport == "bumble":
        async with bumble_link.hosted_link(CLIENT_ADDRESS) as hosted:
            device = hosted.attach("bumble-hrm", BUMBLE_ADDRESS)
            device.add_service(service)
            print(f"bumble controller+link hosted at {hosted.hci_spec}", flush=True)
            return await drive(
                device, measurement, args.seconds, hosted.environment()
            )

    async with await open_transport(HCI) as transport:
        device = Device.with_hci(
            "bumble-hrm", BUMBLE_ADDRESS, transport.source, transport.sink
        )
        device.add_service(service)
        return await drive(device, measurement, args.seconds, dict(os.environ))


if __name__ == "__main__":
    sys.exit(asyncio.run(main()) or 0)
