# Controller bridging: a real radio and a simulated device

Status: evaluation — nothing here is implemented.

## The problem

simble runs devices on three kinds of controller: the in-page `Link`
(`src/controller/sim.rs`), netsim's embedded rootcanal (`src/transport/netsim.rs`),
and real silicon on USB or serial (`src/transport/{usb,serial}.rs`). Can a device
on one discover and connect to a device on another?

**Only one pairing is a real problem — a real-radio device talking to a simulated
one (phy↔sim).** The other two are not:

- **sim↔sim** (in-page ↔ netsim): *relocate.* Every simble host stack is
  transport-agnostic (`LiveTransport`, `SIMBLE_HCI`) and the page already speaks
  netsim's WebSocket, so a scene can put all its devices on one simulated
  controller and they share an ether by construction. Nothing to bridge.
- **phy↔phy** (dongle ↔ dongle): trivial — one radio, one room.

## phy↔sim is a hardware problem

A device is either on the air or in the simulation; it cannot be both. simble can
give a simulated device a real radio over HCI (`usb.rs`/`serial.rs`) — but then it
*is* a real device on the air with the other one, no longer simulated. Nothing in
software keeps a device inside the simulation while it exchanges packets with a
device on real RF.

A packet-level bridge would have to shuttle raw link-layer PDUs between the sim
ether and the air, and of its four legs only one is easy:

| leg | feasible? |
|---|---|
| **read** sim packets *out of* standalone rootcanal (its phy socket) | **easy** — proven |
| **inject** into rootcanal | a single advertising PDU works; sequencing a live *connection*'s raw packets in (state, timing, encryption) does not, and netsimd exposes no phy port at all |
| **sniff** raw PDUs *off* the air | a connection hops across 37 data channels and one radio hears one frequency at a time, so following it is fragile, and capturing all channels blind (a radio per channel, or a wideband SDR) is not practical |
| **inject** raw PDUs *onto* the air | needs a raw-transmitting radio — not the HCI API |

The air side is the wall. A commodity HCI dongle exposes neither raw RX nor raw
TX: the controller owns the link layer, and HCI hands you connections and GATT,
not per-channel PDUs to capture or craft. The radio a bridge would need is a
**raw-PHY sniffer + injector** — and no ordinary dongle is one. Advertising can be
sniffed (nRF Sniffer firmware, Ubertooth, Sniffle on a TI CC1352), but a
*connection* is the wall: it hops across 37 data channels and one radio hears one
frequency at a time, so a sniffer must lock onto the hop sequence from the
`CONNECT_IND` and stay synchronised — fragile and lossy — while listening to all
channels at once would take a radio per channel or a wideband SDR, which is not
practical. Injecting arbitrary link-layer packets needs custom radio firmware on
top of that. So a packet-level phy↔sim bridge is a hardware/research problem, not
a wiring one.

In its absence the only answers are per-device workarounds, never a join of the
two ethers:

- **Dongle-as-PHY** — give the simulated device its own real radio, so it joins
  the real one on the air. simble already does this (`--usb`/`--serial`); the
  "simulated" device is simply now real. This is the practical answer.
- **Two-dongle GAP/GATT proxy** — a device on each side relaying at the profile
  layer. Large correctness surface, no foreign oracle; kept on the shelf.

## Bumble does not bridge it

Bumble is the tool whose "L2 bridging" prompted the question, so it was evaluated.
It cannot join the worlds, and everything it offers is sim-side anyway:

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

## Recommendation

Constraints (`AGENTS.md`): near-zero dependencies, pure Rust, no C/FFI, no async; a
Python process is acceptable as a *test* dependency only.

1. **sim↔sim: relocate scenes onto netsim/rootcanal** (small — `LiveScene`,
   `LiveTransport`, and the netsim WebSocket transport all exist; what's missing is
   a scene-level "put every device on this external controller" switch, plus docs).
   Not a bridge; must not be sold as one.
2. **phy↔sim — the real problem: document the two-dongle / PHY-loan recipe** (small,
   mostly writing). The answer is a real radio for the simulated device, one device
   at a time, with hardware already on the desk. A packet-level bridge waits on a
   raw-PHY sniffer/injector radio that is not available off the shelf.
3. **Do not** build a device-level GAP/GATT proxy (large correctness surface, no
   foreign oracle) or revive Bumble's removed link-relay. Bumble stays what
   `AGENTS.md` says it is: the foreign host stack simble's wire format is checked
   against.

## Sources

Verified against source (Bumble `f534657`; `RemoteLink`/`link-relay` removal in
commit `1b44e73`, first absent in v0.0.213), and rootcanal's `test_environment.cc`,
`link_layer_socket_device.cc`, and `link_layer_packets.pdl`. The rootcanal
phy-socket read/inject was measured on rootcanal 1.12.0 (macOS arm64, no hardware)
with throwaway scripts not added to the tree. Upstream: google/bumble#217 and #645.
