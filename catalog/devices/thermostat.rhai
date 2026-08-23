// Thermostat: the SIG has no thermostat service, so like real BLE
// thermostats this pairs standard Environmental Sensing temperature
// (read/notify) with a custom 128-bit writable setpoint. Set it from a
// central with the write tool; the room then drifts toward the target.
let server = android::BluetoothGattServer("Thermostat");

let ess = android::BluetoothGattService(uuid::from_u16(0x181A), android::SERVICE_TYPE_PRIMARY);
let temp = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A6E),
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
temp.set_value([0x00, 18]); // [flags, degrees C]
temp.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
ess.add_characteristic(temp);
server.add_service(ess);

let ctl = android::BluetoothGattService(uuid::of("5e7b0001-c0de-4a11-b1e5-0000c0ffee01"),
    android::SERVICE_TYPE_PRIMARY);
let setpoint = android::BluetoothGattCharacteristic(uuid::of("5e7b0002-c0de-4a11-b1e5-0000c0ffee01"),
    android::PROPERTY_READ | android::PROPERTY_WRITE,
    android::PERMISSION_READ | android::PERMISSION_WRITE);
setpoint.set_value([21]); // target degrees C — a central write replaces this
ctl.add_characteristic(setpoint);
server.add_service(ctl);

// fn tick keeps no variables between calls — the GATT database is the
// device's state. server.value(uuid) reads it back, central writes included.
fn tick(server, t) {
    let target = server.value(uuid::of("5e7b0002-c0de-4a11-b1e5-0000c0ffee01"))[0];
    let current = server.value(uuid::from_u16(0x2A6E))[1];
    if current < target { current += 1; }
    if current > target { current -= 1; }
    server.update_value(uuid::from_u16(0x2A6E), [0x00, current]);
}
