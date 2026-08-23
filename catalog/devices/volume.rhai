// Volume Control — the LE Audio profile a phone uses to set a speaker's
// volume. Android reaches all of this through one proxy,
// `BluetoothVolumeControl`: setVolume on the device, setVolumeOffset on an
// output, and the gain on an input.
//
// The protocol is Rust (`profiles::vcp`, `vocs`, `aics`), so a peer's write
// to any of these control points drives a real state machine: the operand's
// change counter is checked, a stale one is rejected with ATT error 0x80
// Invalid Change Counter, and only a write that actually moves the state
// advances the counter and republishes it.
//
// This script used to hand-roll the VCS control point in `tick`: poll the
// control point, decode the opcode, write the state back. That version
// could not reject anything — Rhai has no way to fail an ATT write — so it
// accepted stale change counters a real speaker rejects, and it applied the
// command a tick late.
let server = android::BluetoothGattServer("Speaker");

// [volume 0-255, mute]. Relative up/down move by the server's step size.
server.add_vcs(128, 0);
// One output trim per audio location: 0x01 front left, 0x02 front right.
server.add_vocs(0x01, "Left");
server.add_vocs(0x02, "Right");
// One input, with the gain range this device actually supports.
server.add_aics(0, 255, "Line In");

server.advertise_service_uuid(0x1844); // Volume Control Service
