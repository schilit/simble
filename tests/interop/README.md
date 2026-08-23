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

## `classic_peer.py`

The **BR/EDR** direction, and the one that had no foreign witness at all:
simble's Classic *initiator* against a Bumble classic device. Bumble is
discoverable and connectable on netsim with a name, a Class of Device, an SPP
record and an RFCOMM echo server; `examples/classic_initiator.rs` inquires,
finds it, reads its name, pages it, queries SDP, opens the DLC on the channel
**the peer's record named**, writes, checks the echo and disconnects. The
client's exit status is the verdict, and Bumble adds one check of its own —
that the bytes really did reach a foreign RFCOMM server.

```bash
cargo build --example classic_initiator
python3 tests/interop/classic_peer.py                        # the base run
python3 tests/interop/classic_peer.py --inquiry-mode rssi    # event 0x22
python3 tests/interop/classic_peer.py --inquiry-mode eir     # event 0x2F
python3 tests/interop/classic_peer.py --records 40           # forces SDP continuation
```

Every fact asserted is one only Bumble knows: the name comes from its
`HCI_Write_Local_Name`, the Class of Device (0x2C0114, nothing like the
0x240404 headset simble's examples use) from its `HCI_Write_Class_Of_Device`,
and the RFCOMM channel — **7**, deliberately not the 3 every simble example
hardcodes — is allocated by Bumble's `rfcomm.Server.listen()` and read back
out of the record Bumble serialised. A client that guessed instead of reading
the answer passes everywhere else and fails here.

rootcanal *dies* on malformed HCI rather than returning an error, so a run
that reaches the end is also a statement about the Inquiry, Remote Name
Request and Create Connection parameter layouts in particular.

It has already earned its keep three times:

- **The SDP client ignored continuation state.** Bumble caps each response at
  the negotiated L2CAP MTU less nine and returns the rest under a
  continuation state; simble's event-loop client treated the first chunk as
  the whole answer. The prefix is a well-formed PDU whose payload is half a
  data element, so it did not fail loudly — it reported "the peer advertises
  no Serial Port service". Every simble record fits in one response, which is
  why nothing in-tree had ever seen it. `--records 40` reproduces it.
- **Two of the three inquiry-result event forms were unhandled.** rootcanal
  honours `HCI_Write_Inquiry_Mode`, and the host understood only the reset
  default (event 0x02). With mode 0x01 or 0x02 set the inquiry completed
  having found nothing, with no error anywhere. Handling 0x22 and 0x2F also
  bought the EIR name: in `--inquiry-mode eir` the run reads "Bumble SPP
  Peer" out of the inquiry result and never sends a Remote Name Request at
  all, which is what a phone does.
- **The RSSI form's Class of Device offset.** The first fix read event 0x22
  with the standard form's 15-octet stride and offset 9; it is 14 and 8 (one
  reserved octet, not two). The peer's real 0x2C0114 caught it — a
  self-consistent test never would.

`tests/classic_foreign_bytes_test.rs` pins the octets rootcanal and Bumble
actually sent, so `cargo test` re-checks all of the above with no netsim in
sight. Each of the three bugs was mutation-proven against it.

**Not covered:** pairing and encryption (Bumble accepts the link unpaired),
SCO/eSCO, and **ACL reassembly** — rootcanal never fragmented, so the 672-byte
SDP responses arrived whole and simble's lack of a reassembly buffer in
`ClassicHost::handle_acl` was never exercised. A controller with a smaller
ACL data length would truncate.

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
