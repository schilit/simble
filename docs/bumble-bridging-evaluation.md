# Bumble's bridging facilities, and joining simble's three controller worlds

Status: evaluation — nothing here is implemented. The experiment in §3 is a
throwaway that lived only in a scratch directory.

**The question.** simble talks to three kinds of controller, and a device on
one cannot reach a device on another: the in-page `Link` (`src/controller/sim.rs`,
driven from `WebLink` in `src/transport/wasm_ws.rs`), netsim's embedded
rootcanal (`src/transport/netsim.rs`), and real silicon on USB or serial
(`src/transport/usb.rs`, `src/transport/serial.rs`). "Investigate bumble l2
bridging and whether we can use that to bridge built-in, netsim, and usb
controllers."

**The answers, up front.**

1. **"L2 bridging" in the sense meant here does not exist in Bumble.** Bumble's
   `apps/l2cap_bridge.py` bridges an L2CAP CoC channel to a *TCP socket* — it is
   a gateway out of Bluetooth, not a join between two Bluetooth worlds.
2. **The facility that would have done the job was deleted.** `RemoteLink` and
   the `link-relay` WebSocket relay — Bumble's cross-process virtual link — were
   removed in **v0.0.213** (commit `1b44e73`, 2025-07-21). The pinned Bumble in
   this repo's `.venv` (0.0.233) does not have them. Bumble's published docs
   still refer to them.
3. **What Bumble keeps is HCI relay**, which moves a *host* between controllers
   and never joins two ethers.
4. **rootcanal already has exactly the facility Bumble removed**, on a TCP port,
   in a documented packet format — and it works. §3 records an experiment that
   injected a device that existed in no controller anywhere into a rootcanal
   simulation and had a real controller report it over HCI.
5. **netsimd does not expose that port.** Standalone rootcanal does.

---

## 1. What Bumble actually offers, layer by layer

Read against the checkout at `~/Documents/GitHub/bumble` (upstream `main`,
`f534657`) and the installed 0.0.233 in `.venv`.

### 1.1 HCI relay — real, and the only "bridge" Bumble ships

`bumble/bridge.py` is 88 lines. `HCI_Bridge` wires two transports' sources to
each other's sinks through a `Forwarder` that parses each packet, optionally
runs a filter, traces it, and passes it on. `apps/hci_bridge.py` is the CLI:
`hci_bridge.py <host-transport> <controller-transport> [short-circuit opcodes]`,
where the short-circuit list makes the bridge answer chosen opcodes itself with
a synthetic Command Complete rather than forwarding them.

That is precisely what `simble --usb --ws PORT` already does
(`src/bin/simble.rs`), minus the filters.

The maintainer states the limit explicitly in
[google/bumble#217](https://github.com/google/bumble/issues/217):

> despite the generic "bridge" name, the HCI bridge app can only be used to
> bridge the transports of a Controller and a Host […] if you connect the HCI
> side of a Controller to the HCI side of another Controller, that's a mismatch.

**An HCI bridge relocates a host. It cannot merge two ethers.** Two controllers
joined at HCI is a category error, not a configuration problem.

### 1.2 Link layer — in-process only, since v0.0.213

`bumble/link.py` is now 149 lines and contains exactly one class, `LocalLink`.
Its whole interface to a controller is nine methods — five out
(`send_acl_data`, `send_advertising_pdu`, `send_ll_control_pdu`,
`send_lmp_packet`, `on_address_changed`) and four in (`on_link_acl_data`,
`on_ll_advertising_pdu`, `on_ll_control_pdu`, `on_lmp_packet`). Routing is
`asyncio.get_running_loop().call_soon(...)` onto another `Controller` object in
the same process. `apps/controllers.py` is the one tool that uses it: build one
`LocalLink`, attach N `Controller`s, expose each one's HCI over its own
transport.

This is the same architecture as simble's `Link` plus `SimController`, and
`tests/interop/bumble_link.py` already exploits it — that is what
`--transport bumble` is.

**What was removed.** Until v0.0.213 `link.py` also carried `RemoteLink`, a
`Link` implementation that spoke a line-oriented protocol over a WebSocket to
`apps/link_relay/link_relay.py`, a relay hosting named "rooms" of virtual
controllers. The transport moniker was `link-relay:ws://host/room`. The wire
protocol was text: `@<target> acl:<hex>`, `@* advertisement:<hex>`, `connect`,
`connected`, `disconnect:reason=N`, `encrypted:ltk=<hex>`, plus `/set-address`
as an RPC.

Even had it survived it would not have been enough. It modelled **LE only** —
no LMP, so no BR/EDR; no CIS, no BIG, no periodic advertising; encryption was
`on_encrypted_message_received` calling `on_link_encrypted(sender, bytes(8), 0,
bytes(16))` under a `# TODO parse params to get real args`. And current Bumble
has moved its link interface to typed `ll.AdvertisingPdu` / `ll.ControlPdu` /
`lmp.Packet` dataclasses (`bumble/ll.py`, `bumble/lmp.py`) which have **no
serialisation to bytes** — so the old text protocol could not simply be
reattached to today's `Controller`.

**The stale-documentation trap.** <https://google.github.io/bumble/> still says:

> The bus may be process-local, in which case all the controllers attached to
> the bus run in the same process, or it may be remote (see Remote Link), in
> which case several controllers in separate processes can communicate with
> each other.

`https://google.github.io/bumble/apps_and_tools/link_relay.html` — the page that
sentence points at — returns **404**. `apps/README.md` in the checkout still
carries a worked `link-relay:ws://127.0.0.1:10723/test` example. Anyone
researching this from documentation rather than source will conclude the
facility exists.

### 1.3 L2CAP — a gateway, not a bridge

`apps/l2cap_bridge.py` has two halves:

- `ServerBridge` — listens for an inbound **L2CAP CoC** channel on a PSM; when
  one opens, connects a TCP socket to a configured host/port and pumps SDUs
  both ways with `FlowControlAsyncPipe`.
- `ClientBridge` — the mirror: connect to a BLE device, listen on a TCP port,
  and open an L2CAP CoC channel per inbound TCP client.

`apps/rfcomm_bridge.py` is the same shape one layer up (RFCOMM DLC ↔ TCP), and
`apps/gg_bridge.py` the same for Golden Gate's GATT-based link.

All three cross the **Bluetooth/IP boundary**. None of them joins two Bluetooth
ethers, and none of them can: an L2CAP channel presupposes an ACL connection,
which presupposes both endpoints already being on one link. **A device that
cannot see another device's advertising cannot open an L2CAP channel to it, so
nothing at L2CAP can be the thing that makes them see each other.** That is the
core reason "L2CAP bridging" is the wrong layer for this problem, independent of
what Bumble happens to ship.

### 1.4 Bumble as a netsim server

`bumble/transport/android_netsim.py` has a `mode=controller` that makes Bumble
*serve* netsim's `PacketStreamer` gRPC service, so an Android emulator connects
to Bumble instead of to netsimd. It is HCI transport plumbing, and it is
single-tenant: a second device gets `PacketResponse(error='Device busy')` from
`lease_sink`. Not a link.

### 1.5 Summary

| Layer | Bumble facility | Joins two ethers? |
|---|---|---|
| HCI | `bumble/bridge.py`, `apps/hci_bridge.py`, `android-netsim:mode=controller` | **No** — moves a host |
| Link (in-process) | `bumble/link.py` `LocalLink`, `apps/controllers.py` | Yes, within one Python process |
| Link (cross-process) | ~~`RemoteLink` + `apps/link_relay`~~ | **Removed in v0.0.213** |
| L2CAP / RFCOMM / GATT | `apps/l2cap_bridge.py`, `rfcomm_bridge.py`, `gg_bridge.py` | **No** — Bluetooth↔TCP gateway |
| PHY | none | Bumble implements no PHY |

The maintainer's own summary of the last row, in
[google/bumble#645](https://github.com/google/bumble/discussions/645): Bumble
"doesn't implement a PHY layer" and interfaces with RootCanal, which does and
"does support exchanging PHY packets over TCP".

The same thread in #217 adds the strategic note:

> There's a plan to eventually only use the Netsim/RootCanal controller going
> forward, at some point in the near future.

Building on Bumble's virtual link would therefore be building on a component its
own author intends to retire.

---

## 2. What rootcanal offers instead

rootcanal — which this repo already vendors (`third_party/rootcanal-rs`) and
already fetches a standalone build of (`scripts/fetch_rootcanal.sh`) — exposes
**four** TCP ports, not one:

| Port | Purpose |
|---|---|
| 6401 | test channel (control: add devices, add device to phy) |
| 6402 | HCI, bare H4 — one controller per connection |
| **6403** | **BR/EDR phy channel** |
| **6404** | **LE phy channel** |

The phy ports are what `--transport rootcanal` never touches. Each accepted
connection becomes a `LinkLayerSocketDevice` registered on the phy
(`desktop/test_environment.cc:SetUpLinkBleLayerServer`). A
`LinkLayerSocketDevice` is a **promiscuous, bidirectional ether tap**: every
link-layer packet on the phy is written to the socket, and every packet read
from the socket is broadcast on the phy
(`model/devices/link_layer_socket_device.cc`).

The framing is a 4-byte little-endian length followed by the packet — that is
the whole transport-level protocol. The packet body is
`packets/link_layer_packets.pdl`:

```
packet LinkLayerPacket {
  type : PacketType,          // 1 octet
  source_address : Address,   // 6 octets
  destination_address : Address,
  _body_,
}
```

67 packet types are defined. For LE discovery, connection and data you need
seven of them:

```
LE_LEGACY_ADVERTISING_PDU 0x0B { adv_addr_type, target_addr_type, adv_type, data[] }
LE_SCAN                   0x0E { scanning_addr_type, adv_addr_type }
LE_SCAN_RESPONSE          0x0F { adv_addr_type, scan_response_data[] }
LE_CONNECT                0x0C { init_addr_type, adv_addr_type, interval, latency, timeout }
LE_CONNECT_COMPLETE       0x0D { …same… }
ACL                       0x01 { packet_boundary_flag, broadcast_flag, data[] }
DISCONNECT                0x05 { reason }
```

Every field is fixed-width little-endian with a trailing byte array — the exact
shape `zerocopy` `#[repr(C)]` structs parse under this repo's packet-layer
conventions. **A pure-Rust, no-async, zero-dependency implementation of this is
the normal case, not a stretch.** Bill already has one:
`~/Documents/GitHub/rootcanal-link-layer` is a zerocopy Rust crate for these
packets, currently covering the advertising subset.

Upstream carries one warning, and it should be quoted rather than paraphrased:

> **Warning** The protocol can change in backward incompatible ways, be careful
> when depending on it.

### netsimd does not expose the phy ports

The phy servers are opened by `desktop/test_environment.cc` — the **standalone
binary's** `main`, not the rootcanal library. netsimd does not use `desktop/`;
it links rootcanal as a library and drives it through the FFI, adding devices
with `bluetooth_add_rust_device` (netsim's
`rust/daemon/src/bluetooth/chip.rs`). netsimd's own flag list
(`rust/daemon/src/args.rs`) has `--hci-port` and nothing resembling a link or
phy port, and its transports are `fd`, `grpc`, `h4`, `socket`, `uci`,
`websocket` (`rust/daemon/src/transport/mod.rs`) — every one of them HCI.

So: **standalone rootcanal can be joined at the link layer; netsimd can only be
joined at HCI.** netsim's Rust side has an internal `receive_link_layer_packet`
callback for its own beacon devices, but it is in-process only.

---

## 3. The experiment

Both scripts below are throwaways; nothing was added to the tree, no hardware
was touched, and all ports used were 16401–16404.

**Setup.** `scripts/fetch_rootcanal.sh`'s upstream asset,
`rootcanal-1.12.0-macos-arm64.zip`, unpacked to a scratch directory, started as

```
rootcanal --hci_port=16402 --link_port=16403 --link_ble_port=16404 --test_port=16401
```

### 3.1 Reading the ether, and injecting a device into it

One TCP client on the HCI port (a real rootcanal controller, told to scan), one
raw TCP client on the LE phy port (us). Then: (1) read the phy socket; (2)
hand-build a `LeLegacyAdvertisingPdu` for `F0:0A:0B:0C:0D:0E` — an address no
controller in the simulation owns — write it to the phy socket, and see whether
the HCI host reports it.

```
connected: HCI :16402, LE phy :16404
  hci-evt 0e0401030c00                       Reset -> success
  hci-evt 0e0401010c00                       Set Event Mask -> success
  hci-evt 0e0401012000                       LE Set Event Mask -> success
  hci-evt 0e04010b2000 / 0e04010c2000        scan params, scan enable -> success
  hci-evt 3e2b0201030001005501acbe1f0f09674465766963652d626561636f6e…
  hci-evt 3e2b0201030002005501acbe1f0f09674465766963652d626561636f6e…
step 1: 8 link-layer packets observed on the phy socket
         type=0x0B src=01005501acbe dst=000000000000
         type=0x0B src=02005501acbe dst=000000000000
         …
step 2: 15 HCI events, 15 LE Advertising Reports
         REPORT for F0:0A:0B:0C:0D:0E: 3e16020100000e0d0c0b0af00a020106060947484f5354d6
RESULT: phy-socket injection WORKS
```

Reading that report: `3e 16` LE Meta, `02` Advertising Report, `01` one report,
`00` ADV_IND, `00` public, `0e0d0c0b0af0` the ghost address little-endian, `0a`
ten data octets, `020106` flags, `060947484f5354` `Complete Local Name` =
`GHOST`, `d6` RSSI. **A foreign process with no controller, no HCI and no
dependency put a device on rootcanal's ether and a real controller saw it.**

The two `be:ac:01:55:00:0{1,2}` sources in step 1 are rootcanal desktop's
default beacons (`test_environment.cc:237`); their arrival on the phy socket
proves the read direction independently of the write direction.

**One trap.** After `Reset`, the LE Meta Event is masked off, so no advertising
report reaches the host — a run without `Set Event Mask` and `LE Set Event
Mask` reports zero HCI events and looks like a flat failure.

### 3.2 What a phy participant would have to understand

Second script: two HCI controllers on the same rootcanal — one advertising
ADV_IND with a scan response, one **actively** scanning — and a census of what
the phy socket carried over four seconds.

```
546 link-layer packets in 4s on the LE phy socket:
  0x0B LE_LEGACY_ADVERTISING_PDU        194
  0x0E LE_SCAN                          176
  0x0F LE_SCAN_RESPONSE                 176
```

The full active-scan exchange is on the wire, not just advertising. Also: when
this script sent a 31-octet `LE Set Advertising Data` payload where 32 were
owed, rootcanal **dropped the socket** rather than returning an error status —
the behaviour `tests/interop/README.md` documents as the reason netsim's
controller finds bugs Bumble's cannot. A phy-level bridge gets the same
unforgiving oracle.

---

## 4. Which worlds can be joined, and how

The three worlds, and the honest matrix. "Join the ethers" means a device in one
discovers and connects to a device in the other without either being re-staged.

| | in-page `Link` | netsim (rootcanal in netsimd) | USB/serial dongle |
|---|---|---|---|
| **in-page** | — | possible; two ways, §4.1 | **not by software**, §4.3 |
| **netsim** | | — | **not by software**, §4.3 |
| **dongle** | | | trivially (one radio, one room) |

### 4.1 in-page ↔ netsim — possible, two routes

**Route A: relocate, don't bridge.** Every simble host stack is already
transport-agnostic (`LiveTransport`, `SIMBLE_HCI`), and the page already speaks
netsim's WebSocket (`src/transport/netsim.rs`). A page scene whose devices each
join netsimd as their own chip is on netsim's ether by construction. There is
nothing to bridge because there is only one ether. Cost: the in-page `Link`'s
determinism, offline operation, and `tick()`-driven reproducibility are gone for
that scene, and every device costs a WebSocket.

**Route B: bridge at the link layer.** Make the in-page `Link` a participant in
a rootcanal phy: serialise its devices' advertising/connect/ACL as
`LinkLayerPacket`s onto a phy socket, and inject what arrives back as simulated
transmissions. Both ethers stay themselves and merge. Blocked against
**netsimd**, which exposes no phy port (§2); works against a **standalone
rootcanal**, proven in §3. The browser cannot open a raw TCP socket, so the page
needs a WebSocket→phy shim — which is structurally the same program as
`simble --usb --ws PORT`.

### 4.2 netsim ↔ standalone rootcanal

Not one of the three worlds, but worth stating: two rootcanal *instances* can be
joined at the phy (`TestEnvironment::ConnectToRemoteServer` exists for exactly
this), and netsimd is not one of the instances that can play. If netsim's ether
must be joined to anything, the join point is HCI, one host at a time.

### 4.3 Anything ↔ a physical radio — needs a physical relay

**A packet on the air is real.** No amount of software puts a simulated device
on a real radio channel. Concretely:

- A phone or a speaker on the air can be reached only by something that
  transmits. simble already has that: a dongle driven over HCI
  (`src/transport/usb.rs`, `src/transport/serial.rs`). But that is not bridging
  — it is **giving one simulated device a real radio**, at which point it is not
  on the simulated link at all.
- To have a *simulated* device (still on the in-page `Link` or on netsim) meet a
  *physical* peer, something must relay. The only mechanisms that exist:
  1. **A dongle as the simulated device's PHY.** Move that device's host stack
     onto `usb:`/`serial:`. Cheapest, already works, but the device leaves the
     simulation.
  2. **Two dongles back to back / a proxy device.** Dongle A impersonates the
     simulated device on the air; software copies GAP and GATT state between it
     and the simulated device. This is a **device-level** relay, not a packet
     relay: connection events, encryption and ATT timeouts do not survive an
     arbitrary software hop, so the relay has to re-originate the protocol
     rather than forward it.
  3. **HCI proxying**, which is what `simble --usb --ws` already is, and which
     relocates a host rather than joining ethers.

There is no fourth option, and any design that assumes one is wrong:
**simulated↔simulated is a software problem with a good answer;
simulated↔physical is a hardware problem with only workarounds.**

---

## 5. Options for simble, ranked

Constraints from `AGENTS.md`: near-zero dependencies, all pure Rust, no C/FFI,
**no async anywhere**. A Python process is acceptable as a *test* dependency
(Bumble already is one) and not as a shipped runtime component. Precedent worth
noting: `rootcanal-rs` is already a `cfg(rootcanal_oracle)`-gated dev-dependency
for `tests/rootcanal_oracle_test.rs`, so "an FFI-backed foreign oracle, off by
default, invisible to `cargo package`" is an established pattern here.

### Option 1 — Relocate rather than bridge (in-page → netsim/rootcanal)

**Effort:** small; mostly wiring and documentation. `LiveScene`, `LiveTransport`
and the netsim WebSocket transport all exist. What is missing is a scene-level
"put every device on this external controller" switch and the docs saying when
to use it.

**Buys:** closes in-page ↔ netsim completely and honestly. Any page scene can be
re-hosted on the ether that also carries the Android emulator.

**Costs / risks:** the in-page `Link` is the thing that makes simble
demonstrable with no daemon, and this does not extend it — it opts out of it. It
does nothing for the dongle. It is not a bridge and should not be sold as one.

### Option 2 — A rootcanal phy client: `LinkLayerPacket` in pure Rust

**Effort:** medium. Roughly: (a) zerocopy structs for the seven LE packet types
in §2, following `src/packets/` conventions — `~/Documents/GitHub/rootcanal-link-layer`
is a head start; (b) a `PhyTransport` alongside `RootcanalTransport` speaking the
4-byte-length framing on TCP; (c) a translation layer in `src/controller/sim.rs`
between `Link`'s HCI-boundary routing and link-layer PDUs. (c) is the real work
and the real risk: simble's `Link` is deliberately "a thin HCI matchmaker, not a
faithful controller", and a phy participant has to answer scan requests, honour
connect indications and sequence ACL — closer to a link layer than `Link`
currently is.

**Buys:** the only genuine ether merge available. Also — and this may matter more
— it gives simble's *link layer* a foreign oracle for the first time. Everything
in `tests/interop/` today tests the host stack; nothing has ever checked what
`sim.rs` does against a foreign link. rootcanal drops the socket on malformed
input, so the oracle is unforgiving in the way the repo already values.

**Costs / risks:** upstream explicitly warns the PDL protocol may change
incompatibly. It does **not** reach netsimd (§2), so it buys "in-page ↔
standalone rootcanal", not "in-page ↔ netsim". The BR/EDR phy is a second, larger
packet set. Nothing about the dongle changes.

### Option 3 — A documented two-dongle / PHY-loan recipe

**Effort:** small, and mostly writing. `docs/usb-controllers.md` plus a worked
example: move a scripted device's host stack onto `serial:` or `usb:` so a real
phone can see it.

**Buys:** the actual motivating pain — "a real phone on a dongle cannot talk to a
simulated device" — is answered, for one device at a time, with hardware that is
already on the desk.

**Costs / risks:** does not scale past the number of dongles; the device leaves
the simulation; and it must be described as what it is (§4.3) or it will be
mistaken for a bridge.

### Option 4 — A device-level (GAP/GATT) proxy across worlds

**Effort:** large. A "shadow device" in world B mirroring a device in world A:
copy advertising data, mirror the GATT database, forward ATT operations.

**Buys:** it is the only option that works across a *physical* boundary as well
as a simulated one, and the only one that could put a real phone in front of a
netsim device.

**Costs / risks:** it re-implements stack semantics rather than moving packets,
so every layer it does not model is a silent divergence — encryption, MTU
exchange, notifications, ATT timeouts, connection parameters. It is a large
correctness surface with no foreign oracle to check it against. Worth keeping on
the shelf; not worth starting.

### Option 5 — Reimplement Bumble's removed link-relay

**Rejected.** Upstream deleted it. Its protocol was LE-only text with a `# TODO`
where encryption should be, it cannot be reattached to today's `Controller`
(§1.2), and reviving it means owning a wire protocol with no other speakers.
rootcanal's is the one with users, a spec file, and an implementation that will
tell you when you are wrong.

---

## 6. Recommendation

**Do Option 1 and Option 3 now; prove Option 2 before committing to it; do not
do 4 or 5.**

- Options 1 and 3 together cover both halves of the stated pain with work that
  is mostly documentation and wiring, and neither adds a dependency or a
  protocol. They also make the boundary in §4.3 explicit in the docs, which is
  the thing that stops the question being re-asked.
- Option 2 is the only real bridge on offer and is genuinely attractive for a
  reason orthogonal to bridging — a foreign oracle for `sim.rs`. But it is
  medium-sized work against a protocol upstream reserves the right to break, and
  it does not reach netsimd. It should be *proved before it is planned*.

**Bumble is not the vehicle for any of this.** It should stay exactly what
`AGENTS.md` and `tests/interop/README.md` already say it is: the foreign host
stack simble's wire format is checked against. Its virtual controller and link
are, in its own maintainer's words, "a limited component that's useful for unit
testing and some simple configurations", slated to be replaced by
netsim/rootcanal.

### The smallest experiment that proves or kills Option 2

One `#[cfg]`-gated integration test, on the model of
`tests/rootcanal_oracle_test.rs`, in the shape of §3 but with simble on the
injecting end:

1. Start the standalone rootcanal `scripts/fetch_rootcanal.sh` already provides
   (`--hci_port`, `--link_ble_port`, private ports).
2. Connect a simble host to `--hci_port` through the existing
   `RootcanalTransport` and put it into active scanning. **Send `Set Event Mask`
   and `LE Set Event Mask` first** — see §3.1.
3. Open a plain `TcpStream` to `--link_ble_port`. Take one advertiser out of an
   in-page `Link`, serialise its advertising as a `LeLegacyAdvertisingPdu`, and
   write it length-prefixed.
4. Assert the simble host receives an `LE Advertising Report` for that address
   carrying that AD.
5. Then the reverse: read `LE_SCAN` off the phy socket and answer it with an
   `LE_SCAN_RESPONSE`, and assert the scan response reaches the simble host.

Step 4 is the go/no-go: it is the whole bridge in miniature, needs one packet
type, no async, and no dependency. Step 5 costs one more packet type and proves
the loop closes. If both pass in a day, Option 2 is real; if step 5 turns into a
link-layer state machine, that is the honest signal to stop.

---

## 7. Sources

Verified against source except where marked.

**Local checkouts**

- `~/Documents/GitHub/bumble` @ `f534657` — `bumble/link.py`, `bumble/bridge.py`,
  `bumble/controller.py`, `bumble/ll.py`, `bumble/lmp.py`,
  `bumble/transport/__init__.py`, `bumble/transport/android_netsim.py`,
  `apps/hci_bridge.py`, `apps/l2cap_bridge.py`, `apps/rfcomm_bridge.py`,
  `apps/controllers.py`, `apps/README.md`.
- Removal of `RemoteLink` / `link-relay`: commit `1b44e73` ("Remove link-relay
  and RemoteLink", Josh Wu, 2025-07-21), first absent in tag `v0.0.213`. Removed
  content read via `git show 1b44e73^:bumble/link.py` and
  `git show 1b44e73^:apps/link_relay/link_relay.py`.
- `~/Documents/GitHub/rootcanal-rs` — `src/controller.rs`, `src/ffi.rs`,
  `src/rootcanal.rs`, and the vendored upstream at `third_party/rootcanal`:
  `README.md`, `desktop/test_environment.cc`,
  `model/devices/link_layer_socket_device.cc`,
  `model/setup/test_model.cc`, `packets/link_layer_packets.pdl`.
- `~/Documents/GitHub/rootcanal-link-layer` — existing zerocopy Rust structs for
  the rootcanal link-layer packets.
- This repo: `src/controller/sim.rs`, `src/transport/{mod,live,netsim,rootcanal,wasm_ws,usb,serial}.rs`,
  `src/bin/simble.rs`, `tests/interop/{README.md,bumble_link.py,rootcanal_link.py}`,
  `tests/rootcanal_oracle_test.rs`, `Cargo.toml`, `AGENTS.md`.

**Upstream, fetched**

- Bumble maintainer on what an HCI bridge can and cannot do, and on the plan to
  move to netsim/rootcanal — <https://github.com/google/bumble/issues/217>
- Bumble maintainer on Bumble implementing no PHY, and rootcanal exchanging PHY
  packets over TCP — <https://github.com/google/bumble/discussions/645>
- Stale claim that the link bus "may be remote (see Remote Link)" —
  <https://google.github.io/bumble/> ; the page it points at,
  <https://google.github.io/bumble/apps_and_tools/link_relay.html>, is 404.
- netsim daemon flags and transports —
  <https://android.googlesource.com/platform/tools/netsim/+/refs/heads/master/rust/daemon/src/args.rs>,
  <https://android.googlesource.com/platform/tools/netsim/+/refs/heads/master/rust/daemon/src/transport/mod.rs>,
  <https://android.googlesource.com/platform/tools/netsim/+/refs/heads/master/rust/daemon/src/bluetooth/chip.rs>
- rootcanal v1.12.0 release asset `rootcanal-1.12.0-macos-arm64.zip` —
  <https://github.com/google/rootcanal/releases>

**Measured** — §3, on rootcanal 1.12.0, macOS arm64, ports 16401–16404, no
hardware involved. Scripts are throwaways and were not added to the tree.
