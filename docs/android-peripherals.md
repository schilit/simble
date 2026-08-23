# Android-supported Bluetooth peripherals in SimBLE

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

The current user-facing surface is scriptable LE GATT. Classic has a native
host, but no scriptable surface.

## What is actually in the tree

| Area | Availability | Notes |
|---|---|---|
| LE radio | **Scriptable** | `controller/sim.rs` `Link` routes advertising, LE connections, ACL, and ISO SDUs |
| LE host | **Scriptable** | `device/host.rs` `LeHost`, `device/virtual_device.rs` |
| LE GATT profiles | **Scriptable** | `profiles/*`; `android::*` scripts can create the same GATT result |
| LE device models | **Native device API** | `devices/*` models, including HOGP helpers |
| Classic host, SDP, and RFCOMM | **Native device API** | `ClassicHost` configures discovery, handles inbound ACL, L2CAP, SDP, and pluggable RFCOMM services over controller H4; no Rhai/MCP/web surface |
| Other Classic profiles | **Library-only** | `classic/*` contains A2DP, AVRCP, HFP, HID, and more, but they are not yet registered with a scene or a transport |
| Classic link | **Library-only** | `controller/lmp.rs` models link establishment; `LmpLink` peers are driven directly in tests |
| OBEX / Object Push | **Library-only** | Tested OBEX client/server plus an OPP server and SDP record; not yet wired to RFCOMM or a scene |
| LC3 codec | **Available in the web audio demo** | Optional `lc3` feature encodes and decodes mono LC3; it decodes frames from Google's liblc3, but is not a conformance claim |
| CIS establishment | **Available in the web audio demo** | The WebSocket controller establishes a CIS and carries ISO SDUs; verified with a Bumble source |
| BIS / Auracast transport | **Missing** | No BIG/BIS creation or synchronization |

Classic is not scriptable, but it is no longer just disconnected protocol
code: `ClassicHost` is a native H4-facing BR/EDR host. It still needs a
scene/transport adapter before an MCP, web, or Rhai-defined device can use
it. See Tier 3.

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
| Auracast broadcast | Implement BIS/BIG (`LeCreateBig`, `LeBigCreateSync`) on the ISO plumbing. |

netsim's rootcanal implements the ISO/CIS/BIS command set. SimBLE currently
uses the CIS portion for the web audio demo; broadcast support remains open.

### Tier 3 — Classic (BR/EDR): native host, not yet a scene

`ClassicHost` now covers the native path from controller H4 packets through
inbound ACL and Classic L2CAP to SDP and registered protocol handlers. It
includes an RFCOMM handler, so an embedding application can build an SPP-like
service without rebuilding those layers. Its module tests exercise controller
bring-up, connection acceptance, SDP, and RFCOMM.

What remains before Classic belongs beside LE in a SimBLE scene:

1. **A BR/EDR scene/transport adapter.** `Link` is LE-only, and neither the
   MCP nor Rhai surfaces construct a `ClassicHost`.
2. **Profile adapters.** A2DP, AVRCP, HFP, and Classic HID have protocol
   implementations but are not `ClassicHost` handlers yet. SCO/eSCO is also
   absent, so HFP audio cannot run.
3. **OBEX integration.** The library implements OBEX and an Object Push
   server/SDP record, but it is not connected to RFCOMM. PBAP and MAP profile
   layers are still absent.
4. **BNEP.** It is absent, which leaves PAN unavailable.

| Peripheral | Current state | Remaining |
|---|---|---|
| Serial / OBD-II | Native `ClassicHost` + RFCOMM handler | Scene/transport and scripting adapters |
| Classic keyboards/mice/gamepads | HID protocol library | ClassicHost handler and scene/transport adapter |
| Headphones / speakers | A2DP / AVRCP protocol libraries | Profile handler, scene/transport adapter, and media integration |
| Hands-free / headset | HFP / HSP protocol library | Profile handler, scene/transport adapter, and SCO/eSCO audio |
| File push (Bluetooth share) | OBEX + OPP library and SDP record | RFCOMM handler and scene/transport adapter |
| Phonebook / messaging | OBEX core only | PBAP/MAP profile layers and transport integration |
| Tethering | — | BNEP and network plumbing |

Note that most Android game controllers (Xbox, DualSense) pair over
**Classic HID**, so the Tier 1 HOGP gamepad is the LE variant, not those.

## Recommended build order

1. **Tier 1 breadth** — the LE peripherals above. Cheap, immediately usable
   from a phone, and each one exercises the existing path. *(This pass.)*
2. **Android LE Audio source interop** — validate the existing CIS and LC3
   path with a phone, beyond the Bumble/liblc3 source test.
3. **BIS / Auracast** — add broadcast transport on the existing ISO work.
4. **BR/EDR scene/transport adapter** — make the existing `ClassicHost`
   available to scenes, then to MCP, web, and Rhai.
5. **SPP first** on that foundation, then profile handlers for A2DP/AVRCP
   and HFP.
6. **OBEX over RFCOMM** — make Object Push usable, then add PBAP and MAP
   profile layers.

Step 4 is the leverage point: it turns the existing native Classic host into
a scriptable, on-air scene capability.
