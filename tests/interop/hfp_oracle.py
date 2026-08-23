"""Drive simble's Hands-Free side against *Bumble's* Audio Gateway.

Simble's own HFP tests answer one half of the profile with the other half.
That proves the two halves agree; it proves nothing about the wire, and this
repo has a documented history of both fake endpoints agreeing on something a
real stack rejected. Bumble is an independent implementation of the same
spec, so pointing its `AgProtocol` at simble's `HfProtocol` is the cheapest
foreign check available.

Simble's HF runs in `examples/hfp_hf_pipe.rs` and speaks a one-line-per-
message protocol on stdio; Bumble's AG runs here, over a stand-in DLC that
just moves bytes. Nothing is mocked on either side of the AT boundary — the
bytes Bumble parses are the bytes simble wrote.

Scope, stated plainly because it is easy to overclaim: this checks the AT
layer and *only* the AT layer. That includes the Codec Connection procedure
(`AT+BCC`, `+BCS`, `AT+BCS`) which HFP requires before an audio connection —
so the exchange that decides whether there will be a SCO/eSCO link, and
which codec it carries, is verified against a foreign implementation here.
The synchronous link itself is HCI, below the DLC this test stands in for,
and is *not* exercised: for that see `src/controller/sim.rs`'s SCO tests and
the end-to-end scene in `src/device/car_kit.rs`, both of which are simble
answering simble.

Usage:
    cargo build --example hfp_hf_pipe
    python tests/interop/hfp_oracle.py [path/to/hfp_hf_pipe]
"""

import subprocess
import sys

from bumble.hfp import (
    AgConfiguration,
    AgFeature,
    AgIndicator,
    AgIndicatorState,
    AgProtocol,
    AudioCodec,
    CallHoldOperation,
    CallLineIdentification,
    HfIndicator,
)

DEFAULT_BINARY = "target/debug/examples/hfp_hf_pipe"


class PipeDlc:
    """The narrowest thing `AgProtocol` will accept as an RFCOMM DLC: it
    needs `sink` set on it and `write` called. Framing and flow control are
    RFCOMM's business, and this test is about the AT layer above it."""

    def __init__(self):
        self.sink = None
        self.written = bytearray()

    def write(self, data):
        self.written.extend(data.encode() if isinstance(data, str) else data)

    def take(self):
        out = bytes(self.written)
        self.written.clear()
        return out


def indicator(kind, values, status=0):
    return AgIndicatorState(indicator=kind, supported_values=set(values), current_status=status)


def phone_configuration():
    """The same feature set and — crucially — the same indicator *order* as
    `car_kit::phone_configuration`. `+CIEV` names an indicator by its
    one-based position in the `+CIND=?` list and by nothing else, so a
    reordering here would silently change what every notification means."""
    return AgConfiguration(
        supported_ag_features=[
            AgFeature.THREE_WAY_CALLING,
            AgFeature.IN_BAND_RING_TONE_CAPABILITY,
            AgFeature.REJECT_CALL,
            AgFeature.ENHANCED_CALL_STATUS,
            AgFeature.EXTENDED_ERROR_RESULT_CODES,
            AgFeature.CODEC_NEGOTIATION,
            AgFeature.HF_INDICATORS,
            AgFeature.ESCO_S4_SETTINGS_SUPPORTED,
        ],
        supported_ag_indicators=[
            indicator(AgIndicator.SERVICE, {0, 1}, 1),
            indicator(AgIndicator.CALL, {0, 1}),
            indicator(AgIndicator.CALL_SETUP, {0, 1, 2, 3}),
            indicator(AgIndicator.CALL_HELD, {0, 1, 2}),
            indicator(AgIndicator.SIGNAL, {0, 1, 2, 3, 4, 5}, 4),
            indicator(AgIndicator.ROAM, {0, 1}),
            indicator(AgIndicator.BATTERY_CHARGE, {0, 1, 2, 3, 4, 5}, 4),
        ],
        supported_hf_indicators=[HfIndicator.ENHANCED_SAFETY, HfIndicator.BATTERY_LEVEL],
        supported_ag_call_hold_operations=[
            CallHoldOperation.RELEASE_ALL_HELD_CALLS,
            CallHoldOperation.RELEASE_ALL_ACTIVE_CALLS,
            CallHoldOperation.HOLD_ALL_ACTIVE_CALLS,
            CallHoldOperation.ADD_HELD_CALL,
        ],
        supported_audio_codecs=[AudioCodec.CVSD, AudioCodec.MSBC],
    )


class HeadUnit:
    """Simble's HfProtocol, on the other end of a pipe."""

    def __init__(self, binary):
        self.process = subprocess.Popen(
            [binary],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            text=True,
            bufsize=1,
        )
        self.commands = []
        self.events = []

    def _read_turn(self):
        """Reads one turn: `> hex` and `# event` lines, ending at `.`."""
        out = bytearray()
        while True:
            line = self.process.stdout.readline()
            if not line:
                raise RuntimeError("the head unit exited")
            line = line.rstrip("\n")
            if line == ".":
                return bytes(out)
            kind, _, payload = line.partition(" ")
            if kind == ">":
                out.extend(bytes.fromhex(payload))
                self.commands.append(bytes.fromhex(payload).decode(errors="replace"))
            elif kind == "#":
                self.events.append(payload)

    def start(self):
        return self._read_turn()

    def feed(self, data):
        self.process.stdin.write(f"< {data.hex().upper()}\n")
        self.process.stdin.flush()
        return self._read_turn()

    def act(self, command):
        self.process.stdin.write(f"! {command}\n")
        self.process.stdin.flush()
        return self._read_turn()

    def close(self):
        self.process.stdin.close()
        self.process.wait(timeout=5)


def exchange(head_unit, ag, dlc, to_ag, rounds=30):
    """Shuttles bytes until neither side has anything left to say."""
    for _ in range(rounds):
        if not to_ag:
            return
        ag._read_at(to_ag)
        from_ag = dlc.take()
        to_ag = head_unit.feed(from_ag) if from_ag else b""
    raise RuntimeError("the AT dialogue did not settle")


def check(condition, message):
    if not condition:
        print(f"FAIL  {message}")
        raise SystemExit(1)
    print(f"ok    {message}")


def main():
    binary = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_BINARY
    head_unit = HeadUnit(binary)
    dlc = PipeDlc()
    ag = AgProtocol(dlc, phone_configuration())

    slc_complete = []
    ag.on(ag.EVENT_SLC_COMPLETE, lambda: slc_complete.append(True))
    answered = []
    ag.on(ag.EVENT_ANSWER, lambda: answered.append(True))
    hung_up = []
    ag.on(ag.EVENT_HANG_UP, lambda: hung_up.append(True))
    dialed = []
    ag.on(ag.EVENT_DIAL, dialed.append)
    codec_requested = []
    ag.on(ag.EVENT_CODEC_CONNECTION_REQUEST, lambda: codec_requested.append(True))
    codec_negotiated = []
    ag.on(ag.EVENT_CODEC_NEGOTIATION, codec_negotiated.append)

    try:
        exchange(head_unit, ag, dlc, head_unit.start())

        # --- the Service Level Connection ---------------------------------
        check(slc_complete, "Bumble's AG completed the SLC simble's HF drove")
        check(
            "slc-complete" in head_unit.events,
            "simble's HF also considers the SLC complete",
        )
        check(
            "ERROR" not in dlc.written.decode(errors="replace"),
            "Bumble never answered ERROR during the SLC",
        )
        check(
            ag.supported_hf_features != 0,
            f"Bumble read the HF feature bitmask: {ag.supported_hf_features:#x}",
        )
        check(
            AudioCodec.MSBC in ag.supported_audio_codecs,
            "Bumble parsed AT+BAC and saw mSBC offered",
        )
        check(
            len(ag.hf_indicators) == 2,
            f"Bumble accepted both HF indicators from AT+BIND: {list(ag.hf_indicators)}",
        )
        for expected in ("AT+BRSF=", "AT+CIND=?", "AT+CIND?", "AT+CMER=", "AT+BIND="):
            check(
                any(c.startswith(expected.rstrip("=?")) for c in head_unit.commands),
                f"simble's HF issued {expected}...",
            )

        # --- post-SLC head-unit configuration -----------------------------
        exchange(head_unit, ag, dlc, head_unit.act("raw AT+CMEE=1"))
        check(ag.cme_error_enabled, "Bumble enabled extended errors from AT+CMEE=1")
        exchange(head_unit, ag, dlc, head_unit.act("raw AT+CLIP=1"))
        check(ag.cli_notification_enabled, "Bumble enabled caller ID from AT+CLIP=1")
        exchange(head_unit, ag, dlc, head_unit.act("raw AT+VGS=9"))
        exchange(head_unit, ag, dlc, head_unit.act("raw AT+VGM=12"))

        # AT+COPS is simble's addition; Bumble has no handler for it, so the
        # honest result is Bumble answering ERROR — and simble's HF must take
        # that as a failed command rather than hanging.
        exchange(head_unit, ag, dlc, head_unit.act("operator-format"))
        check(
            any(e.startswith("command-completed AT+COPS=3,0") for e in head_unit.events),
            "simble's HF resolved AT+COPS=3,0 against a peer that does not implement it",
        )

        # --- an incoming call ---------------------------------------------
        ag.update_ag_indicator(AgIndicator.CALL_SETUP, 1)
        ag.send_ring()
        ag.send_cli_notification(CallLineIdentification(number="+15551234", type=129))
        head_unit.feed(dlc.take())
        check("ring" in head_unit.events, "simble's HF saw Bumble's RING")
        check(
            "clip +15551234" in head_unit.events,
            f"simble's HF parsed Bumble's +CLIP: {head_unit.events}",
        )
        check(
            "indicator AgIndicator.CallSetup 1" in head_unit.events
            or "indicator CallSetup 1" in head_unit.events,
            f"simble's HF applied Bumble's +CIEV to the right indicator: {head_unit.events}",
        )

        exchange(head_unit, ag, dlc, head_unit.act("answer"))
        check(answered, "Bumble's AG saw simble's ATA")
        ag.update_ag_indicator(AgIndicator.CALL, 1)
        ag.update_ag_indicator(AgIndicator.CALL_SETUP, 0)
        head_unit.feed(dlc.take())
        check(
            "indicator AgIndicator.Call 1" in head_unit.events
            or "indicator Call 1" in head_unit.events,
            "simble's HF followed the call indicator up",
        )

        # --- the codec connection procedure -------------------------------
        #
        # HFP v1.9 4.11.3, and the whole of what has to happen over AT before
        # a SCO/eSCO link can be opened. Both directions are checked: the HF
        # asking for audio, and the AG choosing the codec.
        exchange(head_unit, ag, dlc, head_unit.act("setup-audio"))
        check(
            codec_requested,
            "Bumble's AG took simble's AT+BCC as a codec connection request",
        )
        check(
            any(c.startswith("AT+BCC") for c in head_unit.commands),
            f"and it really was AT+BCC on the wire: {head_unit.commands[-3:]}",
        )

        # Now the AG's half: an unsolicited `+BCS: <codec>`, which the HF must
        # answer with a matching `AT+BCS=<codec>` before the AG opens the
        # link. An HF that answered with a *different* codec, or that treated
        # the unsolicited response as belonging to its last command, would
        # leave the AG opening a link for a codec nobody agreed to.
        ag.send_response(f"+BCS: {AudioCodec.MSBC.value}")
        exchange(head_unit, ag, dlc, head_unit.feed(dlc.take()))
        check(
            codec_negotiated and codec_negotiated[-1] == AudioCodec.MSBC,
            f"Bumble's AG completed codec negotiation on mSBC: {codec_negotiated}",
        )
        check(
            ag.active_codec == AudioCodec.MSBC,
            f"and holds mSBC as the active codec: {ag.active_codec}",
        )
        check(
            any(
                c.rstrip("\r") == f"AT+BCS={AudioCodec.MSBC.value}"
                for c in head_unit.commands
            ),
            f"simble's HF echoed the AG's codec id back, unchanged: "
            f"{head_unit.commands[-3:]}",
        )
        check(
            "codec Msbc" in head_unit.events,
            f"and simble's HF holds the same codec: {head_unit.events[-4:]}",
        )
        check(
            "audio connecting" in head_unit.events,
            f"simble's HF now expects the AG to open the link, and does not "
            f"open one itself: {head_unit.events[-4:]}",
        )

        exchange(head_unit, ag, dlc, head_unit.act("hangup"))
        check(hung_up, "Bumble's AG saw simble's AT+CHUP")

        # --- an outgoing call ---------------------------------------------
        exchange(head_unit, ag, dlc, head_unit.act("dial 5550142"))
        check(dialed, f"Bumble's AG saw simble's ATD: {dialed}")
        check(
            dialed and dialed[0].rstrip(";") == "5550142",
            f"the dialled number survived the wire: {dialed}",
        )
        check(
            dialed and dialed[0].endswith(";"),
            "simble used the voice-call form ATD<number>; that HFP 4.19.1 requires",
        )
    finally:
        head_unit.close()

    print("\nall checks passed against Bumble")


if __name__ == "__main__":
    main()
