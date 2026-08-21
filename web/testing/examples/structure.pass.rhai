// A device is a script; add assert() and it's a test.
let server = android::BluetoothGattServer("HRM");
let hrs = android::BluetoothGattService(uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY);
let hr = android::BluetoothGattCharacteristic(
    uuid::HEART_RATE_MEASUREMENT,
    android::PROPERTY_READ | android::PROPERTY_NOTIFY,
    android::PERMISSION_READ,
);
hr.set_value([0x00, 72]);
hrs.add_characteristic(hr);
server.add_service(hrs);

// These run now, at build time, against the real GATT stack.
assert(server.name == "HRM", "server keeps its name");
let svc = server.get_service(uuid::HEART_RATE_SERVICE);
assert(svc.characteristics.len() == 1, "one characteristic in the service");
let chr = svc.get_characteristic(uuid::HEART_RATE_MEASUREMENT);
assert((chr.properties & android::PROPERTY_NOTIFY) != 0, "measurement is notify-capable");
assert((chr.properties & android::PROPERTY_READ) != 0, "measurement is readable");
