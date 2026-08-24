"""Run the interop scripts with no netsim, by letting *Bumble* be the ether.

Every script in this directory used to need `netsimd` from the Android SDK,
which is why none of them ran in CI. They do not actually need netsim — they
need *a controller and a link*, and Bumble ships both:
`bumble.controller.Controller` is a virtual controller and
`bumble.link.LocalLink` is a virtual ether, the same architecture as simble's
`sim.rs` plus `Link`. Bumble's own `examples/run_controller.py` puts two
controllers on one link already.

So the harness is: build a `LocalLink`, attach the script's Bumble `Device` to
one `Controller` in-process, and publish a second `Controller` on that link
over `tcp-server:`. Bumble's `tcp-server` carries bare H4, which is exactly
what simble's existing `RootcanalTransport` speaks, so the simble example
joins with `SIMBLE_HCI=tcp:127.0.0.1:PORT` and lands on the same ether. No
netsim, no Android SDK, no new Rust transport.

    ┌──────────────┐                          ┌────────────────────┐
    │ Bumble Device│──Host──▶ Controller "C2" │                    │
    └──────────────┘                    │     │                    │
                                   LocalLink  │  all in this        │
    ┌──────────────┐                    │     │  Python process     │
    │ simble example│◀─H4/TCP─▶ Controller "C1"                    │
    └──────────────┘                          └────────────────────┘

**This is additive — netsim is still the honest default.** The two are
different controller implementations and the difference is load-bearing:

- rootcanal (what netsim runs) is the controller a real Android emulator
  uses. It *dies* on malformed HCI instead of answering with an error status,
  which is how this project learned its bytes were wrong twice, and it
  honours `Write Inquiry Mode`, which is how the two unhandled
  inquiry-result forms were found.
- Bumble's controller models **no inquiry at all** and **no BIG**, so
  anything built on either cannot run against it. `requires()` below is how a
  script says so and skips cleanly, because a green run that exercised
  nothing is worse than no run.

What Bumble's controller does model, checked against its `on_hci_*_command`
handlers at 0.0.233: LE advertising, scanning, connections and ACL; extended
and periodic advertising *parameters/data/enable*; CIG/CIS and ISO data
paths; classic `Create Connection`, `Remote Name Request`, `Write Scan
Enable` and ACL; eSCO setup. Not modelled: inquiry, BIG (`LE Create BIG`,
`LE BIG Create Sync`), periodic advertising *sync*, and classic pairing.
"""

import asyncio
import contextlib
import os
import socket
import sys

from bumble import hci
from bumble.controller import Controller
from bumble.device import Device
from bumble.host import Host
from bumble.link import LocalLink
from bumble.transport import open_transport

# Exit status for "this script cannot honestly run in this mode". Distinct
# from 0 (passed) and 1 (failed) so CI can tell "skipped" from "green".
SKIP = 77

# How long to let a simble example reach its controller and start
# page-scanning before paging it. Three seconds is plenty for a debug binary
# on a developer machine and is the historical value; a loaded CI runner can
# want more, and a page that arrives before the peer is listening fails in a
# way that looks like a protocol bug. Tunable rather than guessed.
SETTLE_SECONDS = float(os.environ.get("SIMBLE_INTEROP_SETTLE", "3"))


async def settle():
    """Waits for a just-launched simble example to be reachable."""
    await asyncio.sleep(SETTLE_SECONDS)

# What Bumble's virtual controller does not implement. A script naming one of
# these in `requires()` skips rather than passing having tested nothing.
UNMODELLED = {
    "inquiry": (
        "Bumble's controller has no HCI_Inquiry handler at all, so no "
        "device is ever discoverable to an inquiring host"
    ),
    "big": (
        "Bumble's controller implements no BIG commands (LE Create BIG, "
        "LE BIG Create Sync), so a broadcast isochronous group cannot form"
    ),
    "periodic-sync": (
        "Bumble's controller implements periodic advertising parameters, "
        "data and enable, but not LE Periodic Advertising Create Sync, so a "
        "scanner cannot sync to a train"
    ),
    "classic-pairing": (
        "Bumble's controller implements no link-key, PIN or authentication "
        "commands, so a classic link is only ever unpaired"
    ),
}


# The controllers a script can be pointed at. `rootcanal` is the standalone
# upstream binary (see `rootcanal_link.py`), which reaches the inquiry
# coverage Bumble cannot host without needing the Android SDK netsim does.
TRANSPORTS = ("netsim", "bumble", "rootcanal")


# Where each feature Bumble lacks *can* be had. Measured against both
# rootcanal builds (see `rootcanal_link.py`): the standalone upstream release
# models inquiry and periodic sync but not BIG, so only BIG still needs the
# Android SDK.
ELSEWHERE = {
    "inquiry": (
        "Run it with --transport rootcanal (a standalone upstream rootcanal, "
        "no Android SDK) or --transport netsim for this coverage."
    ),
    "big": (
        "Only netsim's bundled rootcanal implements BIG — the upstream "
        "v1.12.0 release answers LE Create BIG with Unknown HCI Command. Run "
        "it with --transport netsim (a live netsimd) for this coverage."
    ),
    "periodic-sync": (
        "Run it with --transport rootcanal or --transport netsim for this "
        "coverage."
    ),
    "classic-pairing": (
        "Run it with --transport rootcanal or --transport netsim for this "
        "coverage."
    ),
}


def transport_argument(parser, default=None):
    """Adds the `--transport` flag every convertible script takes.

    The default is netsim unless `$SIMBLE_INTEROP_TRANSPORT` overrides it, so
    running a script the way it has always been run keeps reaching rootcanal
    and keeps its rootcanal-only coverage. CI passes `--transport bumble`.
    """
    parser.add_argument(
        "--transport",
        choices=TRANSPORTS,
        default=default or os.environ.get("SIMBLE_INTEROP_TRANSPORT", "netsim"),
        help=(
            "netsim: both ends join a live netsimd's rootcanal (the default, "
            "and the only mode that covers BIG). "
            "bumble: this process hosts a Bumble virtual controller and link, "
            "and needs no netsim. "
            "rootcanal: this process starts a standalone upstream rootcanal "
            "and both ends join it over H4/TCP — no Android SDK, and it "
            "models inquiry, which Bumble does not."
        ),
    )
    return parser


def transport_from_argv(argv=None):
    """`(mode, remaining_argv)` for scripts too small to want argparse.

    Pulls a `--transport MODE` pair out of `argv` and returns the rest
    untouched, so a script that positionally parses `sys.argv[1]` keeps
    working whether or not the flag was passed.
    """
    argv = list(sys.argv[1:] if argv is None else argv)
    mode = os.environ.get("SIMBLE_INTEROP_TRANSPORT", "netsim")
    rest = []
    index = 0
    while index < len(argv):
        argument = argv[index]
        if argument == "--transport" and index + 1 < len(argv):
            mode = argv[index + 1]
            index += 2
            continue
        if argument.startswith("--transport="):
            mode = argument.split("=", 1)[1]
            index += 1
            continue
        rest.append(argument)
        index += 1
    if mode not in TRANSPORTS:
        raise SystemExit(
            f"--transport must be one of {', '.join(TRANSPORTS)}, not {mode!r}"
        )
    return mode, rest


def requires(transport, *features):
    """Exits with `SKIP` if `transport` cannot model one of `features`.

    Called at the top of a script's main. The message names the specific
    missing piece — the failure mode being avoided is a run that looks green
    because it quietly tested nothing.
    """
    if transport != "bumble":
        return
    for feature in features:
        reason = UNMODELLED.get(feature)
        if reason is None:
            raise ValueError(f"unknown controller feature {feature!r}")
        print(f"SKIP — this script needs {feature}, and {reason}.", flush=True)
        print(f"      {ELSEWHERE[feature]}", flush=True)
        sys.exit(SKIP)


def free_port():
    """An unused TCP port to publish the controller on."""
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class HostedLink:
    """A `LocalLink` with one controller published over TCP for simble.

    `hci_spec` is the value to put in the simble example's `$SIMBLE_HCI`.
    `attach()` builds a Bumble `Device` on a second controller on the same
    link — call it once per Bumble endpoint the script needs.
    """

    def __init__(self, link, port, simble_address):
        self.link = link
        self.port = port
        self.simble_address = simble_address
        self.hci_spec = f"tcp:127.0.0.1:{port}"

    def attach(self, name, address, **device_kwargs):
        """A Bumble `Device` on its own controller on this link.

        `address` is the identity Bumble puts on the air. Unlike netsim —
        where rootcanal hands out its own BD_ADDR per session and the script
        has to read `device.public_address` back after power-on — the
        address asked for here is the address used, because the controller
        is this process's own.
        """
        identity = hci.Address(address)
        controller = Controller(f"C-{name}", link=self.link)
        # The controller's own address, not just the Device's.
        #
        # `Device.power_on()` sets an LE random address but never the
        # controller's classic `public_address`, which defaults to
        # 00:00:00:00:00:00 — and `LocalLink.send_lmp_packet` labels every
        # classic LMP packet with the *sender controller's* public address.
        # Left at the default, simble is paged by 00:00:00:00:00:00, replies
        # `Accept Connection Request` for that address, and Bumble answers
        # UNKNOWN_CONNECTION_IDENTIFIER because it filed the connection under
        # the real one. The page then times out and reads like a simble bug.
        controller.public_address = identity
        controller.random_address = identity
        host = Host()
        host.controller = controller
        device = Device(name=name, address=identity, host=host, **device_kwargs)
        return device

    def environment(self, **extra):
        """The env a simble example is launched with to join this link."""
        environment = dict(os.environ)
        environment["SIMBLE_HCI"] = self.hci_spec
        environment.update({k: str(v) for k, v in extra.items()})
        return environment


@contextlib.asynccontextmanager
async def hosted_link(simble_address, port=None):
    """Yields a `HostedLink` whose TCP port simble joins as `simble_address`.

    The published controller's `public_address` is set to `simble_address`,
    so a Bumble peer on the link can page simble at a known BD_ADDR — the
    thing netsim's `address=` query parameter did.
    """
    port = port or free_port()
    async with await open_transport(f"tcp-server:_:{port}") as transport:
        link = LocalLink()
        controller = Controller(
            "C-simble",
            host_source=transport.source,
            host_sink=transport.sink,
            link=link,
        )
        # One identity in both slots, deliberately.
        #
        # simble sends `own address type: public` in LE Create Connection, so
        # Bumble's `create_le_connection` keys the peer's connection on the
        # *public* address. But `LocalLink.send_acl_data` then labels every LE
        # packet with `sender_controller.random_address` unconditionally,
        # ignoring `own_address_type` — so with the random slot left at its
        # `00:00:00:00:00:00` default the peer logs "no connection for
        # 00:00:00:00:00:00" and drops every ACL, and the run looks like a
        # simble discovery bug. Giving both slots the same address makes
        # simble's identity the same whichever slot Bumble reads. netsim does
        # not need this: there the `address=` query parameter is the one
        # identity a device has.
        identity = hci.Address(
            simble_address if "/" in simble_address else f"{simble_address}/P"
        )
        controller.public_address = identity
        controller.random_address = identity
        yield HostedLink(link, port, simble_address)


async def run_simble(binary, *args, environment=None, prefix="simble"):
    """Launches a simble example with its output prefixed onto ours.

    Returns `(process, reader_task)`; await `process.wait()` for the verdict
    and the task to drain what it printed.
    """
    process = await asyncio.create_subprocess_exec(
        binary,
        *args,
        env=environment,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.STDOUT,
    )

    async def pump():
        assert process.stdout is not None
        async for line in process.stdout:
            print(f"{prefix} |", line.decode(errors="replace").rstrip(), flush=True)

    return process, asyncio.create_task(pump())
