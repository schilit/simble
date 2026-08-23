// One earbud of a stereo pair — a CSIP coordinated set member.
// Android reaches this through `BluetoothCsipSetCoordinator`.
//
// The point of CSIP is that a phone can tell "this is the other half of the
// pair I already have" from "this is somebody else's earbud", without the
// two earbuds talking to each other. Both members are provisioned with the
// same Set Identity Resolving Key. Each advertises a Resolvable Set
// Identifier: three random bytes plus three bytes of AES-CMAC over them,
// keyed by the SIRK. A coordinator recomputes the hash with each SIRK it
// holds; a match means "member of that set".
//
// That is the part a script cannot build. Rhai has no AES, so before this
// binding the crypto in `profiles::csip` had exactly one caller and it was a
// test — correct, and unreachable from any device.
let server = android::BluetoothGattServer("Earbud L");

// The set: 16-byte SIRK, 2 members, this one is rank 1 (the left).
// Rank orders the set; a coordinator locks members in rank order so two
// clients cannot grab the same pair from opposite ends and deadlock.
let sirk = [0x83, 0x8E, 0x68, 0x05, 0x53, 0xF1, 0x41, 0x5A,
            0xA2, 0x65, 0xBB, 0xAF, 0xC6, 0xEA, 0x03, 0xB8];
server.add_csis(sirk, 2, 1);

// prand: the random half of the RSI. A real member draws fresh bytes each
// time it starts advertising, so the identifier is not a stable tracker.
server.advertise_set_identity(sirk, [0x69, 0xF5, 0x63]);

// A real earbud is also an LE Audio sink with a volume control.
server.add_pacs(0x01, 0x00); // sink location: front left
server.add_ascs([0x01], []); // one sink ASE
server.add_vcs(128, 0);

server.advertise_service_uuid(0x1846); // Coordinated Set Identification
server.advertise_service_uuid(0x1844); // Volume Control
server.advertise_service_uuid(0x184E); // Audio Stream Control
