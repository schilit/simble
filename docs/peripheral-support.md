# Android-supported Bluetooth peripherals in SimBLE

Authoritative source for what is in the tree: the code. `docs/gaps.md` is the
live list of what is missing. What it would take for SimBLE to emulate each
peripheral type Android's AOSP Bluetooth stack supports natively.

## The distinction that matters

SimBLE has three kinds of "support":

**Scriptable** — a device can be declared in Rhai and added to a scene through
MCP or a web page. With netsim or a USB controller, a real central can interact
with it. Needs the whole path: controller → `VirtualDevice` → GATT → Rhai script
→ MCP `add_peripheral` / web page.

**Native device API** — Rust code can construct a runnable device model, but that
model is not exposed through Rhai, MCP, or the web pages.

**Library-only** — a protocol state machine that exists, compiles, and is
unit-tested, driven Rust-to-Rust by handing byte buffers between two instances.
Nothing on the air.

The current *scriptable* (Rhai) surface is LE GATT. Classic runs natively in a
scene — A2DP, AVRCP and Classic HID as registered handlers, with fixed web-page
demos — but has no Rhai/MCP scripting surface yet.

## What is actually in the tree

| Area | Availability | Notes |
|---|---|---|
| LE radio | **Scriptable** | `controller/sim.rs` `Link` routes advertising, LE connections, ACL, and ISO SDUs |
| LE host | **Scriptable** | `device/host.rs` `LeHost`, `device/virtual_device.rs` |
| LE GATT profiles | **Scriptable** | `profiles/*`; `android::*` scripts can create the same GATT result |
| LE device models | **Native device API** | `devices/*` models, including HOGP helpers |
| Classic radio | **Native device API** | `controller/sim.rs` `Link` models Scan Enable, inquiry, paging, Remote Name Request, and classic ACL routing — two BR/EDR devices connect in one process, no netsim |
| Classic host, SDP, and RFCOMM | **Native device API** | `ClassicHost` is both responder and initiator: discovery, inbound ACL, L2CAP (server and client), SDP (server and query), RFCOMM. `SceneEngine::add_classic_device` puts one in a scene; no Rhai/MCP/web surface yet |
| A2DP / AVRCP / Classic HID | **Native device API** | Registered `ProtocolHandler`s with Rust scene builders (`media_scene`, `speaker_scene`, `keyboard_scene`) and scene tests (`a2dp_scene_test`, `avrcp_scene_test`); web demos on the Audio and Car pages. No Rhai/MCP binding yet |
| HFP | **Native device API** | AT signalling over RFCOMM and a SCO/eSCO link (Setup/Accept/Reject Synchronous Connection, codec negotiation) run in a scene (the Car page); the link carries payload but has no codec — CVSD/mSBC is a seam, so no real speech audio |
| Classic link | **Native device API** | `controller/lmp.rs` `LmpLink` drives the connection handshake inside `sim.rs`; below HCI, so it emits no host-facing events |
| OBEX / Object Push | **Library-only** | Tested OBEX client/server plus an OPP server and SDP record; not yet wired to RFCOMM or a scene |
| LC3 codec | **Available in the web audio demo** | Optional `lc3` feature encodes and decodes mono LC3; decodes frames from Google's liblc3, but is not a conformance claim |
| CIS establishment | **Available in the web audio demo** | The WebSocket controller establishes a CIS and carries ISO SDUs; verified with a Bumble source |
| BIS / Auracast transport | **Scriptable** | `packets/big.rs`, `device/big_broadcaster.rs`, `device/big_receiver.rs`, and a Broadcast domain page. Verified against Bumble's `auracast` app both directions: Bumble decoded our BASE as 440 Hz left / 554 Hz right, and we decoded 23 005 of its SDUs with 0 errors. netsim only — the in-page radio's BIG modelling is sequencing-only. |

Classic is not Rhai-scriptable yet, but is well past stranded: the simulated
controller speaks BR/EDR, two `ClassicHost`s in one scene inquire, page, query SDP
and exchange RFCOMM data with no netsim and no radio, and A2DP/AVRCP/HID run as
registered handlers with scene builders and web demos. What it still needs is a
Rhai/MCP scripting surface and an HFP SCO codec (the SCO/eSCO link works; CVSD/mSBC
is a seam). See Tier 3.

## Feasibility tiers

### Tier 1 — Fits today

A GATT service plus a scripted device; no new subsystem. Scriptable today and
usable with an Android phone through netsim or USB.

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
|---|---|
| LE Audio headphones / hearing aids | Test a real Android source. The web sink already negotiates ASCS, establishes a CIS, and decodes LC3 from Bumble/liblc3; Android-as-source is untested. |
| Auracast broadcast | **Done** (`23ad736`, `8c61ae0`). Remaining: `bass.rs` Add Source must drive a real `BigReceiver` instead of reporting success unconditionally, and encrypted broadcast is unproven — rootcanal does not encrypt BIS payloads, so it may be unprovable on this controller. |

netsim's rootcanal implements the ISO/CIS/BIS command set. SimBLE currently uses
the CIS portion for the web audio demo; broadcast support remains open.

### Tier 3 — Classic (BR/EDR): in a scene, not yet scriptable

`ClassicHost` covers the native path from controller H4 packets through ACL and
Classic L2CAP to SDP and registered protocol handlers, in both directions: it
answers a page, and can inquire, page, open an L2CAP channel as a client, query a
peer's SDP and drive an RFCOMM session.

**Two devices now do this to each other inside a scene**, with no netsim and no
radio. That took two things:

1. **BR/EDR in the simulated controller.** The actual blocker. `controller/sim.rs`
   modelled LE only. It now models Scan Enable (a device that enables neither scan
   is genuinely invisible), Inquiry with results from the scene, paging through to
   Connection Request/Connection Complete at both ends, Remote Name Request, and
   ACL routing between two connected classic devices.
2. **A scene slot.** `SceneEngine::add_classic_device` is the fifth thing a scene
   can host, beside `add_peripheral`, `add_scanner`, `add_central` and
   `add_scripted_central`.

`classic_scene_tests` in `transport/wasm_ws.rs` runs the whole sequence —
inquiry, remote name, page, L2CAP, SDP query, RFCOMM DLC, data, disconnect — and
asserts that a device which never enabled inquiry scan is *not* found.

`controller/lmp.rs` models LMP (controller-to-controller, below HCI), so it emits
no host-facing events and was never the missing layer. It is now used inside
`sim.rs` for the connection handshake, with a host-gated `ConnectionPending`
state — because a controller may not answer a page by itself; the host does, with
Accept/Reject Connection Request.

What remains before Classic is Rhai-scriptable beside LE:

1. **An HFP SCO codec.** A2DP, AVRCP, and Classic HID are `ClassicHost` handlers,
   and HFP runs AT signalling over RFCOMM *and* a SCO/eSCO link that carries payload
   — but there is no codec: CVSD/mSBC air-coding is a seam, so no real speech audio.
   (The handlers and the SCO link themselves are done; see the table.)
2. **Rhai/MCP bindings.** Nothing constructs a classic device from a script yet.
   The Android names to use are tabulated in `docs/scripting-profile-apis.md`;
   discovery maps to `BluetoothAdapter.startDiscovery()`, not to a profile proxy.
3. **OBEX integration.** The library implements OBEX and an Object Push
   server/SDP record, but it is not connected to RFCOMM. PBAP and MAP layers are
   still absent.
4. **BNEP.** Absent, which leaves PAN unavailable.

| Peripheral | Current state | Remaining |
|---|---|---|
| Serial / OBD-II | Runs in a scene: `ClassicDevice` acceptor + initiator, SDP-advertised RFCOMM | Rhai/MCP adapter |
| Classic keyboards/mice/gamepads | `ClassicHidDevice`/`Host` handlers in a scene (`keyboard_scene`) | Rhai/MCP adapter |
| Headphones / speakers | `A2dpSink`/`Source` + `AvrcpTarget`/`Controller` handlers in a scene (`media_scene`, `speaker_scene`, Audio page) | Rhai/MCP adapter |
| Hands-free / headset | HFP AT signalling over RFCOMM + a SCO/eSCO link in a scene (Car page) | A SCO codec (CVSD/mSBC — the link works, the codec is a seam), then a Rhai/MCP adapter |
| File push (Bluetooth share) | OBEX + OPP library and SDP record | Wiring OBEX onto the existing RFCOMM handler |
| Phonebook / messaging | OBEX core only | PBAP/MAP profile layers and transport integration |
| Tethering | — | BNEP and network plumbing |

Most Android game controllers (Xbox, DualSense) pair over **Classic HID**, so the
Tier 1 HOGP gamepad is the LE variant, not those.

## Recommended build order

1. **Tier 1 breadth** — the LE peripherals above. Cheap, immediately usable from a
   phone, each exercises the existing path. *(This pass.)*
2. **Android LE Audio source interop** — validate the existing CIS and LC3 path
   with a phone, beyond the Bumble/liblc3 source test.
3. ~~**BIS / Auracast**~~ **Done 2026-08-23**, verified against Bumble both
   directions. What remains is `bass.rs` (see `docs/gaps.md`), not the transport.
4. ~~**BR/EDR scene/transport adapter**~~ **Done**: BR/EDR in `controller/sim.rs`
   and `SceneEngine::add_classic_device`. What remains is a Rhai/MCP surface.
5. ~~**SPP, then A2DP/AVRCP/HFP/HID handlers**~~ **Done**: all are registered
   `ClassicHost` handlers with scene builders, tests, and web demos. What remains
   is an HFP SCO codec (CVSD/mSBC — the link works), and a Rhai/MCP scripting
   surface for all of them.
6. **OBEX over RFCOMM** — make Object Push usable, then add PBAP and MAP layers.

Step 4 (the in-scene Classic host) and step 5 (the profile handlers) are done; a
Rhai/MCP surface is what turns them into scriptable devices.
