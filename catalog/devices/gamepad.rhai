// HOGP game controller. Note most console pads (Xbox, DualSense)
// pair over CLASSIC HID, not this — but LE gamepads use exactly this
// profile, and the report map is what makes Android map the axes.
let server = android::BluetoothGattServer("SimGamepad");
let hid = android::BluetoothGattService(uuid::from_u16(0x1812), android::SERVICE_TYPE_PRIMARY);

let info = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4A),
    android::PROPERTY_READ, android::PERMISSION_READ);
info.set_value([0x11, 0x01, 0x00, 0x03]);
hid.add_characteristic(info);

let map = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4B),
    android::PROPERTY_READ, android::PERMISSION_READ);
map.set_value([
    0x05, 0x01,       // Usage Page (Generic Desktop)
    0x09, 0x05,       // Usage (Game Pad)
    0xA1, 0x01,       // Collection (Application)
    0x09, 0x01,       //   Usage (Pointer)
    0xA1, 0x00,       //   Collection (Physical)
    0x09, 0x30, 0x09, 0x31, //   Usage X, Y — the left stick
    0x15, 0x81, 0x25, 0x7F, //   Logical -127..127
    0x75, 0x08, 0x95, 0x02,
    0x81, 0x02,       //     Input (Data,Var,Abs) — absolute stick position
    0xC0,             //   End Collection
    0x05, 0x09,       //   Usage Page (Button)
    0x19, 0x01, 0x29, 0x08, // Buttons 1..8
    0x15, 0x00, 0x25, 0x01,
    0x75, 0x01, 0x95, 0x08,
    0x81, 0x02,       //   Input (Data,Var,Abs) — 8 button bits
    0xC0,             // End Collection
]);
hid.add_characteristic(map);

let report = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4D),
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
report.set_value([0, 0, 0]); // [x, y, buttons]
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

// Sweeps the stick and cycles one button at a time.
fn tick(server, t) {
    let step = t.to_int();
    let x = (step * 16) % 127;
    let y = (step * 8) % 127;
    let buttons = 1 << (step % 8);
    server.update_value(uuid::from_u16(0x2A4D), [x, y, buttons]);
}
