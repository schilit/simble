// SimBLE Speaker — LE Audio Volume Control Service.
// The control-point idiom: a peer WRITES a command opcode, and the device
// applies it, updates its state and bumps the change counter. Nothing writes
// the volume directly.
let server = android::BluetoothGattServer("web-speaker");

// A real LE Audio sink: PACS declares what this device can decode and
// ASCS carries the endpoint a peer configures. Without them a phone (or
// Bumble) has nothing to set a stream up against, however good the
// volume control is.
server.add_pacs(0x03, 0x00);   // sink locations: front left + front right
server.add_ascs([0x01], []);   // one sink ASE
server.advertise_service_uuid(0x1850);
server.advertise_service_uuid(0x184E);
// The targeted announcement LeAudioService scans for:
// [type, available contexts (4 octets), metadata length]
server.advertise_service_data(0x184E, [0x01, 0x06, 0x00, 0x00, 0x00, 0x00]);

let vcs = android::BluetoothGattService(uuid::VOLUME_CONTROL_SERVICE, android::SERVICE_TYPE_PRIMARY);

let state = android::BluetoothGattCharacteristic(uuid::VOLUME_STATE,
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
state.set_value([128, 0, 0]); // [volume 0-255, muted, change counter]
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
vcs.add_characteristic(flags);
server.add_service(vcs);

// Opcodes: 0x00 down, 0x01 up, 0x02/0x03 unmute+down/up, 0x04 set absolute,
// 0x05 unmute, 0x06 mute. A write is [opcode, change_counter] (+ volume for 0x04).
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
    server.update_value(uuid::VOLUME_STATE, [volume, muted, (state[2] + 1) % 256]);
    server.update_value(uuid::VOLUME_CONTROL_POINT, [0xFF]); // consumed
}
