// A phone publishing its media player over MCP, so an LE Audio headset's
// play/pause button reaches the music.
//
// NOTE ON NAMING: Android has no Bluetooth profile proxy for this. An app
// controls media through `MediaSession`/`MediaController`, and the Bluetooth
// stack bridges that to MCS behind the scenes — there is no
// `BluetoothMediaControl` class. So these bindings are named for the
// Bluetooth service (`add_gmcs`, `add_mcs`) rather than borrowing an Android
// name that does not exist.
let server = android::BluetoothGattServer("Phone");

// GMCS is the device-wide player: a headset finds it without knowing which
// app is playing. Its Content Control ID (1 here) is what a Call/Media
// Control profile uses to say "this stream belongs to that player".
server.add_gmcs("SimBLE Player", 1);

// A per-app player, alongside the generic one. A real phone registers one
// MCS instance per media app, each with its own CCID.
server.add_mcs("Podcasts", 2);

server.advertise_service_uuid(0x1849); // Generic Media Control Service
server.advertise_service_uuid(0x1848); // Media Control Service
