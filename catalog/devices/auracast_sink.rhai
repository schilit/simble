// Auracast earbud — the Scan Delegator half of BASS.
//
// Android has no proxy for *being* a Scan Delegator; a phone is always the
// Assistant. So this side keeps the registrar spelling the other Rust
// profiles use: `add_bass(n)` installs the real
// crate::profiles::BroadcastAudioScanService, control point and all. A
// Broadcast Assistant then writes Add Source to it and reads the Broadcast
// Receive State back — which is all ordinary GATT.
let server = android::BluetoothGattServer("SimBLE Earbud");

// One Broadcast Receive State slot, the minimum BASS Section 3.2 allows.
server.add_bass(1);
// PACS says what this earbud can decode; without it a phone has nothing to
// match a broadcast's BASE against.
server.add_pacs(0x01, 0x00);   // sink location: front left
server.advertise_service_uuid(0x184F);
server.advertise_service_uuid(0x1850);

// The radio, standing in for a controller.
//
// Add Source *requests* a synchronisation; it does not achieve one, and the
// service is careful to report only SyncInfoRequest or NotSynchronizedToPa
// until something with a radio says otherwise. On a real earbud that
// something is crate::device::BigReceiver — periodic advertising sync, BASE,
// BIGInfo, LE BIG Create Sync. This device has no receiver wired up, so the
// script stands in for one and reports success after a short delay. That is
// a demonstration, not a measurement: nothing here has actually heard a
// broadcast.
fn tick(server, t) {
    for state in server.receive_states() {
        if state.pa_sync_state == "SynchronizedToPa" { continue; }
        // Both BISes of a stereo broadcast: bits 0 and 1 are BIS 1 and 2.
        server.report_sync_outcome(state.source_id, "SynchronizedToPa", 0x03);
    }
}
