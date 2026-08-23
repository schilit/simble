// PLXS continuous measurement. Values are SFLOATs: 16 bits split as a
// 4-bit signed exponent and a 12-bit signed mantissa. With exponent 0 the
// mantissa is just the integer, so 98% SpO2 is 0x0062 little-endian —
// which is why these look like plain numbers below.
let server = android::BluetoothGattServer("PulseOx");
let plxs = android::BluetoothGattService(uuid::from_u16(0x1822), android::SERVICE_TYPE_PRIMARY);

// Continuous Measurement: flags 0x00 (no extra fields) + SpO2 + pulse rate.
let continuous = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A5F),
    android::PROPERTY_NOTIFY, android::PERMISSION_READ);
continuous.set_value([0x00, 0x62, 0x00, 0x3E, 0x00]);
continuous.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
plxs.add_characteristic(continuous);

// Spot-check Measurement is indicated, not notified: it is a single
// reading the collector must acknowledge.
let spot = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A5E),
    android::PROPERTY_INDICATE, android::PERMISSION_READ);
spot.set_value([0x00, 0x62, 0x00, 0x3E, 0x00]);
spot.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
plxs.add_characteristic(spot);

let features = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A60),
    android::PROPERTY_READ, android::PERMISSION_READ);
features.set_value([0x00, 0x00]);
plxs.add_characteristic(features);
server.add_service(plxs);

// SpO2 drifts 96-99%, pulse 58-70 bpm.
fn tick(server, t) {
    let step = t.to_int();
    let spo2 = 96 + (step % 4);
    let pulse = 58 + (step % 13);
    server.update_value(uuid::from_u16(0x2A5F), [0x00, spo2, 0x00, pulse, 0x00]);
}
