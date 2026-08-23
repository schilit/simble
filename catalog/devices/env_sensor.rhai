// Environmental Sensing: temperature (2A6E) and humidity (2A6F)
// on the same service.
let server = android::BluetoothGattServer("EnvSense");
let ess = android::BluetoothGattService(uuid::from_u16(0x181A), android::SERVICE_TYPE_PRIMARY);
let temp = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A6E),
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
temp.set_value([0x00, 21]); // [flags, degrees C]
temp.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
let hum = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A6F),
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
hum.set_value([0x00, 45]); // [flags, percent RH]
hum.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
ess.add_characteristic(temp);
ess.add_characteristic(hum);
server.add_service(ess);

fn tick(server, t) {
    server.update_value(uuid::from_u16(0x2A6E), [0x00, 20 + t.to_int() % 4]);
}
