// SimBLE Speaker — LE Audio Volume Control Service.
// The control-point idiom: a peer WRITES a command opcode, and the device
// applies it, updates its state and bumps the change counter. Nothing writes
// the volume directly. Android reaches this through `BluetoothVolumeControl`.
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

// The volume half, from `profiles::vcp` rather than hand-built: a peer's
// Volume Control Point write drives a real state machine, so a stale change
// counter is rejected with ATT error 0x80 instead of being applied. The
// hand-rolled version this replaces polled the control point in `tick` and
// could not reject anything.
server.add_vcs(128, 0); // [volume 0-255, mute]
