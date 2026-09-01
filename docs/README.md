# docs/

Design notes, format references, and decision records for simble. Each file
states what it covers in its first lines.

## Reference — a format or surface as it is

| File | Covers |
|---|---|
| `scene-format.md` | The scene JSON format (authoritative source: `src/scene/`). |
| `api-surface.md` | Which modules are supported API vs. exposed only for inspection, and how the `testing` feature keeps `tests/` from widening the surface. |
| `usb-controllers.md` | Running on real hardware: choosing a controller, flashing an nRF52840, and the Channel Sounding situation. |
| `scripting-profile-apis.md` | Profile APIs available to scripts (Android's shape); 17 of 20 profiles have no binding yet. |
| `peripheral-support.md` | What it takes to emulate each Android peripheral type; scriptable vs. library-only. |

## Status — what's missing, what's proven

| File | Covers |
|---|---|
| `gaps.md` | What is missing or faked, and where each gap is declared in code or UI. Carries its re-derivation commands. |
| `test-strategy.md` | What the tests prove and can't prove; where the oracle gaps are. |
| `roadmap.md` | The task tracker. |

## Design & decisions — why a choice was made

| File | Covers |
|---|---|
| `controller-routing.md` | The v1 control protocol and controller routing — how a device's controller is backed (sim ether or real radio) and why that can't live in netsim. Device router built; the async backend router is a separate crate. |
| `measurement-regions.md` | Proposed API instrumentation: paired open/close regions, the accepted-vs-completed distinction, correlation. Not implemented. |
| `phone-as-backend.md` | The phone as a first-class backend — the script runs on the device, no network in the loop. Supersedes `android-rpc-peer.md`. |
| `phone-to-phone.md` | The phone-to-phone bulk-transfer path: the phone's own GATT client as central, an optional L2CAP payload channel, and measured throughput. |
| `sig-as-oracle.md` | What the Bluetooth SIG publishes, what a script can consume, and the licensing position. |
| `sbc-evaluation.md` | SBC options and licensing for the A2DP media path. |
| `lc3-evaluation.md` | LC3 options for the wasm demo devices. |
| `bdd-evaluation.md` | Whether BDD is worth it: not as a runner; worth trying as an audited spec. |
| `rfcomm-comparison.md` | simble vs. Bumble vs. Zephyr on RFCOMM. |
| `controller-bridging.md` | Getting a real-radio device and a simulated one to talk (phy↔sim): why it's a hardware problem, not software (sim↔sim is just relocation). Why Bumble isn't the vehicle. |
| `decisions-2026-08-23.md` | Two decisions: L2CAP dispatch keyed on PSM (not `(psm, cid)`), and why `tests/`' `run_until` ticks before it checks. |
| `android-rpc-peer.md` | Superseded by `phone-as-backend.md`; kept for its Android-API boundary analysis (GATT reachable, everything below it not). |
| `HANDOFF-2026-08-22.md` | An early session handoff. Partly stale — §3 is wrong (CIS and LC3 both exist). |
