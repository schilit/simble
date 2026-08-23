// Eddystone-UID: service data on 0xFEAA carrying a frame type, a
// calibrated TX power, and a 16-byte ID split into a 10-byte namespace
// (the operator) and a 6-byte instance (which beacon). Compare the
// fast_pair example — same advertising mechanism, different payload.
let server = android::BluetoothGattServer("Eddystone");
let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
let level = android::BluetoothGattCharacteristic(uuid::BATTERY_LEVEL,
    android::PROPERTY_READ, android::PERMISSION_READ);
level.set_value([95]);
bas.add_characteristic(level);
server.add_service(bas);

server.advertise_service_data(0xFEAA, [
    0x00,             // frame type: UID
    0xEB,             // ranging data: RSSI at 0 m, -21 dBm
    0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, // namespace
    0x00, 0x00, 0x00, 0x00, 0x00, 0x01, // instance
    0x00, 0x00,       // reserved
]);
server.advertise_connectable(false); // beacons broadcast, they do not connect
