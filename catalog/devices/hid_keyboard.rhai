// HOGP keyboard. The Report Map (2A4B) is a USB HID report
// descriptor: it tells the host how to interpret the bytes that arrive
// on the Report characteristic, so the same 8-byte report becomes
// keystrokes. A Report Reference descriptor (2908) tags each report with
// its ID and direction, which is how a host tells inputs from outputs.
let server = android::BluetoothGattServer("SimKeyboard");
let hid = android::BluetoothGattService(uuid::from_u16(0x1812), android::SERVICE_TYPE_PRIMARY);

// bcdHID 1.11, country 0 (not localized), flags: remote wake + normally connectable.
let info = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4A),
    android::PROPERTY_READ, android::PERMISSION_READ);
info.set_value([0x11, 0x01, 0x00, 0x03]);
hid.add_characteristic(info);

let map = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4B),
    android::PROPERTY_READ, android::PERMISSION_READ);
map.set_value([
    0x05, 0x01,       // Usage Page (Generic Desktop)
    0x09, 0x06,       // Usage (Keyboard)
    0xA1, 0x01,       // Collection (Application)
    0x05, 0x07,       //   Usage Page (Keyboard/Keypad)
    0x19, 0xE0, 0x29, 0xE7, // Usage Min/Max (modifier keys)
    0x15, 0x00, 0x25, 0x01, // Logical 0..1
    0x75, 0x01, 0x95, 0x08, // 8 x 1-bit
    0x81, 0x02,       //   Input (Data,Var,Abs) — modifier byte
    0x95, 0x01, 0x75, 0x08,
    0x81, 0x01,       //   Input (Const) — reserved byte
    0x95, 0x06, 0x75, 0x08, // 6 x 8-bit
    0x15, 0x00, 0x25, 0x65, // Logical 0..101
    0x05, 0x07, 0x19, 0x00, 0x29, 0x65,
    0x81, 0x00,       //   Input (Data,Array) — the 6 key slots
    0xC0,             // End Collection
]);
hid.add_characteristic(map);

// The input report: [modifiers, reserved, key1..key6].
let report = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4D),
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
report.set_value([0, 0, 0, 0, 0, 0, 0, 0]);
report.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
// Report Reference: report ID 1, type 1 (Input).
let reference = android::BluetoothGattDescriptor(uuid::from_u16(0x2908),
    android::PERMISSION_READ);
reference.set_value([0x01, 0x01]);
report.add_descriptor(reference);
hid.add_characteristic(report);

// Protocol Mode: 1 = Report (0 would be Boot).
let mode = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4E),
    android::PROPERTY_READ | android::PROPERTY_WRITE_NO_RESPONSE,
    android::PERMISSION_READ | android::PERMISSION_WRITE);
mode.set_value([0x01]);
hid.add_characteristic(mode);

// HID Control Point: the host writes 0x00 (suspend) / 0x01 (exit suspend).
let control = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4C),
    android::PROPERTY_WRITE_NO_RESPONSE, android::PERMISSION_WRITE);
hid.add_characteristic(control);
server.add_service(hid);

// Types "hello" on a loop: press a key, then release it. A real keyboard
// sends the same two reports — a key is held until an empty report.
fn tick(server, t) {
    let keys = [0x0B, 0x08, 0x0F, 0x0F, 0x12]; // HID usage codes: h e l l o
    let step = (t * 2.0).to_int();
    let slot = (step / 2) % 5;
    let key = keys[slot];
    if step % 2 == 1 { key = 0; } // the release report
    server.update_value(uuid::from_u16(0x2A4D), [0, 0, key, 0, 0, 0, 0, 0]);
}
