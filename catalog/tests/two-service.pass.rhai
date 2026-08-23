// A two-service device; assert both are present and correctly shaped.
let server = android::BluetoothGattServer("Wearable");

let hrs = android::BluetoothGattService(uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY);
hrs.add_characteristic(android::BluetoothGattCharacteristic(
    uuid::HEART_RATE_MEASUREMENT, android::PROPERTY_NOTIFY, android::PERMISSION_READ));
server.add_service(hrs);

let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
bas.add_characteristic(android::BluetoothGattCharacteristic(
    uuid::BATTERY_LEVEL, android::PROPERTY_READ, android::PERMISSION_READ));
server.add_service(bas);

assert(server.get_service(uuid::HEART_RATE_SERVICE).characteristics.len() == 1, "HRS has one char");
assert(server.get_service(uuid::BATTERY_SERVICE).characteristics.len() == 1, "BAS has one char");
let batt = server.get_service(uuid::BATTERY_SERVICE).get_characteristic(uuid::BATTERY_LEVEL);
assert((batt.properties & android::PROPERTY_NOTIFY) == 0, "battery level is read-only, not notify");
