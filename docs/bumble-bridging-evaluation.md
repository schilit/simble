# Bumble's bridging facilities, and joining simble's three controller worlds

Status: evaluation — nothing here is implemented.

**The question.** simble talks to three kinds of controller, and a device on
one cannot reach a device on another: the in-page `Link` (`src/controller/sim.rs`,
driven from `WebLink` in `src/transport/wasm_ws.rs`), netsim's embedded
rootcanal (`src/transport/netsim.rs`), and real silicon on USB or serial
(`src/transport/usb.rs`, `src/transport/serial.rs`). "Can Bumble's L2 bridging
join built-in, netsim, and usb controllers?"

## Verdict

**Bumble cannot bridge simble's three controller worlds.** "L2 bridging" in the
sense meant here does not exist in Bumble: `apps/l2cap_bridge.py` bridges an
L2CAP CoC channel to a *TCP socket* — a gateway out of Bluetooth, not a join
between two Bluetooth worlds. The facility that would have done the job,
`RemoteLink` + the `link-relay` WebSocket relay, was **removed in v0.0.213**
(commit `1b44e73`, 2025-07-21); the pinned Bumble in `.venv` (0.0.233) lacks it,
though the published docs still refer to it.

**rootcanal's phy socket is the facility that does exist.** Standalone rootcanal
exposes a promiscuous link-layer TCP port and it works — an experiment injected a
device owned by no controller into a rootcanal simulation and a real controller
reported it over HCI. netsimd does not expose that port.

### What Bumble offers, layer by layer

Read against `~/Documents/GitHub/bumble` (upstream `main`, `f534657`) and the
installed 0.0.233 in `.venv`.

| Layer | Bumble facility | Joins two ethers? |
|---|---|---|
| HCI | `bumble/bridge.py`, `apps/hci_bridge.py`, `android-netsim:mode=controller` | **No** — relocates a host (what `simble --usb --ws PORT` already does). An HCI bridge between two *controllers* is a category error, per [google/bumble#217](https://github.com/google/bumble/issues/217). `mode=controller` is single-tenant: a second device gets `Device busy`. |
| Link (in-process) | `bumble/link.py` `LocalLink`, `apps/controllers.py` | Yes, within one Python process. Same architecture as simble's `Link` + `SimController`; `tests/interop/bumble_link.py` (`--transport bumble`) already uses it. |
| Link (cross-process) | ~~`RemoteLink` + `apps/link_relay`~~ | **Removed in v0.0.213.** Was LE-only text protocol, no LMP/BR-EDR, encryption a `# TODO`; cannot reattach to today's typed `ll.py`/`lmp.py` dataclasses, which have no byte serialisation. |
| L2CAP / RFCOMM / GATT | `apps/l2cap_bridge.py`, `rfcomm_bridge.py`, `gg_bridge.py` | **No** — Bluetooth↔TCP gateway. An L2CAP channel presupposes an ACL connection, so nothing at L2CAP can be what makes two devices see each other. Wrong layer, independent of Bumble. |
| PHY | none | Bumble implements no PHY. Per [google/bumble#645](https://github.com/google/bumble/discussions/645) it "doesn't implement a PHY layer" and interfaces with RootCanal, which "does support exchanging PHY packets over TCP". |

#217 also notes the plan "to eventually only use the Netsim/RootCanal controller
going forward" — so building on Bumble's virtual link means building on a
component its author intends to retire.

**The stale-documentation trap.** <https://google.github.io/bumble/> still says
the link bus "may be remote (see Remote Link)"; the page it points at,
`.../apps_and_tools/link_relay.html`, is **404**. `apps/README.md` still carries
a `link-relay:ws://127.0.0.1:10723/test` example. Anyone researching from docs
rather than source will wrongly conclude the facility exists.

## What rootcanal offers instead

rootcanal — already vendored (`third_party/rootcanal-rs`) and fetched as a
standalone build (`scripts/fetch_rootcanal.sh`) — exposes **four** TCP ports:

| Port | Purpose |
|---|---|
| 6401 | test channel (control: add devices, add device to phy) |
| 6402 | HCI, bare H4 — one controller per connection |
| **6403** | **BR/EDR phy channel** |
| **6404** | **LE phy channel** |

The phy ports are what `--transport rootcanal` never touches. Each connection
becomes a `LinkLayerSocketDevice` (`desktop/test_environment.cc:SetUpLinkBleLayerServer`):
a **promiscuous, bidirectional ether tap** — every link-layer packet on the phy
is written to the socket, and every packet read from the socket is broadcast on
the phy (`model/devices/link_layer_socket_device.cc`).

Framing is a 4-byte little-endian length followed by the packet. The body is
`packets/link_layer_packets.pdl`:

```
packet LinkLayerPacket {
  type : PacketType,          // 1 octet
  source_address : Address,   // 6 octets
  destination_address : Address,
  _body_,
}
```

67 packet types are defined; LE discovery/connection/data needs seven:

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
shape `zerocopy` `#[repr(C)]` structs parse under this repo's conventions. A
pure-Rust, no-async, zero-dependency implementation is the normal case;
`~/Documents/GitHub/rootcanal-link-layer` is a zerocopy Rust crate for these
packets (advertising subset so far). Upstream warns, quoted verbatim:

> **Warning** The protocol can change in backward incompatible ways, be careful
> when depending on it.

**netsimd does not expose the phy ports.** They are opened by `desktop/`'s
`main`, the standalone binary. netsimd links rootcanal as a library and drives it
through FFI (`bluetooth_add_rust_device`); its flags
(`rust/daemon/src/args.rs`) have `--hci-port` and no phy port, and its transports
(`fd`, `grpc`, `h4`, `socket`, `uci`, `websocket`) are all HCI. So **standalone
rootcanal can be joined at the link layer; netsimd can only be joined at HCI.**

**Experiment result.** A throwaway raw-TCP client on the LE phy port injected a
`LeLegacyAdvertisingPdu` for `F0:0A:0B:0C:0D:0E` (owned by no controller) and a
real rootcanal controller told to scan reported it as an `LE Advertising Report`
over HCI. Phy-socket injection works in both directions (rootcanal's default
beacons also arrived on the read side). Two facts worth carrying: the LE Meta
Event is masked off after `Reset`, so a phy participant's host must send
`Set Event Mask` + `LE Set Event Mask` or see zero reports; and rootcanal **drops
the socket** on malformed input rather than returning an error — the same
unforgiving oracle `tests/interop/README.md` credits netsim's controller for.

## Which worlds can be joined, and how

"Join the ethers" means a device in one discovers and connects to a device in the
other without either being re-staged.

| | in-page `Link` | netsim (rootcanal in netsimd) | USB/serial dongle |
|---|---|---|---|
| **in-page** | — | possible; two ways below | **not by software** |
| **netsim** | | — | **not by software** |
| **dongle** | | | trivially (one radio, one room) |

**in-page ↔ netsim — two routes.**
- *Relocate, don't bridge.* Every simble host stack is transport-agnostic
  (`LiveTransport`, `SIMBLE_HCI`) and the page already speaks netsim's WebSocket
  (`src/transport/netsim.rs`). A page scene whose devices each join netsimd as
  their own chip is on netsim's ether by construction — one ether, nothing to
  bridge. Cost: the in-page `Link`'s determinism, offline operation and
  `tick()`-driven reproducibility are gone for that scene, and every device costs
  a WebSocket.
- *Bridge at the link layer.* Make the in-page `Link` a phy participant:
  serialise its devices' advertising/connect/ACL as `LinkLayerPacket`s onto a phy
  socket and inject what arrives back. Blocked against **netsimd** (no phy port);
  works against **standalone rootcanal** (proven above). The browser cannot open
  a raw TCP socket, so the page needs a WebSocket→phy shim — structurally the
  same program as `simble --usb --ws PORT`.

**Anything ↔ a physical radio — needs a physical relay.** A packet on the air is
real; no software puts a simulated device on a real radio channel. simble can
give one simulated device a real radio over HCI (`usb.rs`/`serial.rs`), but that
device then leaves the simulation. **simulated↔simulated is a software problem
with a good answer; simulated↔physical is a hardware problem with only
workarounds** (dongle-as-PHY, two-dongle GAP/GATT proxy, or HCI proxying — none a
packet-level ether join).

## Recommendation

Constraints from `AGENTS.md`: near-zero dependencies, pure Rust, no C/FFI, no
async. A Python process is acceptable as a *test* dependency only. Precedent:
`rootcanal-rs` is already a `cfg(rootcanal_oracle)`-gated dev-dependency for
`tests/rootcanal_oracle_test.rs`.

1. **Relocate in-page scenes onto netsim/rootcanal** (small: `LiveScene`,
   `LiveTransport` and the netsim WebSocket transport all exist; what's missing
   is a scene-level "put every device on this external controller" switch and the
   docs). Closes in-page ↔ netsim honestly. Not a bridge; must not be sold as one.
2. **Document the two-dongle / PHY-loan recipe** (small, mostly writing) so the
   motivating pain — a real phone on a dongle cannot talk to a simulated device —
   is answered one device at a time, with hardware already on the desk.
3. **Prove before planning a rootcanal phy client** (medium). It is the only
   genuine ether merge on offer, and would give simble's *link layer* a foreign
   oracle for the first time (everything in `tests/interop/` tests only the host
   stack). But it is medium work against a protocol upstream reserves the right
   to break, and it does not reach netsimd — it buys "in-page ↔ standalone
   rootcanal", not "in-page ↔ netsim". The real risk is the translation layer in
   `sim.rs`: `Link` is "a thin HCI matchmaker", and a phy participant must answer
   scan requests, honour connect indications and sequence ACL.
4. **Do not** revive Bumble's removed link-relay (upstream deleted it; LE-only,
   no encryption, no other speakers) or build a device-level GAP/GATT proxy (large
   correctness surface with no foreign oracle) — keep the latter on the shelf.

**Bumble is not the vehicle.** It should stay what `AGENTS.md` and
`tests/interop/README.md` say it is: the foreign host stack simble's wire format
is checked against.

### The smallest experiment that proves or kills the phy client

One `#[cfg]`-gated integration test on the model of `tests/rootcanal_oracle_test.rs`:

1. Start the standalone rootcanal `scripts/fetch_rootcanal.sh` provides.
2. Connect a simble host to `--hci_port` via `RootcanalTransport`, into active
   scanning. **Send `Set Event Mask` and `LE Set Event Mask` first.**
3. Open a plain `TcpStream` to `--link_ble_port`; serialise an in-page `Link`
   advertiser as a `LeLegacyAdvertisingPdu` and write it length-prefixed.
4. Assert the simble host receives an `LE Advertising Report` for that address.
   **This is the go/no-go** — the whole bridge in miniature, one packet type.
5. Then reverse: read `LE_SCAN` off the phy and answer with `LE_SCAN_RESPONSE`;
   assert it reaches the host. If step 5 turns into a link-layer state machine,
   that is the honest signal to stop.

## Sources

Verified against source except where marked.

- `~/Documents/GitHub/bumble` @ `f534657` — `bumble/{link,bridge,controller,ll,lmp}.py`,
  `bumble/transport/{__init__,android_netsim}.py`,
  `apps/{hci_bridge,l2cap_bridge,rfcomm_bridge,controllers}.py`, `apps/README.md`.
  Removal of `RemoteLink`/`link-relay`: commit `1b44e73` (2025-07-21), first
  absent in `v0.0.213`; removed content read via `git show 1b44e73^:…`.
- `~/Documents/GitHub/rootcanal-rs` — `src/{controller,ffi,rootcanal}.rs` and
  vendored `third_party/rootcanal`: `README.md`, `desktop/test_environment.cc`,
  `model/devices/link_layer_socket_device.cc`, `model/setup/test_model.cc`,
  `packets/link_layer_packets.pdl`.
- `~/Documents/GitHub/rootcanal-link-layer` — existing zerocopy Rust structs.
- This repo: `src/controller/sim.rs`,
  `src/transport/{mod,live,netsim,rootcanal,wasm_ws,usb,serial}.rs`,
  `src/bin/simble.rs`, `tests/interop/{README.md,bumble_link.py,rootcanal_link.py}`,
  `tests/rootcanal_oracle_test.rs`, `Cargo.toml`, `AGENTS.md`.
- Upstream: [google/bumble#217](https://github.com/google/bumble/issues/217),
  [google/bumble#645](https://github.com/google/bumble/discussions/645);
  <https://google.github.io/bumble/> (stale "Remote Link" claim; linked page 404);
  netsim `rust/daemon/src/{args,transport/mod,bluetooth/chip}.rs`; rootcanal
  v1.12.0 release assets.
- **Measured** — on rootcanal 1.12.0, macOS arm64, private ports 16401–16404, no
  hardware. Scripts were throwaways, not added to the tree.
