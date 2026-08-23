# Android-supported Bluetooth peripherals in SimBLE

> **Checked 2026-08-23.** The two Auracast rows were stale and are corrected
> below. The rest of the table was re-verified against the tree. Read the
> Tier 2/3 estimates as of that date — `docs/gaps.md` is the live list.

What it would take for SimBLE to emulate each peripheral type Android's
AOSP Bluetooth stack supports natively — assessed by reading the code, not
by counting files.

## The distinction that matters

SimBLE has three different kinds of "support", and conflating them is
how a feature list becomes wrong:

**Scriptable** — a device can be declared in Rhai and added to a scene
through MCP or a web page. With netsim or a USB controller, a real central
can then interact with it. This needs the whole path: controller →
`VirtualDevice` → GATT → Rhai script → MCP `add_peripheral` / web page.

**Native device API** — Rust code can construct a runnable device model, but
that model is not itself exposed through Rhai, MCP, or the web pages.

**Library-only** — a protocol state machine that exists, compiles, and is
unit-tested, but is driven Rust-to-Rust by handing byte buffers between two
instances. Nothing on the air, nothing a phone can connect to.

The current user-facing surface is scriptable LE GATT. Classic runs natively
in a scene, but has no scriptable surface.

## What is actually in the tree

| Area | Availability | Notes |
|---|---|---|
| LE radio | **Scriptable** | `controller/sim.rs` `Link` routes advertising, LE connections, ACL, and ISO SDUs |
| LE host | **Scriptable** | `device/host.rs` `LeHost`, `device/virtual_device.rs` |
| LE GATT profiles | **Scriptable** | `profiles/*`; `android::*` scripts can create the same GATT result |
| LE device models | **Native device API** | `devices/*` models, including HOGP helpers |
| Classic radio | **Native device API** | `controller/sim.rs` `Link` models Scan Enable, inquiry, paging, Remote Name Request, and classic ACL routing — two BR/EDR devices connect in one process, no netsim |
| Classic host, SDP, and RFCOMM | **Native device API** | `ClassicHost` is both responder and initiator: discovery, inbound ACL, L2CAP (server and client), SDP (server and query), RFCOMM. `SceneEngine::add_classic_device` puts one in a scene; no Rhai/MCP/web surface yet |
| Other Classic profiles | **Library-only** | `classic/*` contains A2DP, AVRCP, HFP, HID, and more, but they are not yet registered with a scene or a transport |
| Classic link | **Native device API** | `controller/lmp.rs` `LmpLink` drives the connection handshake inside `sim.rs`; it is below HCI, so it emits no host-facing events |
| OBEX / Object Push | **Library-only** | Tested OBEX client/server plus an OPP server and SDP record; not yet wired to RFCOMM or a scene |
| LC3 codec | **Available in the web audio demo** | Optional `lc3` feature encodes and decodes mono LC3; it decodes frames from Google's liblc3, but is not a conformance claim |
| CIS establishment | **Available in the web audio demo** | The WebSocket controller establishes a CIS and carries ISO SDUs; verified with a Bumble source |
| BIS / Auracast transport | **Scriptable** | `packets/big.rs`, `device/big_broadcaster.rs`, `device/big_receiver.rs`, and a Broadcast domain page. Verified against Bumble's `auracast` app both directions: Bumble decoded our BASE as 440 Hz left / 554 Hz right (one tone per BIS), and we decoded 23 005 of its SDUs with 0 errors. netsim only — the in-page radio's BIG modelling is sequencing-only. |

Classic is not scriptable yet, but it is no longer stranded: the simulated
controller speaks BR/EDR, and two `ClassicHost`s in one scene inquire, page,
query SDP and exchange RFCOMM data with no netsim and no radio. What it still
needs is profile handlers and a scripting surface. See Tier 3.

## Feasibility tiers

### Tier 1 — Fits today

A GATT service plus a scripted device; no new subsystem. These can be
scripted today and used with an Android phone through netsim or USB.

| Peripheral | Profile | Notes |
|---|---|---|
| Heart rate monitor | HRS 0x180D | `hrm` example with changing measurements |
| Environmental sensor node | ESS 0x181A | `env_sensor` example |
| Thermometer | HTS 0x1809 | Minimal `thermometer` example: a readable/notifiable measurement, not full HTS measurement encoding |
| Battery / DIS device | 0x180F / 0x180A | `battery` example |
| Volume control (LE Audio control plane) | VCS 0x1844 | `volume` example with a writable control point |
| Fast Pair / iBeacon / Eddystone beacons | advertising only | `fast_pair` and `eddystone` examples |
| **Keyboard** | HOGP 0x1812 | Richer `hid_keyboard` example: report map, input reports, report references, and protocol mode |
| **Mouse / trackpad** | HOGP 0x1812 | `hid_mouse` — relative motion reports |
| **Game controller** | HOGP 0x1812 | `gamepad` — 2 axes + 8 buttons (LE pads; console pads are Classic HID) |
| **Cycling speed & cadence** | CSCS 0x1816 | `cycling` — cumulative wheel/crank counters |
| **Pulse oximeter** | PLXS 0x1822 | `pulse_oximeter` — SpO2 + pulse as SFLOATs |
| **Smart scale** | WSS 0x181D + BCS 0x181B | `weight_scale` — both services, indicated |
| **Smart lock** | vendor 128-bit control point | `smart_lock` — lock/unlock + state notify |
| **Fitness tracker / smartwatch** | multi-service GATT | `fitness_tracker` — HRS + BAS + DIS + vendor steps |
| **Eddystone beacon** | 0xFEAA service data | `eddystone` — UID frame, non-connectable |
| Find My Device Network tag | FMDN over Fast Pair 0xFE2C | Advertising is scriptable; the Google FMDN *protocol* (EIK, ephemeral IDs, provisioning) is a spec-compliance project, not a transport gap |

### Tier 2 — Remaining LE Audio and broadcast work

| Peripheral | Remaining work |
|---|---|---|
| LE Audio headphones / hearing aids | Test a real Android source. The web sink already negotiates ASCS, establishes a CIS, and decodes LC3 from Bumble/liblc3; Android-as-source is untested. |
| Auracast broadcast | **Done** (`23ad736`, `8c61ae0`). Remaining: `bass.rs` Add Source must drive a real `BigReceiver` instead of reporting success unconditionally, and encrypted broadcast is unproven — rootcanal does not encrypt BIS payloads, so it may be unprovable on this controller. |

netsim's rootcanal implements the ISO/CIS/BIS command set. SimBLE currently
uses the CIS portion for the web audio demo; broadcast support remains open.

### Tier 3 — Classic (BR/EDR): in a scene, not yet scriptable

`ClassicHost` covers the native path from controller H4 packets through ACL
and Classic L2CAP to SDP and registered protocol handlers, in both
directions: it answers a page, and it can now also inquire, page, open an
L2CAP channel as a client, query a peer's SDP and drive an RFCOMM session.

**Two devices now do this to each other inside a scene**, with no netsim and
no radio. That took two things, and an earlier version of this document named
only the second:

1. **BR/EDR in the simulated controller.** This was the actual blocker.
   `controller/sim.rs` modelled LE only — no scan enable, no inquiry, no
   paging, no BR/EDR Connection Complete. A scene slot alone would not have
   helped: `ClassicHost::handle_packet` consumes H4 and emits H4, so with no
   controller to send it a Connection Request it would have sat idle for
   ever. `sim.rs` now models Scan Enable (a device that enables neither scan
   is genuinely invisible), Inquiry with results from the scene, paging
   through to Connection Request/Connection Complete at both ends, Remote
   Name Request, and ACL routing between two connected classic devices.
2. **A scene slot.** `SceneEngine::add_classic_device` is the fifth thing a
   scene can host, beside `add_peripheral`, `add_scanner`, `add_central` and
   `add_scripted_central`.

`classic_scene_tests` in `transport/wasm_ws.rs` runs the whole sequence —
inquiry, remote name, page, L2CAP, SDP query, RFCOMM DLC, data, disconnect —
and asserts that a device which never enabled inquiry scan is *not* found.

Note on `controller/lmp.rs`: it models LMP, which is controller-to-controller
*below* HCI, so it emits no host-facing events and was never the missing
layer. It is now used inside `sim.rs` for the connection handshake, with a
new host-gated `ConnectionPending` state — because a controller may not
answer a page by itself; the host does, with Accept/Reject Connection Request.

What remains before Classic is scriptable beside LE:

1. **Profile adapters.** A2DP, AVRCP, HFP, and Classic HID have protocol
   implementations but are not `ClassicHost` handlers yet. SCO/eSCO is also
   absent, so HFP audio cannot run.
2. **Rhai/MCP bindings.** Nothing constructs a classic device from a script
   yet. The Android names to use are tabulated in
   `docs/scripting-profile-apis.md`; note that discovery maps to
   `BluetoothAdapter.startDiscovery()`, not to a profile proxy.
3. **OBEX integration.** The library implements OBEX and an Object Push
   server/SDP record, but it is not connected to RFCOMM. PBAP and MAP profile
   layers are still absent.
4. **BNEP.** It is absent, which leaves PAN unavailable.

| Peripheral | Current state | Remaining |
|---|---|---|
| Serial / OBD-II | Runs in a scene: `ClassicDevice` acceptor + initiator, SDP-advertised RFCOMM | Scripting adapter |
| Classic keyboards/mice/gamepads | HID protocol library | `ProtocolHandler` for Classic HID, then a scripting adapter |
| Headphones / speakers | A2DP / AVRCP protocol libraries | `ProtocolHandler`s and media integration |
| Hands-free / headset | HFP / HSP protocol library | `ProtocolHandler` and SCO/eSCO audio (the controller carries no SCO) |
| File push (Bluetooth share) | OBEX + OPP library and SDP record | Wiring OBEX onto the existing RFCOMM handler |
| Phonebook / messaging | OBEX core only | PBAP/MAP profile layers and transport integration |
| Tethering | — | BNEP and network plumbing |

Note that most Android game controllers (Xbox, DualSense) pair over
**Classic HID**, so the Tier 1 HOGP gamepad is the LE variant, not those.

## Recommended build order

1. **Tier 1 breadth** — the LE peripherals above. Cheap, immediately usable
   from a phone, and each one exercises the existing path. *(This pass.)*
2. **Android LE Audio source interop** — validate the existing CIS and LC3
   path with a phone, beyond the Bumble/liblc3 source test.
3. ~~**BIS / Auracast** — add broadcast transport on the existing ISO work.~~
   **Done 2026-08-23**, verified against Bumble both directions. What remains
   is `bass.rs` (see `docs/gaps.md`), not the transport.
4. ~~**BR/EDR scene/transport adapter** — make the existing `ClassicHost`
   available to scenes.~~ **Done**: BR/EDR in `controller/sim.rs` and
   `SceneEngine::add_classic_device`. What remains is MCP, web, and Rhai.
5. ~~**SPP first** on that foundation~~ — done as the proof of step 4; next
   are profile handlers for A2DP/AVRCP and HFP.
6. **OBEX over RFCOMM** — make Object Push usable, then add PBAP and MAP
   profile layers.

Step 4 was the leverage point, and it is done: the Classic host is now an
in-scene capability. Step 5 turns it into a scriptable one.
