// CSCS sensor. The measurement carries CUMULATIVE revolution counts
// plus the time of the last event (1/1024 s units) — the phone computes
// speed and cadence by differentiating between notifications, which is
// why a counter that only ever increases is the right model.
let server = android::BluetoothGattServer("CadenceSensor");
let cscs = android::BluetoothGattService(uuid::from_u16(0x1816), android::SERVICE_TYPE_PRIMARY);

// Flags bit0 = wheel data present, bit1 = crank data present.
let measurement = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A5B),
    android::PROPERTY_NOTIFY, android::PERMISSION_READ);
measurement.set_value([0x03, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
measurement.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
cscs.add_characteristic(measurement);

// CSC Feature: wheel + crank revolution data supported.
let feature = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A5C),
    android::PROPERTY_READ, android::PERMISSION_READ);
feature.set_value([0x03, 0x00]);
cscs.add_characteristic(feature);

// Sensor Location: 5 = left crank.
let location = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A5D),
    android::PROPERTY_READ, android::PERMISSION_READ);
location.set_value([0x05]);
cscs.add_characteristic(location);

// SC Control Point — this is where a phone resets the odometer.
let control = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A55),
    android::PROPERTY_WRITE | android::PROPERTY_INDICATE,
    android::PERMISSION_WRITE);
// CSCS 1.1, 3.3: the SC Control Point indicates its result, so it needs a
// CCCD for the client to enable those indications.
control.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
cscs.add_characteristic(control);
server.add_service(cscs);

// ~1 wheel rev/s (roughly 8 km/h on a 700c wheel) and 80 rpm cranks.
fn tick(server, t) {
    let seconds = t.to_int();
    let wheel = seconds;
    let crank = (seconds * 4) / 3;
    let event = (seconds * 1024) % 65536;
    let w0 = wheel & 0xFF;
    let w1 = (wheel >> 8) & 0xFF;
    let c0 = crank & 0xFF;
    let c1 = (crank >> 8) & 0xFF;
    let e0 = event & 0xFF;
    let e1 = (event >> 8) & 0xFF;
    server.update_value(uuid::from_u16(0x2A5B),
        [0x03, w0, w1, 0, 0, e0, e1, c0, c1, e0, e1]);
}
