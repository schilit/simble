// HOGP mouse — same shape as the keyboard, different report map.
// The report is [buttons, dx, dy, wheel] with dx/dy/wheel as SIGNED
// relative motion, which is why the descriptor declares Logical Minimum
// -127: read as unsigned, one step left becomes 255 steps right.
let server = android::BluetoothGattServer("SimMouse");
let hid = android::BluetoothGattService(uuid::from_u16(0x1812), android::SERVICE_TYPE_PRIMARY);

let info = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4A),
    android::PROPERTY_READ, android::PERMISSION_READ);
info.set_value([0x11, 0x01, 0x00, 0x03]);
hid.add_characteristic(info);

let map = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4B),
    android::PROPERTY_READ, android::PERMISSION_READ);
map.set_value([
    0x05, 0x01,       // Usage Page (Generic Desktop)
    0x09, 0x02,       // Usage (Mouse)
    0xA1, 0x01,       // Collection (Application)
    0x09, 0x01,       //   Usage (Pointer)
    0xA1, 0x00,       //   Collection (Physical)
    0x05, 0x09,       //     Usage Page (Button)
    0x19, 0x01, 0x29, 0x03, //   Buttons 1..3
    0x15, 0x00, 0x25, 0x01,
    0x95, 0x03, 0x75, 0x01,
    0x81, 0x02,       //     Input (Data,Var,Abs) — 3 button bits
    0x95, 0x01, 0x75, 0x05,
    0x81, 0x01,       //     Input (Const) — 5 bits padding
    0x05, 0x01,       //     Usage Page (Generic Desktop)
    0x09, 0x30, 0x09, 0x31, //   Usage X, Y
    0x09, 0x38,       //     Usage Wheel
    0x15, 0x81, 0x25, 0x7F, //   Logical -127..127
    0x75, 0x08, 0x95, 0x03,
    0x81, 0x06,       //     Input (Data,Var,Rel) — relative motion
    0xC0, 0xC0,       // End Collection x2
]);
hid.add_characteristic(map);

let report = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4D),
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
report.set_value([0, 0, 0, 0]); // [buttons, dx, dy, wheel]
report.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
let reference = android::BluetoothGattDescriptor(uuid::from_u16(0x2908),
    android::PERMISSION_READ);
reference.set_value([0x01, 0x01]);
report.add_descriptor(reference);
hid.add_characteristic(report);

let mode = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4E),
    android::PROPERTY_READ | android::PROPERTY_WRITE_NO_RESPONSE,
    android::PERMISSION_READ | android::PERMISSION_WRITE);
mode.set_value([0x01]);
hid.add_characteristic(mode);
server.add_service(hid);

// Walks the pointer around a square: four headings, 3 seconds each.
fn tick(server, t) {
    let leg = (t / 3.0).to_int() % 4;
    let dx = 0;
    let dy = 0;
    if leg == 0 { dx = 5; }
    if leg == 1 { dy = 5; }
    if leg == 2 { dx = 251; } // -5 as a signed byte
    if leg == 3 { dy = 251; }
    server.update_value(uuid::from_u16(0x2A4D), [0, dx, dy, 0]);
}
