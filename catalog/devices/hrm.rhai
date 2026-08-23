// Heart Rate service with a measurement that changes over time.
let server = android::BluetoothGattServer("HRM");
let hrs = android::BluetoothGattService(uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY);
let hr = android::BluetoothGattCharacteristic(uuid::HEART_RATE_MEASUREMENT,
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
hr.set_value([0x00, 72]); // [flags, bpm]
hr.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
hrs.add_characteristic(hr);
server.add_service(hrs);

// Optional: runs on every scene tick; update_value pushes notifications
// to subscribed centrals.
fn tick(server, t) {
    let bpm = 68 + (t * 2.0).to_int() % 9;
    server.update_value(uuid::HEART_RATE_MEASUREMENT, [0x00, bpm]);
}
