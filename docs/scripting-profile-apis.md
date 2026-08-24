# Profile APIs for scripts, in Android's shape

*Spec, 2026-08-23.*

## The problem

A script can build any GATT database by hand — services, characteristics,
descriptors, values. That covers a Heart Rate monitor or a thermometer
completely, because those profiles *are* a database.

It does not cover a profile with **behaviour**. A Volume Control Service is a
control-point state machine; a Scan Delegator parses Add Source and republishes
a Receive State; CSIP has set-membership crypto. That behaviour exists in Rust,
in `src/profiles/`, tested — and a script cannot reach it.

Today **17 of 20 profiles with a `register()` have no script binding**. The
three that do are `add_pacs`, `add_ascs`, `add_ras`, in
`wasm_ws.rs::register_web_extensions`, and their doc comment already states the
principle this spec generalises:

> "Real profile implementations, callable from a script. The protocol lives in
> Rust — a state machine with tests — and the script just composes a device out
> of them."

## The shape: Android's, and permissions are irrelevant

Android exposes profiles as **profile proxies**, not as GATT. An app does not
write a BASS control point; it calls
`BluetoothLeBroadcastAssistant.addSource(sink, metadata, isGroupOp)`.

Many of these are `@SystemApi` and gated on `BLUETOOTH_PRIVILEGED`, so a
third-party app cannot call them. **That does not constrain us.** simble is a
simulator; a script is not a Play Store app. The only thing the Android shape
buys is that a developer reading a script recognises what it does. Permission
annotations are Android's deployment policy and have no bearing here.

This also corrects an argument made earlier in the project's history: that
Auracast could not be a script because Rhai binds no HCI types. It does not
need them. `android::BluetoothGattServer` exposes no ATT PDUs either — the Rust
binding does that work underneath. A profile binding hides the layer below it;
that is what a profile is.

## What to build first

### 1. `android::BluetoothLeBroadcast` — the Auracast source

Wraps [`crate::device::BigBroadcaster`], which is verified against Bumble in
both directions (`tests/interop/auracast_*.py`).

Android:

    void startBroadcast(BluetoothLeBroadcastSettings settings)
    void startBroadcast(BluetoothLeAudioContentMetadata meta, byte[] code)
    void stopBroadcast(int broadcastId)
    void updateBroadcast(int broadcastId, BluetoothLeBroadcastSettings settings)
    boolean isPlaying(int broadcastId)
    List<BluetoothLeBroadcastMetadata> getAllBroadcastMetadata()

    callbacks: onBroadcastStarted(reason, broadcastId)
               onBroadcastStartFailed(reason)
               onBroadcastStopped(reason, broadcastId)
               onPlaybackStarted / onPlaybackStopped(reason, broadcastId)
               onBroadcastUpdated / onBroadcastUpdateFailed(reason, broadcastId)
               onBroadcastMetadataChanged(broadcastId, metadata)

Rhai:

    let broadcast = android::BluetoothLeBroadcast("SimBLE Auracast");
    broadcast.start_broadcast(#{
        broadcast_id: 0xC0FFEE,
        broadcast_code: (),                  // () = unencrypted
        subgroups: [ #{
            codec: "lc3_48_2",
            bis: ["FRONT_LEFT", "FRONT_RIGHT"],
        } ],
    });

    fn on_broadcast_started(broadcast, reason, broadcast_id) { … }
    fn on_playback_started(broadcast, reason, broadcast_id) { … }

`BluetoothLeBroadcastSettings` is a Java builder and Rhai has no idiomatic
equivalent; a map literal is the honest translation. Do not invent a builder.

### 2. `android::BluetoothLeBroadcastAssistant` — the phone

Pure GATT underneath: it writes BASS Add Source and reads the Broadcast Receive
State back. Nothing below GATT is involved.

    void addSource(BluetoothDevice sink, BluetoothLeBroadcastMetadata m, boolean isGroupOp)
    void modifySource(BluetoothDevice sink, int sourceId, BluetoothLeBroadcastMetadata m)
    void removeSource(BluetoothDevice sink, int sourceId)
    void startSearchingForSources(List<ScanFilter> filters)
    List<BluetoothLeBroadcastReceiveState> getAllSources(BluetoothDevice sink)

    let assistant = android::BluetoothLeBroadcastAssistant("Phone");
    assistant.start_searching_for_sources();

    fn on_source_found(assistant, metadata) {
        assistant.add_source(sink, metadata, true);
    }
    fn on_receive_state_changed(assistant, sink, state) {
        assert(state.pa_sync_state == "SynchronizedToPa", "the earbud joined");
    }

**Why this pair matters most.** It makes the Assistant↔Delegator conversation
scriptable on *both* sides, which turns a scene into a test — the arrangement
the Generic domain already demonstrates. It is also the case that would have
caught `bass.rs` reporting `SynchronizedToPa` unconditionally: a scripted
Assistant asserting that state against a Delegator that always said it was
testing nothing.

### 3. The remaining 17, in the same shape

| Rust profile | Android proxy | Note |
|---|---|---|
| `bass` | `BluetoothLeBroadcastAssistant` (client) / the service is the Delegator | Item 2 |
| `vcp`, `vocs`, `aics` | `BluetoothVolumeControl` | Control-point state machines a script cannot hand-build |
| `csip` | `BluetoothCsipSetCoordinator` | Set membership + SIRK/RSI crypto. `sih()` currently has one caller and it is a test |
| `hap` | `BluetoothHapClient` | Hearing Access presets |
| `asha` | `BluetoothHearingAid` | |
| `mcp` | Media Control | Android surfaces this through `MediaSession`, not a Bluetooth proxy — pick a name deliberately rather than inventing one |
| `ams`, `ancs` | *(none)* | Apple services. An iPhone is the server; Android has no proxy. Name them for what they are |
| `tmap`, `gmap`, `cap` | *(none)* | Umbrella profiles, mostly capability declarations |
| `bas`, `dis`, `hrs`, `gatt_service` | *(n/a)* | Plain databases. A script can already build these by hand; a binding is convenience, not capability. **Lowest priority** |

## Rules for the binding layer

1. **The protocol stays in Rust.** A binding composes an existing
   `profiles::*::register()`; it does not reimplement behaviour in Rhai.
2. **Follow the callback convention already in use.** Android's `Callback`
   object becomes free functions with the object prepended —
   `on_broadcast_started(broadcast, reason, broadcast_id)` — matching
   `on_services_discovered(client)` in the central role.
3. **Do not expose HCI.** If a binding needs an HCI type in Rhai, the seam is
   wrong: the binding should own that and hand the script a profile-level view.
4. **Names mirror Android, cased for Rhai.** `startBroadcast` →
   `start_broadcast`. The type keeps Android's name exactly.
5. **Where Android has no equivalent, say so** in the doc comment rather than
   inventing a plausible-looking Android name. An invented API that looks real
   is the worst outcome — this project has already shipped four invented UUIDs.
6. **Every binding needs a catalog entry** in `catalog/devices/*.rhai`, which
   `tests/catalog_test.rs` builds and ticks. A binding with no runnable example
   is undocumented.

## Where a binding is enough, and where it is not

The general rule, because it decides how much work each profile is:

> **A native support function is enough when the device already fits something
> a scene can host.** For LE that is everything, because every LE device is a
> peripheral or a central. Classic needed *two* things first — a controller
> that speaks BR/EDR and a scene slot to put a device in — and both now
> exist, so its bindings are the easy part too.

### LE — a binding is the whole job

`SceneEngine` hosts four things: `add_peripheral`, `add_scanner`, `add_central`,
`add_scripted_central`. Every LE device is one of those, so the Rust already has
somewhere to live and the only missing piece is a name a script can say.

That covers the 17 unbound profiles, the Auracast source (wrapping
`BigBroadcaster`) and Channel Sounding (wrapping `CsInitiator`). Roughly
mechanical work against implementations that already have tests.

### Classic — the layer below scripting is now built

A2DP, AVRCP, HFP and Classic HID are **not** blocked by a missing Rhai binding.
They were blocked by two things below it, and both are now done:

1. **The simulated controller spoke only LE.** This was the real blocker, and
   an earlier version of this document did not name it. `SceneEngine`'s
   `Link` (`controller/sim.rs`) had no inquiry, no scan enable, no paging and
   no BR/EDR Connection Complete — so even with a scene slot, a `ClassicHost`
   would have sat idle for ever: it consumes H4 and emits H4, and nothing was
   ever going to send it a Connection Request. `sim.rs` now models scan
   enable, inquiry, paging, Remote Name Request and classic ACL routing.
2. **The scene could not host a BR/EDR device.** `SceneEngine::add_classic_device`
   is now the fifth thing a scene can host, beside `add_peripheral`,
   `add_scanner`, `add_central` and `add_scripted_central`.

Two `ClassicHost`s in one scene now inquire, page, open L2CAP, query SDP,
open the advertised RFCOMM channel and exchange data with no netsim and no
radio — see `classic_scene_tests` in `transport/wasm_ws.rs`.

**Two corrections to what this document used to say.** `ClassicHost`'s
constructor does *not* take an `SdpServer`: it is `ClassicHost::new(name,
class_of_device)`, and handlers — `SdpHandler`, `RfcommHandler`, and anything
else implementing `ProtocolHandler` — are registered separately with
`register_handler`. And `controller/lmp.rs` is easy to over-trust: it models
LMP, which is controller-to-controller *below* HCI, so it produces no
host-facing events. It is now used inside `sim.rs` for the connection
handshake (with a new host-gated `ConnectionPending` state, because answering
a page is the host's decision), but it was never the missing layer.

**What is left for the profile bindings** is now genuinely the binding work.
The `ProtocolHandler` half is done for three of the four: `device::a2dp`
(`A2dpSource`/`A2dpSink`), `device::classic_hid`
(`ClassicHidHost`/`ClassicHidDevice`) and `device::avrcp`
(`AvrcpController`/`AvrcpTarget`) are registered handlers with scenes
(`SpeakerScene`, `KeyboardScene`, `RemoteControlScene`/`MediaPlayerScene`) and
tests through the simulated BR/EDR link. HFP still has a protocol
implementation in `classic/hfp.rs` and no handler.

`ProtocolHandler` gained what those two needed: `psms()` so one handler can
claim Control **and** Interrupt, `on_channel_data(HandlerChannel, ..)` so a
handler is told which channel spoke, and `poll_channel_requests()` so AVDTP
can ask the host for its media transport channel. Single-PSM handlers were
not touched.

### Android names for the Classic surface

When those bindings are written, they should use Android's API set, exactly
as the LE ones do — `@SystemApi` is not a reason to avoid a name.

The **Rust type** column is what a binding would wrap. It is filled in only
where the handler exists; an empty cell is the honest statement that there is
nothing yet to bind.

| simble | Rust type | Android proxy |
|---|---|---|
| A2DP source / sink | `device::a2dp::A2dpSource` / `A2dpSink` | `BluetoothA2dp` / `BluetoothA2dpSink` |
| AVRCP controller | `device::avrcp::AvrcpController` | `BluetoothAvrcpController` — see the note below |
| AVRCP target | `device::avrcp::AvrcpTarget` | — (no proxy; see below) |
| HFP AG / HF | — (`classic/hfp.rs`, no handler) | `BluetoothHeadset` / `BluetoothHeadsetClient` |
| Classic HID host / device | `device::classic_hid::ClassicHidHost` / `ClassicHidDevice` | `BluetoothHidHost` / `BluetoothHidDevice` |

**AVRCP is where the correspondence is thinnest, and rule 5 applies twice.**

`BluetoothAvrcpController` is the closest Android has to `AvrcpController`,
and it is not close. It is the proxy for Android acting as an AVRCP
*controller* — Android in car-kit mode, paired with a phone that holds the
player — which is the same role `AvrcpController` plays. But it is
**deprecated** and its public surface is two methods,
`getConnectedDevices()` and `getConnectionState()`. There is no
`play()`, no `pause()`, no `getPlayStatus()`: on Android those live behind
`MediaController` / `MediaSession`, which is a **media framework** API, not a
Bluetooth one, and the AVRCP transport underneath it is not addressable from
an app at all. So a binding named `BluetoothAvrcpController` that exposed
`play` and `pause` would be inventing exactly the plausible-looking API rule 5
forbids. Two honest options when the binding is written: keep Android's name
and expose only what Android exposes, putting the transport keys on a
`MediaController`-shaped companion; or say in the doc comment that the name is
borrowed for the *role* and the methods have no Android counterpart.

`AvrcpTarget` has **no Android proxy at all**. Android is the target whenever
it is the phone streaming A2DP, but it reaches that role through
`MediaSession` — a media API that happens to be published over Bluetooth by
the system — and there is no `BluetoothAvrcpTarget` to mirror. The cell is
empty rather than filled with a guess.

One name needs care, and `src/scripting/hid.rs`'s module doc is the long
version: **`BluetoothHidHost` is not Classic-only.** Android's proxy spans
both transports — `HidHostService.java` has no transport-specific code, and
the split happens down in `bta/hh/`. The existing `android::BluetoothHidHost`
binding is the HOGP (LE) half; `ClassicHidHost` is the *same role on the
other transport*, and the `Classic` prefix exists because Rust needs two
names, not because Bluetooth has two roles. A binding that covers both should
be one binding.

The one that maps onto the **controller** layer built here is not a profile
proxy at all. Android's discovery surface is `BluetoothAdapter.startDiscovery()`
with the `ACTION_FOUND` broadcast — which is HCI Inquiry plus Inquiry Result —
and the remote name is `BluetoothDevice.getName()`, which is HCI Remote Name
Request. Both are now real underneath.

## Deliberately out of scope

- **Classic profile bindings.** The architectural blocker is gone — a scene
  hosts BR/EDR devices, and A2DP and Classic HID are `ProtocolHandler`s with
  passing scene tests. What is left is the naming exercise itself, and the
  table above is the answer to it; nobody needs to re-derive it.
- **Permissions, of any kind.** Noted once here so nobody re-derives the
  question: they are Android's deployment policy, not an API shape, and they do
  not apply to a simulator.
