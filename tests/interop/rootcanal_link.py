"""Run the interop scripts against the **real rootcanal**, with no netsim.

`bumble_link.py` removed netsim from four scripts by letting Bumble be both
the controller and the ether. That works right up to the point where the
thing under test is something *Bumble does not model* — inquiry, BIG,
periodic-advertising sync — which is exactly why `classic_peer.py` and the
`auracast_*.py` pair were left behind.

The controller that models those is rootcanal, and netsim is not the only way
to get it: **upstream ships prebuilt rootcanal binaries** as GitHub release
artifacts (`google/rootcanal`, ~16 MB, linux-x86_64 and macos-arm64). That
archive contains a standalone `bin/rootcanal` which serves bare H4 over TCP —
precisely what simble's `RootcanalTransport` already speaks, and what Bumble
reaches with `tcp-client:`. So both ends join the real controller with no
Android SDK, no bazel, and no new Rust transport:

    ┌───────────────┐                        ┌─────────────────────────┐
    │ Bumble Device │◀─H4/TCP─┐              │   bin/rootcanal         │
    └───────────────┘         ├──▶ hci_port  │   (the real C++ one)    │
    ┌───────────────┐         │              │   one shared LL medium  │
    │ simble example│◀─H4/TCP─┘              └─────────────────────────┘
    └───────────────┘   $SIMBLE_HCI=tcp:127.0.0.1:PORT

Each TCP connection is one device, and every connected device shares one
link — verified, not assumed: two connections, one advertising and one
scanning, and the scanner receives the advertiser's `ADV_IND`.

**The two rootcanals are not the same rootcanal.** Measured with
`Read_Local_Supported_Commands` against both, and confirmed behaviourally:

    feature                     netsim's rootcanal   upstream v1.12.0
    HCI_Inquiry                        yes                 yes
    HCI_Write_Inquiry_Mode             yes                 yes
    LE_Periodic_Adv_Create_Sync        yes                 yes
    LE_Create_BIG                      yes                 NO
    LE_BIG_Create_Sync                 yes                 NO

The upstream release answers both BIG commands with `Unknown HCI Command`
(0x01), while netsim's build answers with semantic errors (0x42, 0x12) — a
command it *has*, refusing the arguments. So inquiry comes back within reach
of CI and BIG does not, and `requires()` below reads that from the live
controller rather than from this table, so the day upstream ships BIG the
scripts start working with no edit here.

# Why this module refuses to trust a controller that answers

`rootcanal-rs`'s `rootcanal-ws` was the obvious vehicle for this and is not
used, because its `build.rs` falls back to `c/ffi_stub.c` when it can find
neither `$ROOTCANAL_LIB_DIR` nor bazel — and that stub is **not inert**. It
answers *every* command with a well-formed Command Complete carrying status
`0x00`. A liveness probe that sends `Reset` and requires an answer passes
against it, and so would a whole interop script that only ever checks exit
status. Measured against a running stub-linked `rootcanal-ws`:

    Reset                         -> Command Complete, 1 return byte (status)
    Read_BD_ADDR                  -> Command Complete, 1 return byte (status)
    Read_Local_Version            -> Command Complete, 1 return byte (status)
    Read_Local_Supported_Commands -> Command Complete, 1 return byte (status)

So [`probe`] asserts on the *content* of the answers, never on their
arrival: a real controller owes 6 address bytes, 8 version bytes and a
64-byte supported-commands bitmap. A stub cannot fake those by answering
uniformly — it would have to implement the actual tables, and then
[`Capabilities.requires`] gates on named bits *inside* that bitmap, and then
the script's own assertions still check facts only the peer stack knows.
Each layer costs real implementation, which is the property a
liveness-only check does not have.
"""

import asyncio
import contextlib
import os
import shutil
import socket
import struct
import sys

from bumble import hci

# Exit status for "this script cannot honestly run in this mode", shared with
# bumble_link so CI can tell a skip from a pass.
SKIP = 77

# Where a vendored release archive is unpacked, relative to the repo root.
VENDORED = os.path.join("third_party", "rootcanal", "bin", "rootcanal")

# The release this was verified against. Named so a CI cache key and a bug
# report can both say which controller was on the other end.
PINNED_VERSION = "1.12.0"


class ControllerError(RuntimeError):
    """The controller on the far end is not one we may honestly test against."""


class ControllerUnavailable(ControllerError):
    """The far end could not be reached — as opposed to answering wrongly.

    Separated because the two deserve opposite treatment while a controller
    is still starting: a socket that is not up yet is worth retrying, and an
    answer we refuse never is. Retrying cannot turn a stub into a controller.
    """


# ---------------------------------------------------------------------------
# Talking H4 to a controller, over either transport a controller offers.
# ---------------------------------------------------------------------------


def _command(opcode, params=b""):
    """One H4-framed HCI command packet."""
    return bytes([0x01, opcode & 0xFF, opcode >> 8, len(params)]) + params


class _TcpLink:
    """Bare H4 over TCP — rootcanal's `hci_port`, netsim's included."""

    def __init__(self, host, port, timeout):
        self.socket = socket.create_connection((host, port), timeout=timeout)

    def send(self, data):
        self.socket.sendall(data)

    def recv_event(self):
        header = self._exactly(3)
        if header[0] != 0x04:
            raise ControllerError(
                f"expected an HCI event (H4 type 0x04), got type 0x{header[0]:02x}"
            )
        return header[1], self._exactly(header[2])

    def _exactly(self, count):
        chunks = b""
        while len(chunks) < count:
            chunk = self.socket.recv(count - len(chunks))
            if not chunk:
                raise ControllerUnavailable(
                    "the controller closed the connection mid-packet — rootcanal "
                    "exits on malformed HCI rather than answering with an error"
                )
            chunks += chunk
        return chunks

    def close(self):
        self.socket.close()


class _WebSocketLink:
    """netsim's WebSocket frontend: one H4 packet per binary frame."""

    def __init__(self, url, timeout):
        from websockets.sync.client import connect

        self.connection = connect(url, open_timeout=timeout)
        self.timeout = timeout

    def send(self, data):
        self.connection.send(data)

    def recv_event(self):
        frame = bytes(self.connection.recv(timeout=self.timeout))
        if len(frame) < 3 or frame[0] != 0x04:
            raise ControllerError(f"not an H4 event frame: {frame.hex()}")
        return frame[1], frame[3:]

    def close(self):
        self.connection.close()


def _open(spec, timeout):
    """A link to whatever `spec` names — the `$SIMBLE_HCI` forms simble takes."""
    if spec.startswith(("ws://", "wss://")):
        return _WebSocketLink(spec, timeout)
    address = spec[len("tcp:"):] if spec.startswith("tcp:") else spec
    host, _, port = address.rpartition(":")
    return _TcpLink(host or "127.0.0.1", int(port), timeout)


# ---------------------------------------------------------------------------
# What the controller says it is.
# ---------------------------------------------------------------------------


class Capabilities:
    """What a live controller answered, and what that lets a script assume."""

    def __init__(self, spec, bd_addr, hci_version, manufacturer, commands):
        self.spec = spec
        self.bd_addr = bd_addr
        self.hci_version = hci_version
        self.manufacturer = manufacturer
        self.commands = commands  # the 64-byte mask as an int

    def supports(self, opcode):
        """Does the controller's own supported-commands bitmap claim `opcode`?"""
        return bool(self.commands & hci.HCI_SUPPORTED_COMMANDS_MASKS.get(opcode, 0))

    def describe(self):
        address = ":".join(f"{b:02X}" for b in reversed(self.bd_addr))
        return (
            f"{self.spec} — BD_ADDR {address}, HCI version 0x{self.hci_version:02x}, "
            f"manufacturer 0x{self.manufacturer:04x}"
        )

    def requires(self, *features):
        """Exits `SKIP` unless the *live* controller claims every feature.

        The claim is read out of the controller's own supported-commands
        bitmap, so this cannot drift from what the binary on the machine can
        actually do — which a hardcoded table of "rootcanal does BIG" would.
        """
        for feature in features:
            missing = [
                name
                for name, opcode in FEATURES[feature][0]
                if not self.supports(opcode)
            ]
            if missing:
                print(
                    f"SKIP — this script needs {feature}, and the controller at "
                    f"{self.spec} does not implement {', '.join(missing)}.",
                    flush=True,
                )
                print(f"      {FEATURES[feature][1]}", flush=True)
                sys.exit(SKIP)


# The HCI commands each named feature is made of. `requires("big")` asks the
# controller about these opcodes rather than about a version number.
FEATURES = {
    "inquiry": (
        [
            ("HCI_Inquiry", hci.HCI_INQUIRY_COMMAND),
            ("HCI_Write_Inquiry_Mode", hci.HCI_WRITE_INQUIRY_MODE_COMMAND),
            ("HCI_Remote_Name_Request", hci.HCI_REMOTE_NAME_REQUEST_COMMAND),
        ],
        "Only a rootcanal build has these; Bumble's controller has no "
        "HCI_Inquiry handler at all.",
    ),
    "big": (
        [
            ("HCI_LE_Create_BIG", hci.HCI_LE_CREATE_BIG_COMMAND),
            ("HCI_LE_BIG_Create_Sync", hci.HCI_LE_BIG_CREATE_SYNC_COMMAND),
        ],
        "netsim's bundled rootcanal implements BIG; the upstream v1.12.0 "
        "release answers both with Unknown HCI Command. Run this against a "
        "live netsimd (--transport netsim) for BIG coverage.",
    ),
    "periodic-sync": (
        [
            (
                "HCI_LE_Periodic_Advertising_Create_Sync",
                hci.HCI_LE_PERIODIC_ADVERTISING_CREATE_SYNC_COMMAND,
            )
        ],
        "A scanner cannot join a periodic train without it.",
    ),
}


def probe(spec, timeout=10.0):
    """Asks the controller at `spec` what it is, and refuses a fake.

    Raises [`ControllerError`] when the answers are structurally impossible
    for a real controller — which is what a stub-linked `rootcanal-ws` looks
    like, since it answers every command with a bare status and no return
    parameters at all.
    """
    link = _open(spec, timeout)
    try:
        def ask(name, opcode, expected):
            link.send(_command(opcode))
            event, body = link.recv_event()
            if event != 0x0E:
                raise ControllerError(
                    f"{name}: expected Command Complete (0x0e), got event 0x{event:02x}"
                )
            # Command Complete: num_packets(1) opcode(2) then return parameters.
            returned = body[3:]
            if not returned or returned[0] != 0x00:
                status = returned[0] if returned else None
                raise ControllerError(f"{name}: status 0x{status:02x}")
            payload = returned[1:]
            if len(payload) != expected:
                raise ControllerError(
                    f"{name}: answered with {len(payload)} return-parameter "
                    f"byte(s), a real controller owes {expected}.\n"
                    "  This is what a *stub* controller looks like: it "
                    "acknowledges every command with a bare status and has no "
                    "table to answer from. A green run against it would prove "
                    "nothing.\n"
                    "  If this is rootcanal-ws, its build.rs fell back to "
                    "c/ffi_stub.c because neither $ROOTCANAL_LIB_DIR nor bazel "
                    "resolved the real library."
                )
            return payload

        link.send(_command(hci.HCI_RESET_COMMAND))
        link.recv_event()

        bd_addr = ask("Read_BD_ADDR", hci.HCI_READ_BD_ADDR_COMMAND, 6)
        version = ask(
            "Read_Local_Version_Information",
            hci.HCI_READ_LOCAL_VERSION_INFORMATION_COMMAND,
            8,
        )
        commands = ask(
            "Read_Local_Supported_Commands",
            hci.HCI_READ_LOCAL_SUPPORTED_COMMANDS_COMMAND,
            64,
        )

        mask = int.from_bytes(commands, "little")
        if not mask:
            raise ControllerError(
                "Read_Local_Supported_Commands returned an all-zero bitmap: "
                "a controller that claims no commands is not a controller"
            )
        # hci_version(1) hci_revision(2) lmp_version(1) manufacturer(2)
        # lmp_subversion(2) — Core Spec Vol 4, Part E, 7.4.1.
        hci_version, _, _, manufacturer, _ = struct.unpack("<BHBHH", version)
        return Capabilities(spec, bd_addr, hci_version, manufacturer, mask)
    finally:
        link.close()


# ---------------------------------------------------------------------------
# Running one.
# ---------------------------------------------------------------------------


def find_binary():
    """The rootcanal executable, or `None`.

    `$SIMBLE_ROOTCANAL` wins, then a release archive unpacked into
    `third_party/` (what CI does), then whatever is on `$PATH`.
    """
    explicit = os.environ.get("SIMBLE_ROOTCANAL")
    if explicit:
        return explicit if os.path.exists(explicit) else None
    if os.path.exists(VENDORED):
        return VENDORED
    return shutil.which("rootcanal")


def install_hint():
    return (
        "No rootcanal binary found. Set $SIMBLE_ROOTCANAL, or fetch the "
        f"upstream release (~16 MB, no Android SDK and no bazel):\n"
        f"    scripts/fetch_rootcanal.sh        # unpacks into {VENDORED}\n"
        "Upstream builds it at "
        "https://github.com/google/rootcanal/releases"
    )


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class RootcanalLink:
    """A running rootcanal, with both ends' connection details."""

    def __init__(self, process, host, port, capabilities):
        self.process = process
        self.host = host
        self.port = port
        self.capabilities = capabilities
        # What simble's `$SIMBLE_HCI` takes, and what Bumble's
        # `open_transport` takes, for the same controller.
        self.hci_spec = f"tcp:{host}:{port}"
        self.bumble_transport = f"tcp-client:{host}:{port}"

    def requires(self, *features):
        self.capabilities.requires(*features)

    def environment(self, **extra):
        environment = dict(os.environ)
        environment["SIMBLE_HCI"] = self.hci_spec
        environment.update({k: str(v) for k, v in extra.items()})
        return environment


@contextlib.asynccontextmanager
async def rootcanal_link(timeout=15.0):
    """Starts a private rootcanal, probes it, and yields a [`RootcanalLink`].

    The probe runs *before* anything is yielded, so a script can never reach
    its assertions against a controller that would answer them vacuously.

    `$SIMBLE_ROOTCANAL_HCI` (a `HOST:PORT`) joins a controller that is already
    running instead of starting one — several scripts sharing one rootcanal,
    or a build with capabilities the local binary lacks. It is probed exactly
    as a self-started one is: nothing here trusts a controller it did not
    interrogate.
    """
    external = os.environ.get("SIMBLE_ROOTCANAL_HCI")
    if external:
        address = external[len("tcp:"):] if external.startswith("tcp:") else external
        host, _, port = address.rpartition(":")
        host, port = host or "127.0.0.1", int(port)
        capabilities = await asyncio.to_thread(probe, f"tcp:{host}:{port}", timeout)
        print(f"rootcanal | {capabilities.describe()} (already running)", flush=True)
        yield RootcanalLink(None, host, port, capabilities)
        return

    binary = find_binary()
    if binary is None:
        print(f"SKIP — {install_hint()}", flush=True)
        sys.exit(SKIP)

    port = free_port()
    process = await asyncio.create_subprocess_exec(
        binary,
        f"-hci_port={port}",
        f"-link_port={free_port()}",
        f"-link_ble_port={free_port()}",
        f"-test_port={free_port()}",
        stdout=asyncio.subprocess.DEVNULL,
        stderr=asyncio.subprocess.DEVNULL,
    )
    try:
        capabilities = await _wait_until_answering(process, port, timeout)
        print(f"rootcanal | {capabilities.describe()}", flush=True)
        yield RootcanalLink(process, "127.0.0.1", port, capabilities)
    finally:
        with contextlib.suppress(ProcessLookupError):
            process.terminate()
        with contextlib.suppress(asyncio.TimeoutError):
            await asyncio.wait_for(process.wait(), timeout=5)


async def _wait_until_answering(process, port, timeout):
    """Polls until the probe succeeds, or gives up with the reason it failed."""
    loop = asyncio.get_running_loop()
    deadline = loop.time() + timeout
    last = None
    while loop.time() < deadline:
        if process.returncode is not None:
            raise ControllerError(
                f"rootcanal exited with {process.returncode} before serving"
            )
        try:
            return await asyncio.to_thread(probe, f"tcp:127.0.0.1:{port}", 2.0)
        except ControllerUnavailable as e:
            # Not up yet. Worth another go.
            last = e
            await asyncio.sleep(0.1)
        except ControllerError:
            # An answer we refuse is final: retrying cannot turn a stub into
            # a controller.
            raise
        except OSError as e:
            last = e
            await asyncio.sleep(0.1)
    raise ControllerError(f"rootcanal did not accept a connection in {timeout}s ({last})")


def main(argv):
    """`python3 tests/interop/rootcanal_link.py [SPEC]` — vet a controller.

    With no argument it starts a private rootcanal and reports what it can
    do; with one it probes a controller already running, which is how CI
    checks that the thing it is about to test against is real.
    """
    if argv:
        try:
            capabilities = probe(argv[0])
        except ControllerError as e:
            print(f"REFUSED — {e}", flush=True)
            return 1
        print(capabilities.describe(), flush=True)
    else:
        async def run():
            async with rootcanal_link() as link:
                return link.capabilities
        try:
            capabilities = asyncio.run(run())
        except ControllerError as e:
            print(f"REFUSED — {e}", flush=True)
            return 1

    for feature, (commands, _) in FEATURES.items():
        have = [name for name, opcode in commands if capabilities.supports(opcode)]
        print(f"  {feature:16s} {len(have)}/{len(commands)} commands: {', '.join(have) or 'none'}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
