// Auracast broadcast source — android::BluetoothLeBroadcast.
//
// A profile with behaviour, reached from a script: the extended advertising
// set, the periodic train carrying the BASE, LE Create BIG and one ISO data
// path per BIS all live in Rust (crate::device::BigBroadcaster, checked
// against Bumble in both directions). The script only says what to publish.
//
// A source is also an ordinary GATT peripheral — a real Auracast transmitter
// is connectable and reports its battery — so the two coexist here, as they
// do in the controller: legacy advertising and an extended advertising set
// are separate objects.
let server = android::BluetoothGattServer("SimBLE Auracast");
let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
let level = android::BluetoothGattCharacteristic(uuid::BATTERY_LEVEL,
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
level.set_value([92]); // percent
level.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
bas.add_characteristic(level);
server.add_service(bas);

// BluetoothLeBroadcastSettings is a Java builder, and a map literal is the
// honest translation of one — so that is what start_broadcast takes.
let broadcast = android::BluetoothLeBroadcast("SimBLE Auracast");
broadcast.start_broadcast(#{
    broadcast_id: 0xC0FFEE,
    broadcast_code: (),                  // () = unencrypted
    subgroups: [ #{
        // The only *named* configuration, because it is the only one whose
        // BASE has been checked byte-for-byte against Bumble. Anything else
        // is spelled out as
        // #{sampling_frequency_hz, frame_duration_us, octets_per_frame}.
        codec: "lc3_48_2",
        // Stereo: BIS 1 is front left, BIS 2 front right — the allocation the
        // BASE publishes for two BISes.
        bis: ["FRONT_LEFT", "FRONT_RIGHT"],
        // Streaming_Audio_Contexts = Media (Assigned Numbers 6.12.6).
        metadata: [0x03, 0x02, 0x04, 0x00],
    } ],
});

// Android splits "the broadcast exists" from "audio is playing", and so does
// the ladder underneath: the BIG exists once LE Create BIG Complete lands,
// and SDUs only flow once every ISO data path is open.
// Rhai functions are pure and cannot see the calling scope, so `this` — the
// per-device map every handler shares — is where a callback stashes the
// object a later `fn tick` needs.
fn on_broadcast_started(broadcast, reason, broadcast_id) {
    this.source = broadcast;
    this.broadcast_id = broadcast_id;
}
fn on_playback_started(broadcast, reason, broadcast_id) {
    this.playing = true;
}
fn on_broadcast_start_failed(broadcast, reason) {
    this.playing = false;
}

fn tick(server, t) {
    // A silent LC3 frame is still a frame: the point is that both BISes carry
    // SDUs at all once playback has started. `send_audio` answers false while
    // the data paths are shut, which is when a controller would drop them.
    if this.playing == true {
        this.source.send_audio(1, blob(100, 0));
        this.source.send_audio(2, blob(100, 0));
    }
    // Battery drains slowly, so the device has something to notify.
    server.update_value(uuid::BATTERY_LEVEL, [92 - (t.to_int() / 30) % 10]);
}
