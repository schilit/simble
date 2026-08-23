// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Object Push Profile (OPP 1.2) — "Bluetooth share" on Android.
//!
//! The thinnest OBEX profile: a peer PUTs an object (a vCard, a photo, a
//! file) and the receiver accepts it. There is no folder model, no session
//! state worth the name, and — per OPP 1.2 Section 4.3 — **no requirement to
//! CONNECT first**, which is why [`ObexServer`] is created here with
//! [`SessionPolicy::Optional`].
//!
//! What is here is the profile: an OBEX server configured OPP's way, and the
//! SDP record a phone needs in order to discover it.

use crate::classic::sdp::{DataElement, Service, ServiceAttribute, attribute_id};

use super::server::{ObexServer, ServerLimits, SessionPolicy};

/// Service class and protocol UUIDs OPP needs (Assigned Numbers).
pub mod opp_uuid {
    use crate::classic::sdp::SdpUuid;

    /// OBEXObjectPush service class.
    pub const OBJECT_PUSH_SERVICE: SdpUuid = SdpUuid::Uuid16(0x1105);
    /// The OBEX protocol identifier, used in the protocol descriptor list.
    pub const OBEX_PROTOCOL: SdpUuid = SdpUuid::Uuid16(0x0008);
    /// L2CAP protocol identifier.
    pub const L2CAP_PROTOCOL: SdpUuid = SdpUuid::Uuid16(0x0100);
    /// RFCOMM protocol identifier.
    pub const RFCOMM_PROTOCOL: SdpUuid = SdpUuid::Uuid16(0x0003);
}

/// Object formats an OPP server declares support for (OPP 1.2, Section 5.1,
/// the `SupportedFormatsList` attribute).
pub mod object_format {
    /// vCard 2.1.
    pub const VCARD_2_1: u8 = 0x01;
    /// vCard 3.0.
    pub const VCARD_3_0: u8 = 0x02;
    /// vCal 1.0.
    pub const VCAL_1_0: u8 = 0x03;
    /// iCal 2.0.
    pub const ICAL_2_0: u8 = 0x04;
    /// vNote.
    pub const VNOTE: u8 = 0x05;
    /// vMessage.
    pub const VMESSAGE: u8 = 0x06;
    /// Any type — what a general-purpose receiver advertises.
    pub const ANY: u8 = 0xFF;
}

/// The `SupportedFormatsList` attribute ID (OPP 1.2, Section 5.1).
pub const SUPPORTED_FORMATS_LIST: u16 = 0x0303;

/// ServiceName for the primary language.
///
/// Service-name attributes are not fixed IDs: they sit at an offset from the
/// base named in `LanguageBaseAttributeIDList`, and 0x0100 is that base for
/// the primary language every record declares (Core Spec Vol 3, Part B,
/// Section 5.1.10).
pub const SERVICE_NAME_PRIMARY: u16 = 0x0100;

/// Creates an OBEX server configured the way OPP requires.
///
/// The distinguishing choice is [`SessionPolicy::Optional`]: pushing a vCard
/// at a device you have never connected to is the profile's entire purpose,
/// so refusing a session-less PUT would break it.
pub fn object_push_server(limits: ServerLimits) -> ObexServer {
    ObexServer::new(SessionPolicy::Optional, limits)
}

/// Builds the SDP service record a phone looks for before pushing.
///
/// `rfcomm_channel` must match the channel the RFCOMM server actually
/// listens on — an SDP record naming a channel nothing serves is a promise
/// the device cannot keep, and the peer's connection simply times out.
///
/// `formats` is the `SupportedFormatsList`; pass `&[object_format::ANY]` for
/// a receiver that will take anything.
pub fn object_push_service_record(
    rfcomm_channel: u8,
    service_name: &str,
    formats: &[u8],
) -> Service {
    vec![
        ServiceAttribute::new(
            attribute_id::SERVICE_CLASS_ID_LIST,
            DataElement::sequence(vec![DataElement::uuid(opp_uuid::OBJECT_PUSH_SERVICE)]),
        ),
        // L2CAP → RFCOMM(channel) → OBEX: the stack a peer must climb to
        // reach this service.
        ServiceAttribute::new(
            attribute_id::PROTOCOL_DESCRIPTOR_LIST,
            DataElement::sequence(vec![
                DataElement::sequence(vec![DataElement::uuid(opp_uuid::L2CAP_PROTOCOL)]),
                DataElement::sequence(vec![
                    DataElement::uuid(opp_uuid::RFCOMM_PROTOCOL),
                    DataElement::unsigned_integer(rfcomm_channel as u64, 1),
                ]),
                DataElement::sequence(vec![DataElement::uuid(opp_uuid::OBEX_PROTOCOL)]),
            ]),
        ),
        ServiceAttribute::new(
            SERVICE_NAME_PRIMARY,
            DataElement::text_string(service_name.as_bytes().to_vec()),
        ),
        ServiceAttribute::new(
            SUPPORTED_FORMATS_LIST,
            DataElement::sequence(
                formats
                    .iter()
                    .map(|&f| DataElement::unsigned_integer(f as u64, 1))
                    .collect::<Vec<_>>(),
            ),
        ),
    ]
}

// ---------------------------------------------------------------------------
// What the sibling profiles still need
// ---------------------------------------------------------------------------
//
// PBAP (Phone Book Access) and MAP (Message Access) ride the same OBEX core
// built here, so neither needs new transport work. What each additionally
// requires:
//
// **PBAP** — a session is mandatory (`SessionPolicy::Required`), because the
// CONNECT carries a Target header naming the Phone Book Access service UUID
// (0x796135F0-F0C5-11D8-0966-0800200C9A66). On top of that:
//   * GET, which this server currently answers `Not Implemented` — PBAP is
//     pull-only, so GET is the whole profile.
//   * SETPATH with the phonebook folder semantics (`telecom/pb`,
//     `telecom/ich`, …), including the "go to parent" flag.
//   * vCard 2.1/3.0 generation, and the `x-bt/phonebook` and
//     `x-bt/vcard-listing` MIME types.
//   * App Parameters, which PBAP uses heavily (MaxListCount, ListStartOffset,
//     Filter, Format, and the PhonebookSize/NewMissedCalls responses). The
//     header is already carried verbatim; the tag-length-value contents are
//     not parsed.
//
// **MAP** (Message Access) needs everything PBAP does, plus:
//   * Two channels: the Message Access Service and a separate Message
//     Notification Service in the *reverse* direction, so the device pushes
//     notifications to the phone — meaning a client role on a second RFCOMM
//     channel, and a second SDP record.
//   * bMessage encoding (the message format itself) and the folder listing
//     format.
//   * Its own Target UUID, and MAP-specific App Parameters.
//
// Neither is blocked on anything in this module; both are profile work.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::obex::packet::{Response, response};
    use crate::obex::server::put_packets;

    #[test]
    fn test_a_vcard_can_be_pushed_without_connecting_first() {
        // OPP 1.2 Section 4.3: a push with no session must be accepted.
        let mut server = object_push_server(ServerLimits::default());
        let vcard = b"BEGIN:VCARD\r\nVERSION:2.1\r\nN:Schilit;Bill\r\nEND:VCARD\r\n";

        for packet in put_packets(Some("bill.vcf"), Some(b"text/x-vcard\0"), vcard, 0x2000) {
            let (bytes, _) = server.handle_packet(&packet);
            let parsed = Response::parse(&bytes, false).unwrap();
            assert!(
                parsed.code == response::CONTINUE || parsed.code == response::SUCCESS,
                "unexpected {:#04X}",
                parsed.code
            );
        }

        let objects = server.take_objects();
        assert_eq!(objects.len(), 1);
        assert_eq!(objects[0].name.as_deref(), Some("bill.vcf"));
        assert_eq!(objects[0].body, vcard);
    }

    #[test]
    fn test_a_large_object_is_reassembled_across_packets() {
        let mut server = object_push_server(ServerLimits::default());
        let photo: Vec<u8> = (0..5000u32).map(|i| (i % 256) as u8).collect();
        for packet in put_packets(Some("photo.jpg"), Some(b"image/jpeg\0"), &photo, 512) {
            server.handle_packet(&packet);
        }
        let objects = server.take_objects();
        assert_eq!(objects[0].body, photo);
        assert_eq!(objects[0].declared_length, Some(5000));
    }

    /// The record must name the channel RFCOMM actually serves, and carry
    /// the L2CAP → RFCOMM → OBEX stack a peer climbs.
    #[test]
    fn test_service_record_describes_the_obex_stack() {
        let record = object_push_service_record(9, "Simble Object Push", &[object_format::ANY]);

        let classes =
            ServiceAttribute::find_attribute_in_list(&record, attribute_id::SERVICE_CLASS_ID_LIST)
                .expect("service class list");
        assert!(ServiceAttribute::is_uuid_in_value(
            opp_uuid::OBJECT_PUSH_SERVICE,
            classes
        ));

        let protocols = ServiceAttribute::find_attribute_in_list(
            &record,
            attribute_id::PROTOCOL_DESCRIPTOR_LIST,
        )
        .expect("protocol descriptor list");
        for uuid in [
            opp_uuid::L2CAP_PROTOCOL,
            opp_uuid::RFCOMM_PROTOCOL,
            opp_uuid::OBEX_PROTOCOL,
        ] {
            assert!(
                ServiceAttribute::is_uuid_in_value(uuid, protocols),
                "{uuid:?} missing from the protocol stack"
            );
        }

        // The RFCOMM channel a peer will dial.
        let DataElement::Sequence(layers) = protocols else {
            panic!("expected a sequence of protocol layers");
        };
        let DataElement::Sequence(rfcomm) = &layers[1] else {
            panic!("expected the RFCOMM layer");
        };
        assert_eq!(
            rfcomm[1].as_unsigned_integer().map(|(v, _)| v),
            Some(9),
            "the advertised channel must be the one served"
        );

        assert!(
            ServiceAttribute::find_attribute_in_list(&record, SUPPORTED_FORMATS_LIST).is_some(),
            "OPP requires SupportedFormatsList"
        );
    }
}
