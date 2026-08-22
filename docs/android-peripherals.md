# Android-supported Bluetooth peripherals in SimBLE

What it would take for SimBLE to emulate each peripheral type Android's
AOSP Bluetooth stack supports natively — assessed by reading the code, not
by counting files.

## The one distinction that decides everything

SimBLE has two very different kinds of "support", and conflating them is
how a feature list becomes wrong:

**Reachable** — the device can be added to a live scene and talked to by a
real central (an Android emulator over netsim, nRF Connect, Bumble). This
needs the whole path: simulated radio → `VirtualDevice` → GATT → Rhai
script → MCP `add_peripheral` / web page.

**Library-only** — a protocol state machine that exists, compiles, and is
unit-tested, but is driven Rust-to-Rust by handing byte buffers between two
instances. Nothing on the air, nothing a phone can connect to.

Everything LE is reachable. Everything Classic is library-only.

## What is actually in the tree

| Area | Code | Status |
|---|---|---|
| LE radio | `controller/sim.rs` `Link` | Reachable. Routes advertising, LE connections, ACL, and (new) ISO SDUs |
| LE host | `device/host.rs` `LeHost`, `device/virtual_device.rs` | Reachable |
| LE GATT profiles | `profiles/*` (~8.9k lines: ASCS, ANCS, AMS, BASS, AICS, BAP, HAP, MCS, ASHA, VOCS, PACS, CSIS, HRS, BAS, DIS, VCS, RAS…) | Rust service builders; a script reaches the same result via `android::*` |
| LE device models | `devices/*` (keyboard, mouse, HRM, iBeacon, Eddystone) | Rust models incl. **HOGP** (`devices/helpers/hid_device.rs`) |
| Classic protocols | `classic/*` (~13.6k lines: SDP, RFCOMM, AVDTP, AVCTP/AVC, AVRCP, HFP+AT, HID, A2DP) | **Library-only, but well tested** (~135 tests, mostly in `tests/`) |
| Classic link | `controller/lmp.rs`, `l2cap/classic.rs` | **Library-only** — `LmpLink` pairs are driven by hand |
| OBEX | *none* | Missing entirely |
| LC3 codec | *none* | Missing entirely |
| CIS/BIS establishment | *none* | ISO SDUs currently ride the ACL connection handle |

The Classic situation is better than "not implemented" and worse than
"supported": the *protocols* are largely written and well tested (~135
tests; `tests/classic_integration_test.rs` chains LMP → L2CAP → SDP/RFCOMM
end to end), but nothing in `scripting/`, `mcp.rs`, or `transport/`
references `crate::classic`, and `Link` speaks only LE. No Classic profile
can be put on the air today. It is an **integration** problem — see Tier 3.

## Feasibility tiers

### Tier 1 — Fits today

A GATT service plus a scripted device; no new subsystem. These are
reachable by an Android phone as soon as they are written.

| Peripheral | Profile | Notes |
|---|---|---|
| Heart rate monitor | HRS 0x180D | Shipped (`hrm` example) |
| Environmental sensor node | ESS 0x181A | Shipped (`env_sensor`) |
| Thermometer | HTS 0x1809 | Shipped (`thermometer`) |
| Battery / DIS device | 0x180F / 0x180A | Shipped (`battery`) |
| Volume control (LE Audio control plane) | VCS 0x1844 | Shipped (`volume`) |
| Fast Pair / iBeacon / Eddystone beacons | advertising only | Shipped (`fast_pair`); Eddystone added |
| **Keyboard** | HOGP 0x1812 | `hid_keyboard` — report map, input reports, protocol mode |
| **Mouse / trackpad** | HOGP 0x1812 | `hid_mouse` — relative motion reports |
| **Game controller** | HOGP 0x1812 | `gamepad` — 2 axes + 8 buttons (LE pads; console pads are Classic HID) |
| **Cycling speed & cadence** | CSCS 0x1816 | `cycling` — cumulative wheel/crank counters |
| **Pulse oximeter** | PLXS 0x1822 | `pulse_oximeter` — SpO2 + pulse as SFLOATs |
| **Smart scale** | WSS 0x181D + BCS 0x181B | `weight_scale` — both services, indicated |
| **Smart lock** | vendor 128-bit control point | `smart_lock` — lock/unlock + state notify |
| **Fitness tracker / smartwatch** | multi-service GATT | `fitness_tracker` — HRS + BAS + DIS + vendor steps |
| **Eddystone beacon** | 0xFEAA service data | `eddystone` — UID frame, non-connectable |
| Find My Device Network tag | FMDN over Fast Pair 0xFE2C | Advertising is reachable; the Google FMDN *protocol* (EIK, ephemeral IDs, provisioning) is a spec-compliance project, not a transport gap |

### Tier 2 — Needs one new subsystem

| Peripheral | Missing piece | Effort |
|---|---|---|
| LE Audio headphones / hearing aids (ASCS, HAP, CSIP) | **CIS establishment** (`LeSetCigParameters`, `LeCreateCis`, `LeAcceptCisRequest`, `LeSetupIsoDataPath` + the LE CIS Request/Established events) — the control plane and the ISO SDU transport already exist | ~1 week |
| …and to actually *hear* it | **LC3 codec** — Android streams LC3; no decoder exists. Options: a Rust LC3 crate, or Google's liblc3 compiled to wasm. WebCodecs cannot help: browsers do not implement LC3 | ~1 week + dependency decision |
| Auracast broadcast | **BIS** (`LeCreateBig`, `LeBigCreateSync`) on top of the same ISO plumbing | ~3 days after CIS |

netsim's rootcanal already implements the full ISO/CIS/BIS command set, and
the Android 14 emulator's stack lists `LE_AUDIO`, `HAP`, `VOLUME_CONTROL`
and `LE_BROADCAST_ASSISTANT` — so both peers are ready; only SimBLE is not.

### Tier 3 — Classic (BR/EDR): an integration problem, not a greenfield one

**This tier is much closer to done than a first look suggests, and is
handed off to the Classic workstream.** The protocol layers are written
*and well tested* — the gap is that none of it is connected to anything.

Measured coverage (inline `#[test]` + `tests/`), because inline counts
alone badly understate it:

| Module | Lines | Inline | Integration | Real total |
|---|---|---|---|---|
| `avrcp.rs` | 3316 | 0 | 35 (`avrcp_test.rs`) | **35** |
| `hfp.rs` | 2327 | 6 | — | 6 |
| `avdtp.rs` | 1979 | 0 | 16 (`avdtp_test.rs`) | **16** |
| `rfcomm.rs` | 1518 | 15 | 8 | **23** |
| `sdp.rs` | 1438 | 4 | 12 | **16** |
| `hid.rs` | 876 | 5 | 15 | **20** |
| `a2dp.rs` | 686 | 0 | 11 (`a2dp_test.rs`) | **11** |
| `avc.rs` / `avctp.rs` | 1023 | 0 | via `avrcp_test.rs` | indirect |
| `at.rs` | 416 | 7 | 2 | **9** |

`tests/classic_integration_test.rs` already chains **LMP → Classic L2CAP →
SDP** and **LMP → Classic L2CAP → RFCOMM** end to end between two simulated
peers, and touches `VirtualDevice`. So the layers compose correctly; they
are simply driven by hand-shuttling byte buffers in a test.

What is genuinely missing:

1. **BR/EDR in the radio** — `controller/sim.rs`'s `Link`/`SimController`
   handle only LE (advertising, LE connections, ACL, ISO); the single
   `CreateConnection` match is `LeCreateConnection`. Needs inquiry /
   inquiry scan, page / page scan, and Classic ACL handles.
   `controller/lmp.rs` models link establishment already but nothing drives
   it from the radio. *(netsim's rootcanal does provide a Classic
   controller — `Inquiry`, `CreateConnection`, `SetupSynchronousConnection`,
   `RemoteNameRequest` — so the netsim path may be reachable sooner than
   the in-process one.)*
2. **Classic L2CAP hung off `VirtualDevice`** — `l2cap/classic.rs` works
   standalone; it needs PSM routing on a device, as ATT has.
3. **SDP registered as a device service**, so a phone's discovery finds it.
4. **Script/MCP surface** — `android::*` bindings so a scripted device can
   declare an A2DP sink or an RFCOMM channel.
5. **OBEX** — genuinely absent (only a passing mention in `avrcp.rs`).
   Blocks PBAP, MAP, and OPP alike.
6. **BNEP** — absent. Blocks PAN.

Per-profile, once the foundation exists:

| Peripheral | Profile | State of the protocol code | Remaining |
|---|---|---|---|
| Serial / OBD-II | SPP over RFCOMM | RFCOMM + SDP, 39 tests | **Cheapest win: wiring only, ~2 days** |
| Classic keyboards/mice/gamepads | HID | `hid.rs`, 20 tests | Wiring, ~3 days |
| Headphones / speakers | A2DP + AVRCP | AVDTP 16, AVRCP 35, A2DP 11 tests; capability negotiation only | Wiring + AVDTP media path (RTP packetization), ~1 week |
| Hands-free / headset | HFP / HSP | `hfp.rs` + `at.rs`, 15 tests | Wiring + SCO/eSCO links in the radio, ~1.5 weeks |
| Phonebook | PBAP | — | **OBEX** + vCard, ~1.5 weeks |
| Messaging | MAP | — | **OBEX** + bMessage, ~1.5 weeks |
| File push (Bluetooth share) | OPP | — | **OBEX**, ~1 week |
| Tethering | PAN | — | **BNEP** + network plumbing, ~2 weeks |

Note that most Android game controllers (Xbox, DualSense) pair over
**Classic HID**, so the Tier 1 HOGP gamepad is the LE variant, not those.

## Bug found while building Tier 1

**Mixed 16-bit and 128-bit services break central-side discovery.** A
device that advertises standard services *and* a vendor 128-bit service
(the `fitness_tracker` example: HRS + BAS + DIS + a 128-bit step counter)
leaves `CentralDevice` stuck in `phase: "discovering characteristics"`,
reporting phantom services (`9E6F`, `5B9E`, `0001` — fragments of the
128-bit UUIDs read as 16-bit ones) and the same characteristics repeated
dozens of times.

A 128-bit service *on its own* discovers correctly, so the fault is in
how the discovery walk handles a mixed database — most likely the same
128-bit UUID mis-parse already noted against the `thermostat` example.
This affects the peripheral's usability from any central, so it is worth
fixing before the vendor-service examples are demoed. The device side is
unaffected: `status` (the god-view) reports every service correctly, which
is why `test_fitness_tracker_exposes_every_service` asserts against that
rather than through the central.

## Recommended build order

1. **Tier 1 breadth** — the LE peripherals above. Cheap, immediately usable
   from a phone, and each one exercises the existing path. *(This pass.)*
2. **CIS establishment** — unlocks LE Audio streaming end-to-end and is
   verifiable against Bumble (`setup_cig`/`create_cis`) before involving
   Android.
3. **LC3** — the only way Android's own audio reaches a SimBLE speaker.
4. **BR/EDR radio + Classic L2CAP + SDP-on-device** — the one investment
   that converts ~13.6k lines of already-written Classic protocol code from
   library-only into reachable.
5. **SPP first** on that foundation (2 days, high utility: Arduino, OBD-II),
   then A2DP/AVRCP, then HFP.
6. **OBEX** — unlocks PBAP, MAP, and OPP together.

The ordering point: step 4 is the highest leverage in the whole document.
It is a big piece of work, but it is the difference between a large body of
Classic code being a library and being a product feature.
