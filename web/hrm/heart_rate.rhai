// heart_rate.rhai — this script IS the device.
//
// Legacy filename: this default now builds a THERMOMETER, not a heart-rate
// monitor. That's the point — the page renders whatever GATT structure the
// running script creates, so an AI-generated device of any type is legible.
// Generate a different device with AI (or click a suggestion chip), paste the
// returned Rhai here, and press Run to watch the live viewer re-render.
//
// The page runs a real Simble host stack compiled to WebAssembly; the device's
// whole behavior is defined right here, in Rhai (a Rust-flavored scripting
// language — no compiler involved). Edit anything and press Run: the device is
// torn down and rebuilt from this text.
//
// Web-runtime conventions (see the note under this pane):
//   fn tick(server, t)              if defined, called ~10x/sec; t = seconds
//                                   since Run, so behavior can animate.
//   server.update_value(uuid, [..]) writes a characteristic's value into the
//                                   live GATT database; the page sends a real
//                                   notification to subscribed centrals
//                                   whenever the value changes.

let server = android::BluetoothGattServer("web-thermometer");

// Environmental Sensing (0x181A) with a Temperature characteristic (0x2A6E):
// a signed 16-bit value in hundredths of a degree Celsius, little-endian.
let ess = android::BluetoothGattService(
    uuid::of("181A"),
    android::SERVICE_TYPE_PRIMARY,
);
let temp = android::BluetoothGattCharacteristic(
    uuid::of("2A6E"), // Temperature
    android::PROPERTY_READ | android::PROPERTY_NOTIFY,
    android::PERMISSION_READ,
);
temp.set_value([0x66, 0x08]); // 0x0866 = 2150 = 21.50 °C
let cccd = android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE,
);
temp.add_descriptor(cccd);
ess.add_characteristic(temp);
server.add_service(ess);

// A Battery Service (0x180F) too, so the viewer shows a second, differently
// decoded characteristic: Battery Level (0x2A19) is a single byte percentage.
// Read-only here (no notifications) — a plain, static reading.
let bas = android::BluetoothGattService(
    uuid::BATTERY_SERVICE,
    android::SERVICE_TYPE_PRIMARY,
);
let battery = android::BluetoothGattCharacteristic(
    uuid::BATTERY_LEVEL,
    android::PROPERTY_READ,
    android::PERMISSION_READ,
);
battery.set_value([87]);
bas.add_characteristic(battery);
server.add_service(bas);

// The temperature drifts gently between ~20.0 and ~23.0 °C.
fn tick(server, t) {
    let centi = (2150.0 + 150.0 * sin(t / 6.0)).to_int();
    server.update_value(uuid::of("2A6E"), [centi & 0xFF, (centi >> 8) & 0xFF]);
}
