use super::*;
use crate::device::classic_device::{ClassicPhase, SERIAL_PORT_SERVICE_CLASS};
use crate::device::classic_host::{self, spp_service_record};

/// The RFCOMM server channel the acceptor advertises and serves.
const SPP_CHANNEL: u8 = 3;

fn addr(s: &str) -> Address {
    s.parse().unwrap()
}

/// Run the scene until `done` or `limit` ticks have passed. Returns the
/// number of ticks used, so a test can assert progress rather than merely
/// eventual success.
fn run_until(
    scene: &mut SceneEngine,
    limit: usize,
    mut done: impl FnMut(&SceneEngine) -> bool,
) -> usize {
    for tick in 0..limit {
        if done(scene) {
            return tick;
        }
        scene.tick(tick as f64 * 0.01);
    }
    limit
}

/// Two `ClassicHost`s in one scene, with nothing but the simulated
/// controller between them: A finds B by inquiry, learns its name, pages
/// it, opens L2CAP, queries SDP, opens the RFCOMM channel that SDP
/// advertised, exchanges data, and disconnects.
///
/// This is the whole point of the BR/EDR work in `controller::sim`: none
/// of it was reachable before, because a `ClassicHost` had nothing to
/// talk to except netsim.
#[test]
fn test_two_classic_hosts_discover_connect_and_exchange_data() {
    let mut scene = SceneEngine::new();
    let acceptor_address = addr("AA:BB:CC:00:00:02");
    let acceptor = scene.add_classic_device(
        acceptor_address,
        ClassicDevice::acceptor("Simble SPP", [0x04, 0x04, 0x24], SPP_CHANNEL),
    );
    let initiator = scene.add_classic_device(
        addr("AA:BB:CC:00:00:01"),
        ClassicDevice::initiator(
            "Simble Phone",
            [0x04, 0x02, 0x5A],
            acceptor_address,
            b"hello serial".to_vec(),
        ),
    );

    let ticks = run_until(&mut scene, 200, |scene| {
        matches!(
            scene.classic_device(initiator).map(ClassicDevice::phase),
            Some(ClassicPhase::Done) | Some(ClassicPhase::Failed)
        )
    });

    let client = scene.classic_device(initiator).expect("classic device");
    assert_eq!(
        client.phase(),
        ClassicPhase::Done,
        "the whole BR/EDR sequence must complete (stopped after {ticks} \
             ticks): {:?}",
        client.error()
    );

    // Each stage, asserted for itself — a plan that reached `Done` by
    // skipping one would be a worse bug than one that never finished.
    assert_eq!(
        client.discovered().len(),
        1,
        "exactly the one discoverable device in the scene"
    );
    let found = &client.discovered()[0];
    assert_eq!(found.address, acceptor_address, "found by inquiry");
    assert_eq!(
        found.class_of_device,
        [0x04, 0x04, 0x24],
        "with the Class of Device the acceptor's host wrote"
    );
    assert_eq!(
        found.name.as_deref(),
        Some("Simble SPP"),
        "and its name, which only a Remote Name Request can supply — an \
             inquiry result carries no name"
    );
    assert_eq!(
        client.received(),
        [b"hello serial".to_vec()],
        "the acceptor's echoing serial port answered over RFCOMM, which \
             means SDP, L2CAP and the ACL underneath all worked"
    );

    // The acceptor saw a real link too, and it is gone again.
    let server = scene.classic_device(acceptor).expect("classic device");
    assert!(
        server.host().connection().is_none(),
        "the initiator disconnected, and the acceptor was told"
    );
}

/// The seam a profile above RFCOMM uses: both ends hold the link open and
/// talk through their ports for as long as they like, in both directions.
///
/// The plan in `initiator`/`acceptor` is an errand — say one thing, read
/// the echo, hang up — and its two habits are silently fatal to anything
/// else: it tears the ACL down after the first payload, and it *drains
/// the port itself*, swallowing the bytes the profile above is waiting
/// for. Both are covered here, because both look like a working link
/// right up to the point where nothing arrives.
#[test]
fn test_a_held_open_link_carries_a_conversation_in_both_directions() {
    let mut scene = SceneEngine::new();
    let server_address = addr("AA:BB:CC:00:00:02");
    let (server_device, server_port) = ClassicDevice::serving(
        "Simble Service",
        [0x04, 0x04, 0x24],
        SPP_CHANNEL,
        vec![(
            0x00010001,
            spp_service_record(0x00010001, SPP_CHANNEL, "Simble Service"),
        )],
    );
    let server = scene.add_classic_device(server_address, server_device);
    let (client_device, client_port) = ClassicDevice::client(
        "Simble Client",
        [0x04, 0x02, 0x5A],
        server_address,
        SERIAL_PORT_SERVICE_CLASS,
    );
    let client = scene.add_classic_device(addr("AA:BB:CC:00:00:01"), client_device);

    // Bring the link up and wait for the data link, not for a payload.
    let ticks = run_until(&mut scene, 200, |_| {
        client_port.lock().is_ok_and(|p| p.is_open())
    });
    assert!(
        client_port.lock().unwrap().is_open(),
        "the data link never opened after {ticks} ticks: {:?}",
        scene.classic_device(client).and_then(ClassicDevice::error)
    );

    // Three exchanges, alternating who speaks first — a conversation, not
    // an errand.
    for round in 0..3u8 {
        client_port.lock().unwrap().write(vec![round, 0xC1]);
        run_until(&mut scene, 40, |_| {
            server_port
                .lock()
                .is_ok_and(|p| p.received_count() > usize::from(round))
        });
        assert_eq!(
            server_port.lock().unwrap().take_received(),
            [vec![round, 0xC1]],
            "round {round}: the server's port, not the plan, must get the bytes"
        );
        server_port.lock().unwrap().write(vec![round, 0x5E]);
        run_until(&mut scene, 40, |_| {
            client_port
                .lock()
                .is_ok_and(|p| p.received_count() > usize::from(round))
        });
        assert_eq!(
            client_port.lock().unwrap().take_received(),
            [vec![round, 0x5E]],
            "round {round}: and the answer must come back"
        );
    }

    // The link is still up: nothing tore it down after the first payload.
    assert_eq!(
        scene.classic_device(client).map(ClassicDevice::phase),
        Some(ClassicPhase::Exchanging),
        "a held-open client stays in Exchanging rather than disconnecting"
    );
    assert!(
        scene
            .classic_device(server)
            .and_then(|d| d.host().connection())
            .is_some(),
        "and the server still has the ACL"
    );

    // The credit window is visible from the device end, which is the only
    // place a page or a test can see RFCOMM flow control at all.
    let window = client_port
        .lock()
        .unwrap()
        .window()
        .expect("the port reports its DLC");
    assert_eq!(window.dlci, SPP_CHANNEL << 1);
    assert!(window.tx_credits > 0, "the client may still write");
}

/// The negative half, and the reason Scan Enable is modelled at all: a
/// device that never made itself discoverable must not be found. A
/// simulator that connected regardless would hide the single most common
/// BR/EDR bring-up bug there is.
#[test]
fn test_a_device_that_is_not_discoverable_is_not_found() {
    let mut scene = SceneEngine::new();
    let hidden_address = addr("AA:BB:CC:00:00:02");
    scene.add_classic_device(
        hidden_address,
        ClassicDevice::acceptor("Invisible", [0x04, 0x04, 0x24], SPP_CHANNEL)
            .with_scan_enable(classic_host::scan_enable::NONE),
    );
    let initiator = scene.add_classic_device(
        addr("AA:BB:CC:00:00:01"),
        ClassicDevice::initiator(
            "Simble Phone",
            [0x04, 0x02, 0x5A],
            hidden_address,
            b"hello".to_vec(),
        ),
    );

    run_until(&mut scene, 200, |scene| {
        matches!(
            scene.classic_device(initiator).map(ClassicDevice::phase),
            Some(ClassicPhase::Done) | Some(ClassicPhase::Failed)
        )
    });

    let client = scene.classic_device(initiator).expect("classic device");
    assert!(
        client.discovered().is_empty(),
        "a device with Scan Enable 0x00 answers no inquiry: {:?}",
        client.discovered()
    );
    assert_eq!(
        client.phase(),
        ClassicPhase::Failed,
        "and the client must give up rather than hang"
    );
    assert!(
        client.error().is_some_and(|e| e.contains("inquiry")),
        "with a reason that names the stage it failed at: {:?}",
        client.error()
    );
}

/// A device that is discoverable but not connectable is found by an
/// inquiry and then refuses to say what it is — which is exactly what an
/// "unknown device" entry in a phone's Bluetooth list means, since a
/// Remote Name Request pages the device.
#[test]
fn test_a_discoverable_but_unconnectable_device_is_found_but_not_named() {
    let mut scene = SceneEngine::new();
    let shy_address = addr("AA:BB:CC:00:00:02");
    scene.add_classic_device(
        shy_address,
        ClassicDevice::acceptor("Shy", [0x04, 0x04, 0x24], SPP_CHANNEL)
            .with_scan_enable(classic_host::scan_enable::INQUIRY_ONLY),
    );
    let initiator = scene.add_classic_device(
        addr("AA:BB:CC:00:00:01"),
        ClassicDevice::initiator(
            "Simble Phone",
            [0x04, 0x02, 0x5A],
            shy_address,
            b"hello".to_vec(),
        ),
    );

    run_until(&mut scene, 60, |_| false);

    let client = scene.classic_device(initiator).expect("classic device");
    assert_eq!(
        client.discovered().len(),
        1,
        "inquiry scan alone is enough to be found"
    );
    assert_eq!(
        client.discovered()[0].name,
        None,
        "but page scan is what it takes to be named"
    );
    assert_eq!(
        client.phase(),
        ClassicPhase::ResolvingNames,
        "the client is still waiting on a name it will never get — and it \
             is *visibly* stuck at that stage rather than silently elsewhere"
    );
}

/// A scene can hold LE and BR/EDR devices at once. They share the
/// simulated room and nothing else, so neither shows up in the other's
/// discovery.
#[test]
fn test_classic_and_le_devices_coexist_without_seeing_each_other() {
    let mut scene = SceneEngine::new();
    let acceptor_address = addr("AA:BB:CC:00:00:02");
    scene.add_classic_device(
        acceptor_address,
        ClassicDevice::acceptor("Simble SPP", [0x04, 0x04, 0x24], SPP_CHANNEL),
    );
    let initiator = scene.add_classic_device(
        addr("AA:BB:CC:00:00:01"),
        ClassicDevice::initiator(
            "Simble Phone",
            [0x04, 0x02, 0x5A],
            acceptor_address,
            b"ping".to_vec(),
        ),
    );
    let scanner = scene.add_scanner(addr("AA:BB:CC:00:00:03"));

    run_until(&mut scene, 200, |scene| {
        matches!(
            scene.classic_device(initiator).map(ClassicDevice::phase),
            Some(ClassicPhase::Done) | Some(ClassicPhase::Failed)
        )
    });

    assert_eq!(
        scene.classic_device(initiator).map(ClassicDevice::phase),
        Some(ClassicPhase::Done),
        "the classic pair is unaffected by an LE scanner in the room"
    );
    assert!(
        scene.scanner_reports_json(scanner) == "[]",
        "and an LE scanner sees no BR/EDR device: inquiry and advertising \
             are different radios doing different things"
    );
}
