// A BLE smart lock. No SIG profile covers locks, so real products use
// a vendor service — the shape is always the control-point idiom: the
// phone WRITES a command, the lock applies it and notifies the new state.
// Nothing here trusts the writer; a real lock authenticates first (see
// the pairing/bonding path) before honouring a command.
let server = android::BluetoothGattServer("SmartLock");
let svc = android::BluetoothGattService(
    uuid::of("d3a70001-1f8a-4b2c-9a11-000000000001"), android::SERVICE_TYPE_PRIMARY);

// 0 = unlocked, 1 = locked, 2 = jammed.
let state = android::BluetoothGattCharacteristic(
    uuid::of("d3a70002-1f8a-4b2c-9a11-000000000001"),
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
state.set_value([0x01]);
state.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
svc.add_characteristic(state);

// Commands: 0x01 lock, 0x02 unlock. 0xFF means "no command pending".
let control = android::BluetoothGattCharacteristic(
    uuid::of("d3a70003-1f8a-4b2c-9a11-000000000001"),
    android::PROPERTY_WRITE, android::PERMISSION_WRITE);
control.set_value([0xFF]);
svc.add_characteristic(control);

// Battery, because every lock's real failure mode is a dead battery.
let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
let level = android::BluetoothGattCharacteristic(uuid::BATTERY_LEVEL,
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
level.set_value([72]);
bas.add_characteristic(level);
server.add_service(bas);
server.add_service(svc);

fn tick(server, t) {
    let command = server.value(uuid::of("d3a70003-1f8a-4b2c-9a11-000000000001"));
    if command.len() < 1 || command[0] == 0xFF { return; }
    let op = command[0];
    if op == 0x01 {
        server.update_value(uuid::of("d3a70002-1f8a-4b2c-9a11-000000000001"), [0x01]);
    }
    if op == 0x02 {
        server.update_value(uuid::of("d3a70002-1f8a-4b2c-9a11-000000000001"), [0x00]);
    }
    // Consume the command so the next write is seen as new.
    server.update_value(uuid::of("d3a70003-1f8a-4b2c-9a11-000000000001"), [0xFF]);
}
