// A finder tag (car key, luggage tag, "where are my keys"): the
// device a phone ranges to. Same Ranging Service as `ranging`, plus the
// battery every real tag exposes and a name a phone will show.
//
// Pair this with `ranging` to have two ranging devices on the air at once
// — a phone measuring distance to several tags is the actual use case.
let server = android::BluetoothGattServer("FinderTag");
server.add_ras();

let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
let level = android::BluetoothGattCharacteristic(uuid::BATTERY_LEVEL,
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
level.set_value([92]);
level.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
bas.add_characteristic(level);
server.add_service(bas);

server.advertise_service_uuid(0x185B);

fn tick(server, t) {
    // Held still, so the estimate jitters around 2.4 m the way a real
    // phase-based measurement does rather than sitting perfectly still.
    let jitter = ((t * 3.0).to_int() % 7).to_float() / 100.0;
    server.update_value(uuid::RANGING_REALTIME_DATA, f32_le(2.4 + jitter) + f32_le(0.91));
}
