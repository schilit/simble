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
> peripheral or a central. Classic needs the scene to learn a new kind of
> device first, and only then do bindings become the easy part.

### LE — a binding is the whole job

`SceneEngine` hosts four things: `add_peripheral`, `add_scanner`, `add_central`,
`add_scripted_central`. Every LE device is one of those, so the Rust already has
somewhere to live and the only missing piece is a name a script can say.

That covers the 17 unbound profiles, the Auracast source (wrapping
`BigBroadcaster`) and Channel Sounding (wrapping `CsInitiator`). Roughly
mechanical work against implementations that already have tests.

### Classic — blocked one layer below scripting

A2DP, AVRCP, HFP and Classic HID are **not** blocked by a missing Rhai binding.
`ClassicHost` appears nowhere in `wasm_ws.rs` or `scene/mod.rs`: there is no
path for a BR/EDR device to enter a scene at all. Its constructor takes an
`SdpServer` and it speaks H4 straight to a controller.

Adding `android::BluetoothA2dp` today would produce a script that compiles and
has nothing to attach to — which is worse than not having it, because it looks
like support.

The prerequisite is the scene/transport adapter described in
`docs/peripheral-support.md`: a fifth thing a scene can host. Once that exists,
four profiles unlock together and the bindings are as mechanical as the LE ones.

**So the two halves are different kinds of work.** The LE half is ~17
bindings against tested Rust. The Classic half is one piece of architecture.
Do not price them the same, and do not start the Classic bindings first.

## Deliberately out of scope

- **Classic profile bindings**, until the scene can host a BR/EDR device — see
  above. The blocker is architectural, not a naming exercise.
- **Permissions, of any kind.** Noted once here so nobody re-derives the
  question: they are Android's deployment policy, not an API shape, and they do
  not apply to a simulator.
