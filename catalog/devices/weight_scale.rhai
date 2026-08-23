// A smart scale exposes two services: Weight Scale for the raw mass
// and Body Composition for the derived numbers. Both measurements are
// INDICATED rather than notified — a weigh-in is a record the phone must
// acknowledge, not a stream it can miss.
let server = android::BluetoothGattServer("SmartScale");

let wss = android::BluetoothGattService(uuid::from_u16(0x181D), android::SERVICE_TYPE_PRIMARY);
// Flags 0x00 = SI units; weight is uint16 in 5 g steps, so 74.5 kg = 14900.
let weight = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A9D),
    android::PROPERTY_INDICATE, android::PERMISSION_READ);
weight.set_value([0x00, 0x34, 0x3A]);
weight.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
wss.add_characteristic(weight);

let wss_feature = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A9E),
    android::PROPERTY_READ, android::PERMISSION_READ);
wss_feature.set_value([0x00, 0x00, 0x00, 0x0C]); // 5 g mass resolution
wss.add_characteristic(wss_feature);
server.add_service(wss);

let bcs = android::BluetoothGattService(uuid::from_u16(0x181B), android::SERVICE_TYPE_PRIMARY);
// Flags 0x00 (SI) + body fat percentage in 0.1% steps: 182 = 18.2%.
let composition = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A9C),
    android::PROPERTY_INDICATE, android::PERMISSION_READ);
composition.set_value([0x00, 0x00, 0xB6, 0x00]);
composition.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
bcs.add_characteristic(composition);

let bcs_feature = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A9B),
    android::PROPERTY_READ, android::PERMISSION_READ);
bcs_feature.set_value([0x02, 0x00, 0x00, 0x00]); // body fat supported
bcs.add_characteristic(bcs_feature);
server.add_service(bcs);

// A step-on wobble that settles: real scales average a noisy load cell.
fn tick(server, t) {
    let wobble = (t * 3.0).to_int() % 7;
    let grams = 14900 + wobble * 5;
    let lo = grams & 0xFF;
    let hi = (grams >> 8) & 0xFF;
    server.update_value(uuid::from_u16(0x2A9D), [0x00, lo, hi]);
}
