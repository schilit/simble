// The other earbud of the stereo pair — the rank-2 member of the same
// coordinated set as `earbud`.
//
// A set of one is not a set. `earbud` alone proves the crypto reaches the
// air; it takes two members sharing a SIRK to show what the profile is for:
// a phone that has bonded with one of them recognises the second as "the
// other half of the pair I already have" the first time it hears it, without
// the two earbuds ever talking to each other.
//
// Run both in one scene and scan. Two advertisements, two different
// Resolvable Set Identifiers — a fresh prand per member, so nothing links
// them by inspection — and both resolve against the one SIRK.
let server = android::BluetoothGattServer("Earbud R");

// The same 16-byte SIRK as `earbud`. This is the whole mechanism: shared
// key, per-member identifier.
let sirk = [0x83, 0x8E, 0x68, 0x05, 0x53, 0xF1, 0x41, 0x5A,
            0xA2, 0x65, 0xBB, 0xAF, 0xC6, 0xEA, 0x03, 0xB8];

// Set size 2 again, but rank 2 — the right earbud. Ranks are 1-based and
// unique within the set; a coordinator locks members in rank order so two
// clients cannot grab the same pair from opposite ends and deadlock.
server.add_csis(sirk, 2, 2);

// A different prand from the left earbud's, which is what makes the two
// advertisements look unrelated to anyone without the SIRK. A real member
// draws fresh bytes each time it starts advertising.
server.advertise_set_identity(sirk, [0x2B, 0x7E, 0x15]);

// The mirror of the left earbud: an LE Audio sink with a volume control.
server.add_pacs(0x02, 0x00); // sink location: front right
server.add_ascs([0x01], []); // one sink ASE
server.add_vcs(128, 0);

server.advertise_service_uuid(0x1846); // Coordinated Set Identification
server.advertise_service_uuid(0x1844); // Volume Control
server.advertise_service_uuid(0x184E); // Audio Stream Control
