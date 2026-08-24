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

## `a2dp_peer.py`

The **A2DP** direction, and the first foreign witness simble's AVDTP acceptor
has ever had. `examples/a2dp_sink.rs` joins netsim as a discoverable,
connectable speaker publishing an Audio Sink SDP record; Bumble pages it and
runs the whole initiator sequence — Discover, Get_All_Capabilities,
Set_Configuration, Open, the **second** L2CAP channel on PSM 0x0019, Start —
then streams RTP/SBC into it. The sink's exit status is the verdict.

```bash
cargo build --example a2dp_sink
.venv/bin/python tests/interop/a2dp_peer.py                  # 40 SBC frames
.venv/bin/python tests/interop/a2dp_peer.py --frames 200     # a longer stream
```

Three things here are decided by someone other than simble:

- the **SBC operating point** comes from Bumble's `MediaCodecCapabilities`
  and arrives in Bumble's Set_Configuration;
- the **media transport channel** is a second L2CAP connection on a PSM that
  already has one. *Nothing on the wire says which channel is which.* Simble
  binds it because an OPEN just succeeded (AVDTP §5.4.6), and if that rule is
  wrong the media lands on an unattached CID and not one frame decodes. This
  is the check the whole script exists for;
- the **RTP framing and A2DP payload header** — sequence numbers, the frame
  count nibble, fragmentation when a frame exceeds the MTU — are Bumble's
  `MediaPacketPump`.

The audio is **libsbc's**, not simble's and not Bumble's: the frames are read
straight out of `LIBSBC_JOINT_STEREO_FRAMES` in `tests/sbc_interop_test.rs`,
where they are recorded as what bluez's libsbc produced for a known signal.
So a frame that decodes at the far end is three implementations agreeing —
libsbc wrote it, Bumble packetised it, simble decoded it. The vector is
parsed out of the Rust source rather than duplicated, so it cannot drift from
the test that says what it is.

**Not covered:** pairing and encryption (Bumble accepts the link unpaired);
the **source** direction, since only the sink is exercised here — simble's
`A2dpSource` has never met a foreign sink; AVRCP, so nothing sends a
transport key on *this* link (`avrcp_peer.py` covers AVRCP on its own); SDP, because Bumble finds the AVDTP PSM from the profile
rather than by searching simble's Audio Sink record (the record is published
and goes unread); and codec fallback — the sink advertises every SBC
capability, so Bumble never has to negotiate down and the reject path is
unreached over the air. `tests/a2dp_scene_test.rs` covers that one in
simulation.

## `avrcp_peer.py`

The **AVRCP** direction, and the only one here that runs **both roles**.
`examples/avrcp_remote.rs` is either end depending on `AVRCP_ROLE`, so one
script covers both.

```bash
cargo build --example avrcp_remote
.venv/bin/python tests/interop/avrcp_peer.py            # both phases
.venv/bin/python tests/interop/avrcp_peer.py --phase 1  # bumble CT -> simble TG
.venv/bin/python tests/interop/avrcp_peer.py --phase 2  # simble CT -> bumble TG
```

**Phase 1** — Bumble's controller pages simble's target and drives its media
player. Every assertion is made on *Bumble's* side, out of objects Bumble
parsed from simble's bytes: the event list from `get_supported_events()`, the
213 000 ms track length and `PlayStatus.PLAYING` from its
`SongAndPlayStatus`, the title and artist from `get_element_attributes()`, and
— the one that ties the two together — the `PlaybackStatusChanged` its
`monitor_playback_status()` yields as **PAUSED** after it sends a PASS THROUGH
PAUSE. The simble side asserts the mirror fact, that AV/C operation IDs it
never constructed arrived, and its exit status is the other half of the
verdict.

**Phase 2** — simble's controller pages a Bumble AVRCP *target* and sets its
volume. The fact asserted is foreign state: `delegate.volume` on the Bumble
side has to become 0x53. Nothing in the simble process can write that field.

`tests/avrcp_foreign_bytes_test.rs` pins the thirteen AVCTP SDUs Bumble sent
in a passing run, so `cargo test` re-checks the AV/C framing, the AVCTP
headers and both directions of the PDU layer with no netsim in sight.

**What this does *not* cover, and why:**

- **Fragmentation.** Bumble's `avrcp.Protocol.send_avrcp_response` and its
  `avctp.Protocol.send_message` both carry a literal `# TODO: fragmentation`,
  and its controller never sends `RequestContinuingResponse`. So a
  spec-correct fragmented response from simble would be dropped on Bumble's
  floor, and Bumble can never produce one to feed simble. The metadata here
  is deliberately kept inside one AV/C frame.
  `tests/avrcp_continuation_test.rs` covers the fragmented path in simulation
  — including the bug this work found: *neither* side modelled it, and the
  silent half was the **receive** path, where the controller reassembled
  fragments it had no way to ask for and a metadata read simply never
  answered.
- **Most of the target surface, in phase 2.** Bumble's AVRCP target
  implements exactly three commands — GetCapabilities, SetAbsoluteVolume and
  RegisterNotification — and answers everything else
  REJECTED(INVALID_PARAMETER). Simble's `GetPlayStatus` and
  `GetElementAttributes` therefore have no oracle in that direction. Phase 1
  covers them the other way round.
- **The browsing channel** (PSM 0x001B), which simble does not wire at all.
- **Pairing and encryption** — Bumble accepts the link unpaired.
- **SDP.** Both ends publish an AVRCP record and neither reads the other's:
  the AVCTP PSM is fixed by the profile, so nothing has to search for it.

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
