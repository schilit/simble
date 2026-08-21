// heart_rate.rhai — this script IS the device.
//
// The page runs a real Simble host stack compiled to WebAssembly; the
// device's whole behavior is defined right here, in Rhai (a Rust-flavored
// scripting language — no compiler involved). Edit anything and press Run:
// the device is torn down and rebuilt from this text.
//
// Web-runtime conventions (see the note under this pane):
//   fn tick(server, t)              if defined, called ~10x/sec; t = seconds
//                                   since Run, so behavior can animate.
//   server.update_value(uuid, [..]) writes a characteristic's value into the
//                                   live GATT database; the page sends a real
//                                   notification to subscribed centrals
//                                   whenever the value changes.

let server = android::BluetoothGattServer("web-hrm");

let hrs = android::BluetoothGattService(
    uuid::HEART_RATE_SERVICE,
    android::SERVICE_TYPE_PRIMARY,
);

let hr = android::BluetoothGattCharacteristic(
    uuid::HEART_RATE_MEASUREMENT,
    android::PROPERTY_READ | android::PROPERTY_NOTIFY,
    android::PERMISSION_READ,
);
hr.set_value([0x00, 72]); // flags byte, then bpm
let cccd = android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE,
);
hr.add_descriptor(cccd);
hrs.add_characteristic(hr);

let location = android::BluetoothGattCharacteristic(
    uuid::of("2A38"), // Body Sensor Location
    android::PROPERTY_READ,
    android::PERMISSION_READ,
);
location.set_value([2]); // wrist
hrs.add_characteristic(location);

server.add_service(hrs);

// A resting heart that slowly breathes between ~64 and ~88 bpm.
fn tick(server, t) {
    let bpm = 76 + (12.0 * sin(t / 4.0)).to_int();
    server.update_value(uuid::HEART_RATE_MEASUREMENT, [0x00, bpm]);
}
