// This test FAILS on purpose — the assertion doesn't hold, and its message
// tells you exactly what went wrong (this is what a caught regression looks like).
let server = android::BluetoothGattServer("HRM");
let hrs = android::BluetoothGattService(uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY);
hrs.add_characteristic(android::BluetoothGattCharacteristic(
    uuid::HEART_RATE_MEASUREMENT, android::PROPERTY_READ, android::PERMISSION_READ));
server.add_service(hrs);

let chr = server.get_service(uuid::HEART_RATE_SERVICE).get_characteristic(uuid::HEART_RATE_MEASUREMENT);
// The characteristic is READ-only, so this assertion is false:
assert((chr.properties & android::PROPERTY_NOTIFY) != 0, "expected the measurement to be notify-capable");
