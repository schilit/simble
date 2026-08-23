// Volume Control Service — the LE Audio profile a phone uses to set a
// speaker's volume. This is the control-point idiom: the peer WRITES a
// command opcode, and the device applies it and notifies the new state.
let server = android::BluetoothGattServer("Speaker");
let vcs = android::BluetoothGattService(uuid::VOLUME_CONTROL_SERVICE, android::SERVICE_TYPE_PRIMARY);

let state = android::BluetoothGattCharacteristic(uuid::VOLUME_STATE,
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
state.set_value([128, 0, 0]); // [volume 0-255, muted, change counter]
// A characteristic that declares NOTIFY needs a CCCD, or no real central
// can subscribe to it (Core Spec Vol 3, Part G, Section 3.3.3.3).
state.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
vcs.add_characteristic(state);

let point = android::BluetoothGattCharacteristic(uuid::VOLUME_CONTROL_POINT,
    android::PROPERTY_WRITE, android::PERMISSION_WRITE);
point.set_value([0xFF]); // 0xFF = no command pending
vcs.add_characteristic(point);

let flags = android::BluetoothGattCharacteristic(uuid::VOLUME_FLAGS,
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
flags.set_value([0x01]); // volume setting persisted
flags.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
vcs.add_characteristic(flags);
server.add_service(vcs);

// Opcodes (Volume Control Service 1.0, Table 3.3): 0x00 down, 0x01 up,
// 0x02/0x03 unmute+down/up, 0x04 set absolute, 0x05 unmute, 0x06 mute.
// A write is [opcode, change_counter] (+ volume for 0x04).
fn tick(server, t) {
    let command = server.value(uuid::VOLUME_CONTROL_POINT);
    if command.len() < 1 || command[0] == 0xFF { return; }
    let state = server.value(uuid::VOLUME_STATE);
    let volume = state[0];
    let muted = state[1];
    let op = command[0];
    if op == 0x00 || op == 0x02 { volume = if volume > 16 { volume - 16 } else { 0 }; }
    if op == 0x01 || op == 0x03 { volume = if volume < 239 { volume + 16 } else { 255 }; }
    if op == 0x02 || op == 0x03 || op == 0x05 { muted = 0; }
    if op == 0x04 && command.len() > 2 { volume = command[2]; }
    if op == 0x06 { muted = 1; }
    // The change counter increments on every state change, so a peer can
    // detect a command it raced against.
    server.update_value(uuid::VOLUME_STATE, [volume, muted, (state[2] + 1) % 256]);
    server.update_value(uuid::VOLUME_CONTROL_POINT, [0xFF]); // consumed
}
