# Joining simble's three controller worlds

Status: evaluation — nothing here is implemented.

## The question

simble talks to three kinds of controller, and a device on one cannot reach a
device on another:

- the in-page `Link` (`src/controller/sim.rs`, driven from `WebLink` in
  `src/transport/wasm_ws.rs`),
- netsim's embedded rootcanal (`src/transport/netsim.rs`), and
- real silicon on USB or serial (`src/transport/usb.rs`, `src/transport/serial.rs`).

Can the ethers be *joined* — a device in one world discovering and connecting to
a device in another, without either being re-staged onto a shared controller?

## The answer

| | in-page `Link` | netsim (rootcanal in netsimd) | USB/serial dongle |
|---|---|---|---|
| **in-page** | — | possible; two routes below | **not by software** |
| **netsim** | | — | **not by software** |
| **dongle** | | | trivially (one radio, one room) |

**simulated↔simulated is a software problem with a good answer; simulated↔physical
is a hardware problem with only workarounds.** A packet on the air is real, and no
software puts a simulated device on a real radio channel — simble can give one
simulated device a real radio over HCI (`usb.rs`/`serial.rs`), but that device
then leaves the simulation. The workarounds (dongle-as-PHY, a two-dongle GAP/GATT
proxy, HCI proxying) join one device at a time, never the ethers.

The two software routes for **in-page ↔ netsim**:

- *Relocate, don't bridge.* Every simble host stack is transport-agnostic
  (`LiveTransport`, `SIMBLE_HCI`) and the page already speaks netsim's WebSocket.
  A page scene whose devices each join netsimd as their own chip is on netsim's
  ether by construction — one ether, nothing to bridge. Cost: the in-page `Link`'s
  determinism, offline operation, and `tick()`-driven reproducibility are gone for
  that scene, and each device costs a WebSocket.
- *Bridge at the link layer.* Make the in-page `Link` a participant on a rootcanal
  phy socket (below). Works against **standalone rootcanal**; blocked against
  **netsimd**, which does not expose the phy port. The browser can't open a raw TCP
  socket, so the page needs a WebSocket→phy shim — structurally the same program as
  `simble --usb --ws PORT`.

## The facility that works: rootcanal's phy socket

Standalone rootcanal — already vendored (`third_party/rootcanal-rs`) and fetched
as a build (`scripts/fetch_rootcanal.sh`) — exposes four TCP ports:

| Port | Purpose |
|---|---|
| 6401 | test channel (add devices, add device to phy) |
| 6402 | HCI, bare H4 — one controller per connection |
| **6403** | **BR/EDR phy channel** |
| **6404** | **LE phy channel** |

The phy ports are what `--transport rootcanal` never touches. Each connection
becomes a **promiscuous, bidirectional ether tap** (`LinkLayerSocketDevice`):
every link-layer packet on the phy is written to the socket, and every packet
read from the socket is broadcast on the phy. The wire format is a 4-byte
little-endian length plus a `LinkLayerPacket` (a type byte, source and
destination addresses, then a body); LE discovery/connect/data needs seven of the
67 packet types. Each is fixed-width little-endian with a trailing byte array —
the shape zerocopy `#[repr(C)]` structs parse directly, and a starter crate exists
(`rootcanal-link-layer`). Upstream reserves the right to change the protocol
incompatibly.

**Proven, both directions.** A raw-TCP client on the LE phy port injected an
advertising PDU for an address owned by no controller, and a real rootcanal
controller told to scan reported it over HCI as an `LE Advertising Report`;
rootcanal's own beacons arrived on the read side. Two facts for any phy
participant: the LE Meta Event is masked off after `Reset`, so its host must send
`Set Event Mask` + `LE Set Event Mask` or see nothing; and rootcanal **drops the
socket** on malformed input rather than erroring — the same unforgiving oracle
netsim's controller is credited for.

**netsimd does not expose the phy ports.** They belong to `desktop/`'s standalone
binary; netsimd links rootcanal as a library and drives it over FFI, and all its
transports are HCI. So **standalone rootcanal can be joined at the link layer;
netsimd can only be joined at HCI.**

## Candidates that don't work

**Bumble** — the tool whose "L2 bridging" prompted this question — cannot join the
worlds. What it offers, layer by layer:

| Layer | Bumble facility | Joins two ethers? |
|---|---|---|
| HCI | `bridge.py`, `hci_bridge.py`, `android-netsim:mode=controller` | **No** — relocates a host (what `simble --usb --ws PORT` already does); an HCI bridge between two *controllers* is a category error ([#217](https://github.com/google/bumble/issues/217)), and `mode=controller` is single-tenant (a second device gets `Device busy`). |
| Link (in-process) | `link.py` `LocalLink` | Yes, but only within one Python process — the same architecture as simble's `Link` + `SimController`. |
| Link (cross-process) | ~~`RemoteLink` + `link-relay`~~ | **Removed in v0.0.213** (the facility that would have done the job). Was LE-only, no BR/EDR, encryption a TODO, and cannot reattach to today's typed `ll.py`/`lmp.py`. |
| L2CAP / RFCOMM / GATT | `l2cap_bridge.py`, `rfcomm_bridge.py`, `gg_bridge.py` | **No** — a Bluetooth↔TCP gateway. An L2CAP channel presupposes an ACL connection, so nothing at L2CAP can be what makes two devices see each other. Wrong layer. |
| PHY | none | Bumble implements no PHY; it interfaces with RootCanal for that ([#645](https://github.com/google/bumble/discussions/645)). |

Two caveats compound it: #217 says the plan is "to eventually only use the
Netsim/RootCanal controller," so building on Bumble's virtual link builds on a
component its author intends to retire; and the published docs still advertise the
removed `RemoteLink`/`link-relay` (the linked page is 404), so anyone researching
from docs rather than source will wrongly conclude it exists.

**A physical relay** is the only thing that reaches a real radio, and it is
hardware, not a packet-level ether join — see the answer above.

## Recommendation

Constraints (`AGENTS.md`): near-zero dependencies, pure Rust, no C/FFI, no async; a
Python process is acceptable as a *test* dependency only. Precedent: `rootcanal-rs`
is already a `cfg(rootcanal_oracle)`-gated dev-dependency.

1. **Relocate in-page scenes onto netsim/rootcanal** (small — `LiveScene`,
   `LiveTransport`, and the netsim WebSocket transport all exist; what's missing is
   a scene-level "put every device on this external controller" switch, plus docs).
   Closes in-page ↔ netsim. Not a bridge; must not be sold as one.
2. **Document the two-dongle / PHY-loan recipe** (small, mostly writing), so the
   motivating pain — a real phone on a dongle can't talk to a simulated device — is
   answered one device at a time with hardware already on the desk.
3. **Prove before planning a rootcanal phy client** (medium). It is the only
   genuine ether merge on offer and would give simble's *link layer* a foreign
   oracle for the first time — but it is medium work against a protocol upstream
   may break, and it reaches standalone rootcanal, not netsimd. The real risk is
   the translation in `sim.rs`: `Link` is a thin HCI matchmaker, while a phy
   participant must answer scan requests, honour connect indications, and sequence
   ACL.
4. **Do not** revive Bumble's removed link-relay, or build a device-level GAP/GATT
   proxy (large correctness surface, no foreign oracle). Bumble stays what
   `AGENTS.md` says it is: the foreign host stack simble's wire format is checked
   against — not a bridge.

**The go/no-go experiment for the phy client:** one `cfg`-gated integration test on
the model of `tests/rootcanal_oracle_test.rs`. Start standalone rootcanal; connect
a simble host to `--hci_port` into active scanning (sending `Set Event Mask` +
`LE Set Event Mask` first); open a `TcpStream` to `--link_ble_port` and write an
in-page `Link` advertiser as a length-prefixed advertising PDU; assert the host
receives an `LE Advertising Report`. That one packet type is the whole bridge in
miniature. Reversing it (read a scan off the phy, answer with a scan response) is
the point where, if it turns into a link-layer state machine, the honest signal is
to stop.

## Sources

Verified against source (Bumble `f534657`; `RemoteLink`/`link-relay` removal in
commit `1b44e73`, first absent in v0.0.213), and rootcanal's `test_environment.cc`,
`link_layer_socket_device.cc`, and `link_layer_packets.pdl`. The phy-socket
experiment was measured on rootcanal 1.12.0 (macOS arm64, no hardware) with
throwaway scripts not added to the tree. Upstream: google/bumble#217 and #645.
