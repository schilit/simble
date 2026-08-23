// Fast Pair beacon: what makes Android pop the pairing sheet. The
// identity lives in the ADVERTISEMENT, not the GATT — service data on
// the Fast Pair UUID (FE2C) carrying a 3-byte Model ID. The same
// advertise_* calls build any beacon (Eddystone, Quick Share nudge).
let server = android::BluetoothGattServer("FastPairBeacon");
let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
let level = android::BluetoothGattCharacteristic(uuid::BATTERY_LEVEL,
    android::PROPERTY_READ, android::PERMISSION_READ);
level.set_value([88]); // percent
bas.add_characteristic(level);
server.add_service(bas);

server.advertise_service_data(0xFE2C, [0x00, 0x11, 0x22]); // Model ID
server.advertise_manufacturer_data(0x00E0, [0x01]); // 0x00E0 = Google
server.advertise_connectable(false); // a real beacon is broadcast-only
