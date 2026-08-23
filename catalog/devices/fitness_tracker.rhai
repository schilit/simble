// A wearable is not one profile — it is a handful of services on one
// GATT server, which is what makes it a useful shape to copy: standard
// services where they exist (Heart Rate, Battery, Device Information)
// and a vendor service for everything the SIG never standardised (steps).
let server = android::BluetoothGattServer("FitBand");

let hrs = android::BluetoothGattService(uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY);
let hr = android::BluetoothGattCharacteristic(uuid::HEART_RATE_MEASUREMENT,
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
hr.set_value([0x00, 64]);
hr.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
hrs.add_characteristic(hr);
let location = android::BluetoothGattCharacteristic(uuid::BODY_SENSOR_LOCATION,
    android::PROPERTY_READ, android::PERMISSION_READ);
location.set_value([0x02]); // wrist
hrs.add_characteristic(location);
server.add_service(hrs);

let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
let level = android::BluetoothGattCharacteristic(uuid::BATTERY_LEVEL,
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
level.set_value([84]);
bas.add_characteristic(level);
server.add_service(bas);

// Device Information — how a phone labels the device in its UI.
let dis = android::BluetoothGattService(uuid::from_u16(0x180A), android::SERVICE_TYPE_PRIMARY);
let manufacturer = android::BluetoothGattCharacteristic(uuid::MANUFACTURER_NAME,
    android::PROPERTY_READ, android::PERMISSION_READ);
manufacturer.set_value([0x53, 0x69, 0x6D, 0x42, 0x4C, 0x45]); // "SimBLE"
dis.add_characteristic(manufacturer);
let model = android::BluetoothGattCharacteristic(uuid::MODEL_NUMBER,
    android::PROPERTY_READ, android::PERMISSION_READ);
model.set_value([0x42, 0x61, 0x6E, 0x64, 0x20, 0x31]); // "Band 1"
dis.add_characteristic(model);
server.add_service(dis);

// Vendor step counter: a 32-bit cumulative count, like the cycling sensor.
let steps_svc = android::BluetoothGattService(
    uuid::of("f1e20001-8c3d-4a5b-9e6f-000000000001"), android::SERVICE_TYPE_PRIMARY);
let steps = android::BluetoothGattCharacteristic(
    uuid::of("f1e20002-8c3d-4a5b-9e6f-000000000001"),
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
steps.set_value([0, 0, 0, 0]);
steps.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
steps_svc.add_characteristic(steps);
server.add_service(steps_svc);

// Heart rate wanders with activity; steps accumulate about 2 per second.
fn tick(server, t) {
    let seconds = t.to_int();
    let bpm = 64 + (seconds * 3) % 40;
    server.update_value(uuid::HEART_RATE_MEASUREMENT, [0x00, bpm]);
    let count = seconds * 2;
    let b0 = count & 0xFF;
    let b1 = (count >> 8) & 0xFF;
    let b2 = (count >> 16) & 0xFF;
    server.update_value(uuid::of("f1e20002-8c3d-4a5b-9e6f-000000000001"), [b0, b1, b2, 0]);
}
