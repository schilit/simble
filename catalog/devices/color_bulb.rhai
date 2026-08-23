// SimBLE Color Bulb — a Magic-Blue-style RGB light.
// A writable [R, G, B] color characteristic on a custom 128-bit service. The
// page's color picker writes this value (host glue), and a connected central
// is notified. No tick() needed — the color is driven by writes, not by time.
let server = android::BluetoothGattServer("web-lightbulb");

let svc = android::BluetoothGattService(
    uuid::of("f0ff0001-1234-5678-90ab-cdef01234567"),
    android::SERVICE_TYPE_PRIMARY,
);
let color = android::BluetoothGattCharacteristic(
    uuid::of("f0ff0002-1234-5678-90ab-cdef01234567"),
    android::PROPERTY_READ | android::PROPERTY_WRITE
        | android::PROPERTY_WRITE_NO_RESPONSE | android::PROPERTY_NOTIFY,
    android::PERMISSION_READ | android::PERMISSION_WRITE,
);
color.set_value([0x33, 0xCC, 0xFF]); // a cool cyan to start
let cccd = android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE,
);
color.add_descriptor(cccd);
svc.add_characteristic(color);
server.add_service(svc);
