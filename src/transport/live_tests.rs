// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Spec parsing for [`LiveTransport`]. Asserted through [`resolve`] rather
//! than [`LiveTransport::open`] on purpose: `open` dials, so an `open`-based
//! test would pass or fail on whether a `netsimd` happens to be running on
//! the machine — and on a developer box one often is.
//!
//! What matters here is that a spec selects the backend it names, that the
//! netsim URL is built with the reversed address netsim actually reads, and
//! that a typo is *refused* rather than silently falling back to netsim. The
//! last one is load-bearing: a fallback would turn a CI run that meant to
//! reach Bumble into a run that quietly reached nothing.

use super::*;

/// An address whose bytes are not a palindrome, so a reversed netsim wire
/// form is visibly different from the display form.
fn address() -> Address {
    "F0:DE:C0:00:0C:0B".parse().expect("valid address")
}

#[test]
fn test_unknown_spec_is_refused_rather_than_falling_back_to_netsim() {
    let error = resolve("bumble", "simble", address()).expect_err("unknown spec");
    let message = error.to_string();
    assert!(
        message.contains("unrecognized") && message.contains("bumble"),
        "the error should name the offending spec: {message}"
    );
}

#[test]
fn test_the_default_spec_builds_a_netsim_url_with_the_reversed_address() {
    assert_eq!(
        resolve("", "simble-speaker", address()).unwrap(),
        Backend::Netsim(
            "ws://127.0.0.1:7681/v1/websocket/bt?name=simble-speaker\
             &address=0B:0C:00:C0:DE:F0"
                .to_string()
        ),
        "netsim reads address= least-significant byte first"
    );
}

#[test]
fn test_the_word_netsim_resolves_the_same_as_the_empty_spec() {
    assert_eq!(
        resolve("netsim", "simble", address()).unwrap(),
        resolve("", "simble", address()).unwrap(),
    );
}

#[test]
fn test_an_explicit_ws_url_is_passed_through_verbatim() {
    // A hand-built URL must not be re-decorated with name/address: it may
    // already carry a different identity, and a second `?` would break it.
    let url = "ws://127.0.0.1:9999/v1/websocket/bt?name=other&address=01:02:03:04:05:06";
    assert_eq!(
        resolve(url, "ignored", address()).unwrap(),
        Backend::Netsim(url.to_string()),
    );
}

#[test]
fn test_a_tcp_spec_selects_the_h4_backend_and_keeps_the_socket_address() {
    assert_eq!(
        resolve("tcp:127.0.0.1:16402", "ignored", address()).unwrap(),
        Backend::Tcp("127.0.0.1:16402".to_string()),
    );
}

#[test]
fn test_open_from_env_defaults_to_netsim_when_the_variable_is_unset() {
    // `open_from_env` turns an unset variable into the empty spec; that this
    // spec means netsim is what keeps every pre-existing invocation working.
    assert!(matches!(
        resolve("", "simble", address()).unwrap(),
        Backend::Netsim(_)
    ));
}

#[test]
fn test_a_malformed_tcp_spec_is_refused_when_dialed() {
    // `resolve` is deliberately not a socket-address parser — it hands the
    // string to the transport, which is what reports an unusable one.
    assert_eq!(
        resolve("tcp:not-a-socket-address", "ignored", address()).unwrap(),
        Backend::Tcp("not-a-socket-address".to_string()),
    );
    assert!(
        LiveTransport::open("tcp:not-a-socket-address", "ignored", address()).is_err(),
        "a tcp: spec that is not host:port has nowhere to connect"
    );
}

#[test]
fn test_a_bare_usb_spec_selects_the_first_dongle() {
    assert_eq!(
        resolve("usb", "ignored", address()).unwrap(),
        Backend::Usb(UsbSelector::First),
    );
}

#[test]
fn test_a_usb_spec_carries_every_selector_form_through_unchanged() {
    // Each of these names a dongle differently, and the differences matter
    // on a bench with four plugged in: only the bus.port form survives a
    // re-plug. `resolve` must not flatten them into one.
    for (spec, expected) in [
        ("usb:#1", UsbSelector::Index(1)),
        ("usb:0a12:0001", UsbSelector::VidPid(0x0A12, 0x0001)),
        (
            "usb:02/4",
            UsbSelector::BusAddress {
                bus_id: "02".to_string(),
                device_address: 4,
            },
        ),
        (
            "usb:02.3.4",
            UsbSelector::BusPort {
                bus_id: "02".to_string(),
                port_chain: vec![3, 4],
            },
        ),
    ] {
        assert_eq!(
            resolve(spec, "ignored", address()).unwrap(),
            Backend::Usb(expected),
            "{spec} should reach the dongle it names",
        );
    }
}

#[test]
fn test_a_malformed_usb_selector_is_refused_by_resolve_rather_than_at_dial_time() {
    // Unlike `tcp:`, a dongle selector *is* parsed here — `UsbSelector::parse`
    // is pure, and its error lists the accepted forms. Reporting it before
    // touching USB is what keeps a typo from looking like absent hardware.
    let error = resolve("usb:nonsense", "ignored", address()).expect_err("no separator");
    let message = error.to_string();
    assert!(
        message.contains("nonsense"),
        "the error should name the offending selector: {message}"
    );
}
