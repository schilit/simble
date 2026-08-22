# Interop scripts

Bumble-driven scripts that test simble against a *foreign* stack over netsim.
Unit tests cannot find the bugs these find: two simble endpoints always agree
with each other, so only a real peer proves the wire format is right.

They are not run by `cargo test` — they need a live `netsimd` and a Python
environment with [Bumble](https://github.com/google/bumble) installed.

```bash
python3 -m venv .venv && .venv/bin/pip install bumble lc3py   # Python >= 3.10
~/Library/Android/sdk/emulator/netsim devices                 # confirm netsimd
```

## `lea_source.py`

A complete LE Audio source: connects to a simble sink, discovers ASCS,
walks the ASE through Config Codec → Config QoS → Enable, establishes a
**real CIS**, and streams LC3 frames encoded by **Google's liblc3** — the
same implementation Android ships — so the sink is decoding foreign audio,
not its own encoder's output.

```bash
# with web/audio/ open in a browser on the netsim (WebSocket) controller,
# and its "Enable sound" button clicked:
.venv/bin/python tests/interop/lea_source.py CC:1E:57:00:00:08/P
```

The address is the page's built-in sink. Expect its SDU counter to climb at
~100/s (10 ms SDUs) and the audio to play at the device's live volume — this
is the interesting direction, a *foreign* source feeding simble's sink.
