// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Two [`ClassicHost`]s meeting over the simulated BR/EDR controller and
//! pairing: Secure Simple Pairing end to end, a link key stored at both ends,
//! authentication, encryption, data over the encrypted link, and a reconnect
//! that reuses the bond.
//!
//! `src/controller/sim.rs`'s own tests check the controller one command at a
//! time — the right event type, the right completion event, in order. This
//! checks the *pair*: a real host answering a real controller's questions,
//! which is the only place a policy mistake in `ClassicHost` (an unanswered
//! Link Key Request, a reply naming the wrong address) shows up at all. A
//! controller that asks a question nobody answers does not fail; it waits.
//!
//! The one thing two simble endpoints cannot prove is that a *foreign* stack
//! accepts the same conversation. That is `tests/interop/classic_peer.py`,
//! against Bumble over netsim.

use simble::classic::sdp::{SDP_PSM, SdpServer, SdpUuid};
use simble::controller::sim::Link;
use simble::device::classic_host::{
    authentication_requirements, io_capability, spp_service_record,
};
use simble::device::{ClassicHost, LinkKey, SdpHandler, SdpQueryHandler, SharedSdpQueryResults};
use simble::transport::HciChannel;
use simble::types::Address;
use std::sync::Arc;

mod common;
use common::{Scene, address};

/// The acceptor's public address.
fn acceptor_address() -> Address {
    address(0x02)
}

/// The initiator's public address.
fn initiator_address() -> Address {
    address(0x01)
}

/// Serial Port Profile service class — what the acceptor's record advertises
/// and what the initiator's SDP query searches for.
const SERIAL_PORT: SdpUuid = SdpUuid::Uuid16(0x1101);
/// The RFCOMM channel the acceptor's SDP record names. Never guessed by the
/// initiator: it reads it back out of the answer, which is what makes the
/// query a real exchange rather than a formality.
const RFCOMM_CHANNEL: u8 = 7;

/// One device in the scene: a host and the controller channel it drives.
struct Node {
    host: ClassicHost,
    channel: Arc<HciChannel>,
    started: bool,
}

impl Node {
    fn new(link: &mut Link, address: Address, host: ClassicHost) -> Self {
        Self {
            channel: link.add_device(address),
            host,
            started: false,
        }
    }

    /// Queues bring-up on the first call: reset, name, Class of Device,
    /// **Write Simple Pairing Mode**, scan enable. Without the third of those
    /// this controller answers Authentication Requested with Pairing Not
    /// Allowed rather than running SSP the host never switched on.
    fn start(&mut self) {
        if self.started {
            return;
        }
        self.started = true;
        for packet in self.host.start_commands() {
            self.channel.inject_host_packet(packet).unwrap();
        }
    }

    /// Hands every packet the controller produced to the host and sends back
    /// whatever it answers with. This is the loop that answers the four SSP
    /// questions; nothing else in the test does.
    fn pump(&mut self) {
        while let Some(packet) = self.channel.poll_controller_packet() {
            for out in self.host.handle_packet(&packet).expect("the host parses") {
                self.channel.inject_host_packet(out).unwrap();
            }
        }
        for out in self.host.poll() {
            self.channel.inject_host_packet(out).unwrap();
        }
    }
}

/// An acceptor: discoverable, connectable, serving one SPP record.
fn acceptor(name: &str) -> ClassicHost {
    let mut host = ClassicHost::new(name, [0x04, 0x04, 0x24]);
    let mut sdp = SdpHandler::new(SdpServer::new());
    sdp.server_mut().service_records.insert(
        0x0001_0001,
        spp_service_record(0x0001_0001, RFCOMM_CHANNEL, name),
    );
    host.register_handler(Box::new(sdp)).unwrap();
    host
}

/// An initiator, with an SDP client searching for the Serial Port class.
fn initiator(name: &str) -> (ClassicHost, SharedSdpQueryResults) {
    let mut host = ClassicHost::new(name, [0x0C, 0x02, 0x5A]);
    let (query, results) = SdpQueryHandler::searching(SERIAL_PORT);
    host.register_handler(Box::new(query)).unwrap();
    (host, results)
}

/// The initiator, the acceptor, and the medium between them.
struct SecurityScene {
    link: Link,
    client: Node,
    server: Node,
}

impl Scene for SecurityScene {
    /// Both hosts queue their bring-up (once each), the medium routes, then
    /// both answer whatever came back. The order is load-bearing: a host that
    /// has not sent its bring-up has nothing to answer with, and a host pumped
    /// before `link.tick` would be answering the previous step's packets.
    fn tick(&mut self) {
        self.client.start();
        self.server.start();
        self.link.tick();
        self.client.pump();
        self.server.pump();
    }
}

/// Pages, pairs and encrypts. Returns the scene and the initiator's SDP
/// results, ready for the assertions each test cares about.
fn paired_scene() -> (SecurityScene, SharedSdpQueryResults) {
    let mut link = Link::new();
    let server = Node::new(&mut link, acceptor_address(), acceptor("Speaker"));
    let (host, results) = initiator("Phone");
    let client = Node::new(&mut link, initiator_address(), host);
    let mut scene = SecurityScene {
        link,
        client,
        server,
    };

    // Both claim a screen and a button and both ask for MITM protection, so
    // the controllers select Numeric Comparison and the key that comes out is
    // an authenticated one.
    for node in [&mut scene.client, &mut scene.server] {
        node.host.set_io_capability(
            io_capability::DISPLAY_YES_NO,
            authentication_requirements::GENERAL_BONDING_MITM,
        );
    }

    // Bring-up first: the acceptor has to be connectable before the page, and
    // both have to have sent Write Simple Pairing Mode. A page that goes out
    // before Write Scan Enable lands is a Page Timeout, not a failure worth
    // debugging.
    let _ = scene.run_until(8, |_| false);

    for packet in scene.client.host.create_connection(acceptor_address()) {
        scene.client.channel.inject_host_packet(packet).unwrap();
    }
    assert!(
        scene.run_until(16, |s| s.client.host.connection().is_some()),
        "the page never completed"
    );
    (scene, results)
}

/// Runs Authentication Requested and waits for the whole SSP conversation.
fn authenticate(scene: &mut SecurityScene) -> bool {
    for packet in scene.client.host.authenticate() {
        scene.client.channel.inject_host_packet(packet).unwrap();
    }
    scene.run_until(24, |s| {
        s.client.host.security().authenticated && s.server.host.security().authenticated
    })
}

/// Runs Set Connection Encryption and waits for both Encryption Changes.
fn encrypt(scene: &mut SecurityScene) -> bool {
    for packet in scene.client.host.encrypt(true) {
        scene.client.channel.inject_host_packet(packet).unwrap();
    }
    scene.run_until(16, |s| {
        s.client.host.security().encrypted && s.server.host.security().encrypted
    })
}

#[test]
fn test_two_classic_hosts_pair_authenticate_encrypt_and_exchange_data() {
    let (mut scene, results) = paired_scene();

    assert!(
        authenticate(&mut scene),
        "pairing never completed: initiator {:?}, acceptor {:?}",
        scene.client.host.security(),
        scene.server.host.security()
    );

    // Both sides ran a real pairing, not a bonded shortcut.
    assert_eq!(
        scene.client.host.security().pairing_status,
        Some(0x00),
        "the initiator saw Simple Pairing Complete"
    );
    assert_eq!(scene.server.host.security().pairing_status, Some(0x00));
    // And both were shown the same six digits, which is the entire content of
    // Numeric Comparison.
    assert!(scene.client.host.security().numeric_value.is_some());
    assert_eq!(
        scene.client.host.security().numeric_value,
        scene.server.host.security().numeric_value,
        "two devices shown different numbers cannot be compared by a person"
    );
    assert_eq!(
        scene.client.host.security().peer_io_capability,
        Some(io_capability::DISPLAY_YES_NO),
        "the IO Capability Response is how each side learns what the other is"
    );

    // The bond: the same sixteen octets at both ends, and authenticated,
    // because a person was in the loop.
    let client_key = scene
        .client
        .host
        .link_key(acceptor_address())
        .expect("the initiator stored a key");
    let server_key = scene
        .server
        .host
        .link_key(initiator_address())
        .expect("the acceptor stored a key");
    assert_eq!(
        client_key.value, server_key.value,
        "a bond whose two halves differ is not a bond"
    );
    assert!(
        client_key.is_authenticated(),
        "Numeric Comparison with MITM asked for makes an authenticated key, \
         got key type {:#04X}",
        client_key.key_type
    );

    assert!(
        encrypt(&mut scene),
        "encryption never started: initiator {:?}, acceptor {:?}",
        scene.client.host.security(),
        scene.server.host.security()
    );

    // Data over the encrypted link. The SDP query is a real round trip: the
    // answer is the acceptor's record, and the channel number in it is one
    // only the acceptor knows.
    scene
        .client
        .host
        .open_channel(SDP_PSM)
        .expect("the SDP channel opens on a live ACL")
        .into_iter()
        .for_each(|packet| scene.client.channel.inject_host_packet(packet).unwrap());
    assert!(
        scene.run_until(32, |_| {
            results.lock().map(|r| r.answered).unwrap_or(false)
        }),
        "the SDP query was never answered over the encrypted link"
    );
    let answer = results.lock().unwrap();
    assert_eq!(
        answer.error, None,
        "the peer's SDP server refused the query"
    );
    assert_eq!(
        answer.channel_for(SERIAL_PORT),
        Some(RFCOMM_CHANNEL),
        "the answer must carry the acceptor's own channel, not a guess"
    );

    // And the link stayed encrypted while it carried it.
    assert!(scene.client.host.security().encrypted);
    assert!(scene.server.host.security().encrypted);
}

#[test]
fn test_a_reconnect_with_a_stored_link_key_does_not_pair_again() {
    // The observable difference a bond makes, from the host's side: same
    // command, same link, and no Simple Pairing Complete at all — because no
    // pairing ran.
    let (mut scene, _) = paired_scene();
    assert!(authenticate(&mut scene));
    assert!(encrypt(&mut scene));

    let bonded_key = scene.client.host.link_key(acceptor_address()).unwrap();

    // Tear the link down. The keys survive it; that is what makes them a bond
    // rather than a session.
    for packet in scene.client.host.disconnect() {
        scene.client.channel.inject_host_packet(packet).unwrap();
    }
    assert!(
        scene.run_until(16, |s| s.client.host.connection().is_none()
            && s.server.host.connection().is_none()),
        "the link never came down"
    );
    assert_eq!(
        scene.client.host.link_key(acceptor_address()),
        Some(bonded_key),
        "a disconnect must not forget the bond"
    );
    assert_eq!(
        scene.client.host.security().pairing_status,
        None,
        "and it must forget the *link's* security, which died with the link"
    );

    // Reconnect and authenticate again.
    for packet in scene.client.host.create_connection(acceptor_address()) {
        scene.client.channel.inject_host_packet(packet).unwrap();
    }
    assert!(
        scene.run_until(24, |s| s.client.host.connection().is_some()),
        "the reconnect never completed"
    );

    for packet in scene.client.host.authenticate() {
        scene.client.channel.inject_host_packet(packet).unwrap();
    }
    assert!(
        scene.run_until(24, |s| s.client.host.security().authenticated),
        "the bonded link never authenticated: {:?}",
        scene.client.host.security()
    );

    assert_eq!(
        scene.client.host.security().pairing_status,
        None,
        "a bonded reconnect runs no pairing, so there is no Simple Pairing \
         Complete to report: {:?}",
        scene.client.host.security()
    );
    assert_eq!(
        scene.client.host.security().numeric_value,
        None,
        "and nobody is asked to compare anything"
    );
    assert_eq!(
        scene.client.host.link_key(acceptor_address()),
        Some(bonded_key),
        "the key is reused, not replaced"
    );

    // Encryption still works on the reused key, which is the point of keeping
    // it: a bond that could not encrypt would be a bond that saved nothing.
    assert!(encrypt(&mut scene));
}

#[test]
fn test_a_forgotten_bond_on_one_side_pairs_again() {
    // Half a bond is no bond. The acceptor forgets; the reconnect has to run
    // the whole pairing rather than authenticate against a key one side no
    // longer holds.
    let (mut scene, _) = paired_scene();
    assert!(authenticate(&mut scene));

    let first_key = scene.client.host.link_key(acceptor_address()).unwrap();
    assert!(scene.server.host.remove_link_key(initiator_address()));

    for packet in scene.client.host.disconnect() {
        scene.client.channel.inject_host_packet(packet).unwrap();
    }
    assert!(
        scene.run_until(16, |s| s.client.host.connection().is_none()),
        "the link never came down"
    );
    for packet in scene.client.host.create_connection(acceptor_address()) {
        scene.client.channel.inject_host_packet(packet).unwrap();
    }
    assert!(scene.run_until(24, |s| s.client.host.connection().is_some()));
    assert!(authenticate(&mut scene));

    assert_eq!(
        scene.client.host.security().pairing_status,
        Some(0x00),
        "the forgotten half forces a fresh pairing: {:?}",
        scene.client.host.security()
    );
    assert_eq!(
        scene
            .client
            .host
            .link_key(acceptor_address())
            .map(|k| k.value),
        scene
            .server
            .host
            .link_key(initiator_address())
            .map(|k| k.value),
        "and both ends end up holding the same new key"
    );
    // The derivation is stable, so the "new" key is the same one — which is
    // a property of this simulated controller, not of SSP, and is asserted so
    // that a change to `derived_link_key` shows up here rather than as a
    // mysterious pass.
    assert_eq!(
        scene.client.host.link_key(acceptor_address()),
        Some(first_key)
    );
}

#[test]
fn test_a_peer_that_refuses_confirmation_fails_and_the_link_is_not_half_encrypted() {
    let (mut scene, _) = paired_scene();
    scene.server.host.set_accept_pairing(false);

    for packet in scene.client.host.authenticate() {
        scene.client.channel.inject_host_packet(packet).unwrap();
    }
    // Wait for the failure to land rather than for a success that will not
    // come: the initiator's Simple Pairing Complete is the thing that arrives.
    assert!(
        scene.run_until(24, |s| s.client.host.security().pairing_status.is_some()),
        "the refusal never reached the initiator"
    );

    assert_eq!(
        scene.client.host.security().pairing_status,
        Some(0x05),
        "Authentication Failure is what a refused confirmation becomes"
    );
    assert!(
        !scene.client.host.security().authenticated,
        "a refused pairing must not leave the link authenticated"
    );
    assert!(!scene.server.host.security().authenticated);
    assert_eq!(
        scene.client.host.link_key(acceptor_address()),
        None,
        "and it must not leave a key behind"
    );

    // Now try to encrypt anyway. This is the half-encrypted case: it has to
    // fail at the asking end and change nothing at the peer.
    for packet in scene.client.host.encrypt(true) {
        scene.client.channel.inject_host_packet(packet).unwrap();
    }
    let _ = scene.run_until(16, |_| false);
    assert!(
        !scene.client.host.security().encrypted,
        "the initiator must not believe an unkeyed link is encrypted"
    );
    assert!(
        !scene.server.host.security().encrypted,
        "and neither must the peer — a link encrypted at one end only is the \
         worst of the three possible states"
    );

    // The ACL itself is still up and still usable: a failed pairing is not a
    // failed connection, and an SDP query over an unencrypted link is exactly
    // what a phone does before it offers to pair.
    assert!(scene.client.host.connection().is_some());
    assert!(scene.server.host.connection().is_some());
}

#[test]
fn test_a_link_key_planted_at_both_ends_authenticates_with_no_pairing() {
    // The bond a device restores from storage at boot. Nothing here has ever
    // paired, and the very first Authentication Requested still succeeds.
    let (mut scene, _) = paired_scene();
    let key = LinkKey {
        value: [0x5A; 16],
        key_type: 0x05,
    };
    scene.client.host.insert_link_key(acceptor_address(), key);
    scene.server.host.insert_link_key(initiator_address(), key);

    for packet in scene.client.host.authenticate() {
        scene.client.channel.inject_host_packet(packet).unwrap();
    }
    // Only the initiator is waited on. On the bonded path the *acceptor* is
    // told nothing at all — no Authentication Complete (that goes to the host
    // that asked), no Simple Pairing Complete (no pairing ran), and no Link
    // Key Notification (no new key). Its only signal that anything happened
    // is the Encryption Change below, which is why a profile that needs a
    // secure link should require encryption rather than authentication.
    assert!(
        scene.run_until(24, |s| s.client.host.security().authenticated),
        "a restored bond has to authenticate: {:?}",
        scene.client.host.security()
    );
    assert_eq!(
        scene.client.host.security().pairing_status,
        None,
        "and it has to do it without pairing"
    );
    assert_eq!(
        scene.client.host.link_key(acceptor_address()),
        Some(key),
        "the restored key is used as-is, not replaced"
    );
    assert!(encrypt(&mut scene));
}
