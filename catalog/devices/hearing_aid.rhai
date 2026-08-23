// A hearing aid exposing HAP presets — Android's `BluetoothHapClient`.
//
// A preset is a named listening program ("Restaurant", "Music"). The client
// pages the list with Read Presets Request and switches with Set Active
// Preset, all over one control point that INDICATES its responses rather
// than notifying them — a preset list is longer than one MTU, so the server
// sends the records one at a time and waits for each confirmation.
//
// That handshake is the part a script cannot build: `tick` can write bytes
// into a characteristic, but it cannot hold a read cursor across an
// indication and resume when the confirmation arrives. The state machine is
// in `profiles::hap`.
let server = android::BluetoothGattServer("Hearing Aid L");

// Features byte (HAS Section 3.1): bits 0-1 the type, then
// preset-synchronization (bit 2), independent presets (3), dynamic (4),
// writable (5). 0b10_0010 = binaural, dynamic presets, writable.
server.add_has(0b10_0010, [
    #{ index: 1, name: "Universal", writable: true, available: true },
    #{ index: 2, name: "Restaurant", writable: true, available: true },
    #{ index: 3, name: "Music", writable: true, available: true },
    // Unavailable presets stay in the list but cannot be made active — a
    // program the fitting software defined and the wearer has not enabled.
    #{ index: 4, name: "Telecoil", writable: false, available: false },
]);

// Hearing aids come in pairs, so this one is also a coordinated set member.
let sirk = [0x2F, 0x62, 0xC8, 0xAE, 0x41, 0x67, 0x4B, 0x0D,
            0x9E, 0x3C, 0x55, 0xD1, 0x08, 0xFA, 0x77, 0x91];
server.add_csis(sirk, 2, 1);
server.advertise_set_identity(sirk, [0x11, 0x22, 0x33]);

server.add_vcs(100, 0);

server.advertise_service_uuid(0x1854); // Hearing Access Service
server.advertise_service_uuid(0x1846); // Coordinated Set Identification
server.advertise_service_uuid(0x1844); // Volume Control
