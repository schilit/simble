#!/usr/bin/env python3
# Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
#
# Does src/gatt/sig_names.rs still match the Bluetooth SIG's registry?
#
# Every test in this repo has simble on both ends, so two copies of a wrong
# constant always agree with each other. That is not hypothetical here: the
# Ranging Service shipped four *invented* characteristic UUIDs
# (0x2B6E/0x2B70/0x2B71/0x2B72, assigned to nothing) and no in-tree test could
# have caught it. Neither Bumble nor Zephyr implements RAS, so the usual
# foreign oracles were silent too. The SIG's own registry was the only
# reference capable of disagreeing with us.
#
# So: fetch the registry and diff. It is three files over plain HTTPS, no
# authentication. The tables matched exactly when this was written, so the
# check starts green and any output is real drift -- either the SIG assigned
# something new, or someone edited the generated table by hand.
#
#     python3 scripts/check_sig_assigned_numbers.py          # report drift
#     python3 scripts/check_sig_assigned_numbers.py --quiet  # CI: exit code only
#
# Exit 0 = in sync, 1 = drift, 2 = could not reach the registry (which is NOT
# a failure of the code under test, and CI should treat it as a skip).

import argparse
import re
import sys
import urllib.error
import urllib.request

BASE = "https://bitbucket.org/bluetooth-SIG/public/raw/HEAD/assigned_numbers/uuids"
SOURCES = {
    "SERVICE_NAMES": "service_uuids.yaml",
    "CHARACTERISTIC_NAMES": "characteristic_uuids.yaml",
    "DESCRIPTOR_NAMES": "descriptors.yaml",
}
TABLE = "src/gatt/sig_names.rs"


def fetch(name):
    """The SIG's YAML is a flat list of `uuid:`/`name:` pairs; parsed with a
    regex rather than a YAML dependency, since that is all the shape there is."""
    with urllib.request.urlopen(f"{BASE}/{name}", timeout=30) as response:
        text = response.read().decode("utf-8")
    pairs = re.findall(r"-\s+uuid:\s*(0x[0-9A-Fa-f]{4})\s*\n\s*name:\s*(.+)", text)
    return {int(u, 16): n.strip().strip('"') for u, n in pairs}


def parse_table(source, const):
    """Reads one `pub static NAME: &[(u16, &str)] = &[...]` block."""
    start = source.index(f"pub static {const}:")
    end = source.index("];", start)
    body = source[start:end]
    return {
        int(u, 16): n
        for u, n in re.findall(r'\((0x[0-9A-Fa-f]{4}),\s*"((?:[^"\\]|\\.)*)"\)', body)
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--quiet", action="store_true", help="exit code only")
    args = ap.parse_args()

    source = open(TABLE, encoding="utf-8").read()
    drift = 0
    for const, filename in SOURCES.items():
        try:
            sig = fetch(filename)
        except (urllib.error.URLError, TimeoutError) as e:
            print(f"could not reach the SIG registry ({filename}): {e}", file=sys.stderr)
            return 2
        ours = parse_table(source, const)

        # Names the SIG does not have at all are the serious case -- that is
        # how the RAS UUIDs got invented.
        unknown = {u: n for u, n in ours.items() if u not in sig}
        renamed = {u: (n, sig[u]) for u, n in ours.items() if u in sig and n != sig[u]}
        missing = {u: n for u, n in sig.items() if u not in ours}

        drift += len(unknown) + len(renamed)
        if not args.quiet:
            status = "drift" if (unknown or renamed) else "in sync"
            print(f"{const}: {len(ours)} local / {len(sig)} SIG — {status}")
            for u, n in sorted(unknown.items()):
                print(f"  NOT ASSIGNED BY SIG  0x{u:04X}  {n!r}")
            for u, (was, now) in sorted(renamed.items()):
                print(f"  renamed              0x{u:04X}  {was!r} -> {now!r}")
            if missing:
                print(f"  ({len(missing)} assigned numbers we do not carry — not an error)")

    if not args.quiet:
        print("\nin sync" if not drift else f"\n{drift} entries drifted")
    return 1 if drift else 0


if __name__ == "__main__":
    sys.exit(main())
