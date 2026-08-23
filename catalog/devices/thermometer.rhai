// Health Thermometer service. No named const for these assigned
// numbers yet, so lift the 16-bit values with uuid::from_u16.
let server = android::BluetoothGattServer("Thermo");
let hts = android::BluetoothGattService(uuid::from_u16(0x1809), android::SERVICE_TYPE_PRIMARY);
let temp = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A1C),
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
temp.set_value([0x00, 37]); // [flags, degrees C] — byte 1 is what assert checks
temp.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
hts.add_characteristic(temp);
server.add_service(hts);

fn tick(server, t) {
    let c = 36 + t.to_int() % 3;
    server.update_value(uuid::from_u16(0x2A1C), [0x00, c]);
}
