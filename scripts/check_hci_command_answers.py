#!/usr/bin/env python3
# Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
#
# Does src/controller/sim.rs answer every HCI command with the event the Core
# specification says answers it?
#
# There are exactly two answers a controller may give a command: Command
# Complete, which carries the result, or Command Status, which promises a
# *later* completion event. Choosing wrong is not a cosmetic error -- a host
# that sends a Command-Status-only command and gets a Command Complete waits
# forever for a completion event that will never be sent, and a host that
# sends a Command-Complete-only command and gets a Command Status waits
# forever for the same reason. Nothing crashes. Nothing logs. The scene hangs.
#
# This exact shape produced five bugs here in two weeks (BigReceiver
# terminate, CsInitiator, LE CS Procedure Enable, LE CS Remove Config, and
# every BR/EDR command had to dodge it one at a time), because sim.rs's
# catch-all answered *every* unhandled opcode with Command Complete and 61
# Core-6.3 commands are Command-Status-only. Every test in this repo has
# simble on both ends, so a wrong answer kind is invisible to all of them:
# both ends share the misunderstanding. Only an outside reference can
# disagree, and it only helps if something asks.
#
# So: derive the table, and diff it against the code.
#
#   1. The Bluetooth SIG publishes Core v6.3 as browsable HTML, ungated. Every
#      HCI command section in Vol 4 Part E ends with the literal string
#      "Event(s) generated (unless masked away):" followed by prose naming
#      HCI_Command_Status or HCI_Command_Complete. 339 command opcodes parse
#      to an answer kind.
#
#   2. Bumble (Apache-2.0) splits the same commands into HCI_AsyncCommand
#      ("answered by Command Status") and HCI_SyncCommand ("answered by
#      Command Complete"). It covers 197 of them. This project has already
#      used Bumble's tables to correct a hand-transcribed one, so it is used
#      here to cross-check the HTML scrape rather than to replace it: if the
#      two disagree, the scrape has drifted and the numbers below are not
#      trustworthy.
#
#   3. sim.rs is then checked twice over:
#        * COMMAND_STATUS_OPCODES -- the table its catch-all consults so that
#          an unmodelled command is still answered with the right *kind* --
#          must equal the derived Command-Status-only set exactly.
#        * every explicit match arm must emit only the answer kind the spec
#          assigns that opcode. This is what catches a command that is
#          implemented but answered with the wrong event.
#
# Neither source is redistributed. "HCI_LE_Set_PHY is answered by Command
# Status" is a fact, not expression; this fetches, compares, and prints drift.
# That is the position scripts/check_sig_assigned_numbers.py already takes.
#
#     python3 scripts/check_hci_command_answers.py            # report drift
#     python3 scripts/check_hci_command_answers.py --quiet     # CI: exit code
#     python3 scripts/check_hci_command_answers.py --emit-table  # regenerate
#     python3 scripts/check_hci_command_answers.py --bumble ~/src/bumble/bumble/hci.py
#
# Exit 0 = in sync, 1 = drift, 2 = could not reach a source (which is NOT a
# failure of the code under test, and CI should treat it as a skip).

import argparse
import re
import sys
import urllib.error
import urllib.request

SIG_HCI_HTML = (
    "https://www.bluetooth.com/wp-content/uploads/Files/Specification/HTML/"
    "Core_v6.3/out/en/host-controller-interface/"
    "host-controller-interface-functional-specification.html"
)
BUMBLE_HCI_PY = (
    "https://raw.githubusercontent.com/google/bumble/main/bumble/hci.py"
)

SIM = "src/controller/sim.rs"
# Where `mod opcode` in sim.rs forwards to for the opcodes it does not spell
# out as literals.
OPCODE_SOURCES = ["src/packets/hci.rs", "src/packets/big.rs", "src/packets/ext_adv.rs"]
TABLE = "COMMAND_STATUS_OPCODES"

STATUS, COMPLETE = "status", "complete"


def fetch(url):
    # bluetooth.com answers urllib's default User-Agent with 403; any ordinary
    # browser string is served the same public page.
    request = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0"})
    with urllib.request.urlopen(request, timeout=60) as response:
        return response.read().decode("utf-8")


# --- source 1: the Core specification -------------------------------------

# Each command lives in its own numbered section. The section number carries
# the OGF (7.1 Link Control = 1, 7.2 Link Policy = 2, ... 7.8 LE = 8) and the
# summary table at the top of the section carries the name and OCF -- one row
# per version, so [v1] and [v2] of the same command yield two opcodes.
_SECTION = re.compile(
    r'<span class="formal-number">\s*(7\.\d+\.\d+)\s*</span>.*?'
    r'<span class="formal-title">(.*?)</span>',
    re.S,
)
_SUMMARY_ROW = re.compile(
    r"<td[^>]*><p>(HCI_[^<]+)</p></td>\s*<td[^>]*><p>(0x[0-9A-Fa-f]{4})</p></td>"
)
_ANSWER_MARKER = "Event(s) generated"


def parse_spec(html):
    """{opcode: (command name, answer kind, section number)} from the Core HTML.

    Soft hyphens are stripped first: the published HTML breaks long event
    names across lines with U+00AD, so `HCI_Command_­Status` does not match
    `HCI_Command_Status` until they are gone. That is the one trap in this
    document and it silently halves the table if missed.
    """
    html = html.replace("\xad", "")
    marks = [(m.start(), m.group(1)) for m in _SECTION.finditer(html)]
    out = {}
    for index, (start, section) in enumerate(marks):
        end = marks[index + 1][0] if index + 1 < len(marks) else len(html)
        body = html[start:end]
        answer_at = body.find(_ANSWER_MARKER)
        if answer_at < 0:
            continue  # an event or a prose section, not a command
        rows = _SUMMARY_ROW.findall(body[:answer_at])
        if not rows:
            continue
        prose = body[answer_at:]
        kinds = set()
        if "HCI_Command_Status" in prose:
            kinds.add(STATUS)
        if "HCI_Command_Complete" in prose:
            kinds.add(COMPLETE)
        if len(kinds) != 1:
            continue  # conditional or unstated; not something to assert on
        ogf = int(section.split(".")[1])
        kind = next(iter(kinds))
        for name, ocf in rows:
            out[(ogf << 10) | int(ocf, 16)] = (name.strip(), kind, section)
    return out


# --- source 2: Bumble ------------------------------------------------------

_BUMBLE_OPCODE = re.compile(
    r"^(HCI_[A-Z0-9_]+_COMMAND)\s*=\s*hci_command_op_code\(\s*"
    r"(0x[0-9A-Fa-f]+)\s*,\s*(0x[0-9A-Fa-f]+)\s*\)",
    re.M,
)
_BUMBLE_CLASS = re.compile(
    r"^class\s+(HCI_[A-Za-z0-9_]+_Command)\s*\(\s*(HCI_AsyncCommand|HCI_SyncCommand)",
    re.M,
)


def parse_bumble(source):
    """{opcode: answer kind} from bumble/hci.py's Async/Sync command split."""
    opcodes = {
        m.group(1): (int(m.group(2), 16) << 10) | int(m.group(3), 16)
        for m in _BUMBLE_OPCODE.finditer(source)
    }
    out = {}
    for m in _BUMBLE_CLASS.finditer(source):
        name = m.group(1).upper()
        if name in opcodes:
            out[opcodes[name]] = STATUS if m.group(2) == "HCI_AsyncCommand" else COMPLETE
    return out


# --- source 3: sim.rs ------------------------------------------------------

_MOD_OPCODE = re.compile(r"\nmod opcode \{(.*?)\n\}\n", re.S)
_LITERAL = re.compile(r"pub const ([A-Z0-9_]+): u16 = (0x[0-9A-Fa-f]{4});")
_FORWARD = re.compile(
    r"pub const ([A-Z0-9_]+): u16 =\s*[a-z_]+::([A-Z0-9_]+)\.as_u16\(\);", re.S
)
_OPCODE_BYTES = re.compile(
    r"pub const ([A-Z0-9_]+): OpCode = OpCode::from_bytes\(\[\s*"
    r"(0x[0-9A-Fa-f]{2}),\s*(0x[0-9A-Fa-f]{2}),?\s*\]\);"
)
_ARM = re.compile(r"((?:opcode::[A-Z0-9_]+\s*\|\s*)*opcode::[A-Z0-9_]+)\s*=>\s*\{")
_TABLE_ENTRY = re.compile(r"0x([0-9A-Fa-f]{4})")


def read(path):
    with open(path, encoding="utf-8") as f:
        return f.read()


def opcode_names(sim):
    """`mod opcode`'s NAME -> u16, resolving the constants it forwards to."""
    forwarded = {}
    for path in OPCODE_SOURCES:
        for name, low, high in _OPCODE_BYTES.findall(read(path)):
            forwarded[name] = (int(high, 16) << 8) | int(low, 16)

    block = _MOD_OPCODE.search(sim)
    if not block:
        raise SystemExit(f"{SIM}: no `mod opcode` block -- has it been renamed?")
    body = block.group(1)
    names = {n: int(v, 16) for n, v in _LITERAL.findall(body)}
    for local, referenced in _FORWARD.findall(body):
        if referenced not in forwarded:
            raise SystemExit(f"{SIM}: opcode::{local} forwards to an unknown {referenced}")
        names[local] = forwarded[referenced]
    return names


def arm_answers(sim, names):
    """{opcode: set of answer kinds the explicit match arm can emit}.

    Found by locating every `opcode::NAME => {` (one or several alternatives)
    and brace-matching its body, so nested matches inside an arm are counted
    as part of that arm. An arm that emits neither -- one that delegates to a
    helper -- maps to the empty set and is reported, not failed.
    """
    out = {}
    for m in _ARM.finditer(sim):
        depth, i = 0, m.end() - 1
        while i < len(sim):
            if sim[i] == "{":
                depth += 1
            elif sim[i] == "}":
                depth -= 1
                if depth == 0:
                    break
            i += 1
        body = sim[m.end() : i]
        kinds = set()
        if "command_status(" in body:
            kinds.add(STATUS)
        if "command_complete(" in body:
            kinds.add(COMPLETE)
        for alternative in re.findall(r"opcode::([A-Z0-9_]+)", m.group(1)):
            if alternative in names:
                out.setdefault(names[alternative], set()).update(kinds)
    return out


def status_table(sim):
    """The opcodes listed in sim.rs's COMMAND_STATUS_OPCODES."""
    start = sim.find(f"const {TABLE}:")
    if start < 0:
        raise SystemExit(f"{SIM}: no `{TABLE}` -- the catch-all has nothing to consult")
    end = sim.index("];", start)
    return {int(v, 16) for v in _TABLE_ENTRY.findall(sim[start:end])}


# --- the check -------------------------------------------------------------


def rust_table(spec):
    lines = []
    for op, (name, kind, section) in sorted(spec.items()):
        if kind == STATUS:
            lines.append(f"    0x{op:04X}, // {section:9s} {name}")
    return "\n".join(lines)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--quiet", action="store_true", help="exit code only")
    ap.add_argument("--bumble", help="path to a local bumble/hci.py instead of fetching")
    ap.add_argument(
        "--emit-table", action="store_true", help="print the Rust table and exit"
    )
    args = ap.parse_args()

    try:
        spec = parse_spec(fetch(SIG_HCI_HTML))
        bumble = parse_bumble(read(args.bumble) if args.bumble else fetch(BUMBLE_HCI_PY))
    except (urllib.error.URLError, TimeoutError, OSError) as e:
        print(f"could not reach a source: {e}", file=sys.stderr)
        return 2

    if len(spec) < 300:
        print(
            f"only {len(spec)} commands parsed out of the Core HTML -- the "
            f"published markup has changed and this check is not trustworthy",
            file=sys.stderr,
        )
        return 2

    if args.emit_table:
        print(rust_table(spec))
        return 0

    say = (lambda *a: None) if args.quiet else print
    spec_status = {op for op, (_, kind, _) in spec.items() if kind == STATUS}

    say(f"Core v6.3: {len(spec)} commands — {len(spec_status)} answered by Command "
        f"Status, {len(spec) - len(spec_status)} by Command Complete")

    # 1. Cross-check the scrape against Bumble.
    shared = set(spec) & set(bumble)
    disagree = [op for op in sorted(shared) if spec[op][1] != bumble[op]]
    say(f"Bumble covers {len(shared)} of them — "
        f"{'all agree' if not disagree else f'{len(disagree)} DISAGREE'}")
    for op in disagree:
        say(f"  SOURCES DISAGREE  0x{op:04X}  {spec[op][0]}: "
            f"spec says {spec[op][1]}, bumble says {bumble[op]}")

    sim = read(SIM)
    names = opcode_names(sim)
    arms = arm_answers(sim, names)
    table = status_table(sim)

    # 2. sim.rs's catch-all table must be the derived set exactly.
    invented = table - spec_status
    absent = spec_status - table
    say(f"\n{SIM}: {TABLE} lists {len(table)}")
    for op in sorted(invented):
        name = spec[op][0] if op in spec else "not a command in Core v6.3"
        say(f"  NOT COMMAND-STATUS  0x{op:04X}  {name}")
    for op in sorted(absent):
        say(f"  MISSING             0x{op:04X}  {spec[op][0]}")

    # 3. Every explicit arm must emit only the kind the spec assigns it.
    wrong, delegating = [], []
    for op, kinds in sorted(arms.items()):
        if op not in spec:
            continue
        if not kinds:
            delegating.append(op)
        elif kinds != {spec[op][1]}:
            wrong.append(op)

    handled = sorted(op for op in arms if op in spec_status)
    say(f"{SIM}: {len(arms)} commands with an explicit arm, {len(handled)} of "
        f"them Command-Status-only; {len(spec_status) - len(handled)} fall to "
        f"the catch-all")
    for op in wrong:
        say(f"  WRONG ANSWER        0x{op:04X}  {spec[op][0]}: spec says "
            f"{spec[op][1]}, sim.rs emits {'+'.join(sorted(arms[op]))}")
    for op in delegating:
        say(f"  (0x{op:04X} {spec[op][0]} delegates — answer kind not checked here)")

    drift = len(disagree) + len(invented) + len(absent) + len(wrong)
    say("\nin sync" if not drift else f"\n{drift} mismatches")
    return 1 if drift else 0


if __name__ == "__main__":
    sys.exit(main())
