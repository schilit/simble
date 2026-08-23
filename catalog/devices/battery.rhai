// Battery service: one static read-only value. The smallest
// complete peripheral.
let server = android::BluetoothGattServer("Batt");
let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
let level = android::BluetoothGattCharacteristic(uuid::BATTERY_LEVEL,
    android::PROPERTY_READ, android::PERMISSION_READ);
level.set_value([100]); // percent
bas.add_characteristic(level);
server.add_service(bas);
