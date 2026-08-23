// Channel Sounding responder — a Bluetooth 6.0 ranging tag, the
// thing a phone measures its distance to (finder tags, car keys, "where
// did I leave it" trackers).
//
// The distance measurement itself is a CONTROLLER procedure: the two
// radios exchange tones and phase, and the host never sees the RF. What a
// phone talks to is this GATT service, which publishes the results — so
// this device models the reachable half, with `tick` standing in for the
// procedure's output.
let server = android::BluetoothGattServer("Ranger");
server.add_ras(); // Ranging Features, Real-Time Data, Control Point
server.advertise_service_uuid(0x185B);

// Real-Time Ranging Data is [f32 distance_metres, f32 confidence], little
// endian — the encoding RangingService::encode_ranging_data produces.
fn tick(server, t) {
    // A tag drifting slowly between 1 and 5 metres.
    let phase = (t / 4.0) % 2.0;
    let metres = if phase < 1.0 { 1.0 + phase * 4.0 } else { 5.0 - (phase - 1.0) * 4.0 };
    server.update_value(uuid::RANGING_REALTIME_DATA, f32_le(metres) + f32_le(0.87));
}
