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

## `gatt_client.py`

Bumble hosts a Heart Rate peripheral; simble's **scripted** GATT client
(`android::BluetoothGatt`, the catalog's `hrm_client` script) connects to it
over netsim, discovers, subscribes and asserts on the notifications. One
command, and the client's exit status is the verdict:

```bash
cargo build --example scripted_central
.venv/bin/python tests/interop/gatt_client.py
```

`tests/central_script_test.rs` runs the same client against a *simble*
peripheral, which proves the scripting surface and nothing about the wire.
This is the check that counts, and it has already earned its keep twice:

- LE Create Connection was sending peer address type "public" unconditionally.
  Bumble advertises with a random static address, so nothing ever connected —
  and the in-process controller never reads that field, so every simulated
  test passed. The client now scans for the target and takes the type off the
  air.
- Bumble puts a Characteristic User Description at `value_handle + 1` and the
  CCCD at `+ 2`. A client that assumes the common layout subscribes to the
  wrong handle; against a simble server that write even succeeds. The client
  issues Find Information over the descriptor range instead.

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

## `auracast_source.py` and `auracast_sink.py`

The two directions of **Auracast** — LE Audio *broadcast*, a BIG carrying LC3
on Broadcast Isochronous Streams with no connection anywhere in the picture.
Both scripts rebuild the simble example they need with `--features lc3` first,
so neither can pass on a binary that was broadcasting filler.

```bash
.venv/bin/python tests/interop/auracast_source.py   # bumble source -> simble sink
.venv/bin/python tests/interop/auracast_sink.py     # simble source -> bumble sink
```

`auracast_source.py` runs Bumble's own `auracast transmit` app and points
simble's `auracast_sink` example at it: simble scans, syncs to Bumble's
periodic advertising train, parses Bumble's BASE, joins the BIG and decodes.
The sink's exit status is the verdict — non-zero if no SDUs arrived.

`auracast_sink.py` is the mirror, and the direction that finds encoder bugs:
simble builds the extended advertisement, the BASE and the BIG, and Bumble's
`auracast receive` has to make sense of all of it. It checks three things —
that Bumble echoed back the codec configuration simble published, that its own
packet counter moved, and that the decoded PCM is 440 Hz on the left and 554 Hz
on the right. The last one is the point: a stream that merely arrives proves
the transport, but the *right tone in the right channel* proves the per-BIS
Audio Channel Allocation in simble's BASE was understood by a foreign stack.

Both directions pass today. What is not covered: **encrypted** broadcasts.
`broadcast_code` is plumbed through `LE Create BIG` and `LE BIG Create Sync`
and the receiver refuses an encrypted source it has no code for, but no
interop run has exercised it.
