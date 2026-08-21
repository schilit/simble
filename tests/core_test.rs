// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Core type tests: UUID parsing/formatting, Address handling, and GAP
//! Advertising Data payloads.

use simble::gap::{AdvertisingData, flags};
use simble::types::{Address, Uuid};
use std::str::FromStr;

#[test]
fn test_uuid_from_16_bit() {
    let u = Uuid::from_u16(0x7788);
    assert_eq!(u.to_string(), "7788");
    assert_eq!(u.len(), 2);
}

#[test]
fn test_uuid_128_bit_string_representation() {
    let uuid_str = "61a3512c-09be-4ddc-a6a6-0b03667aafc6";
    let u = Uuid::from_str(uuid_str).expect("Valid 128-bit UUID");
    assert_eq!(u.to_string(), uuid_str);
    assert_eq!(u.len(), 16);

    // Parsing again from formatted string
    let u2 = Uuid::from_str(&u.to_string()).expect("Reparse");
    assert_eq!(u, u2);
}

#[test]
fn test_uuid_16_to_128_base_expansion() {
    let u16_val = Uuid::from_u16(0x180D);
    let bytes128 = u16_val.to_128_bit_bytes();

    // In Bluetooth Base UUID (0000xxxx-0000-1000-8000-00805F9B34FB):
    // Little-endian index 12 and 13 contain the 16-bit value
    assert_eq!(bytes128[12], 0x0D);
    assert_eq!(bytes128[13], 0x18);
    assert_eq!(bytes128[14], 0x00);
    assert_eq!(bytes128[15], 0x00);
}

#[test]
fn test_address_formatting_and_parsing() {
    let addr_str = "F0:DE:F1:22:33:44";
    let addr = Address::from_str(addr_str).expect("Valid Address");
    assert_eq!(addr.to_string(), addr_str);

    // Byte layout in memory should be little-endian (least significant first)
    assert_eq!(addr.bytes, [0x44, 0x33, 0x22, 0xF1, 0xDE, 0xF0]);

    // to_be_bytes should return [0xF0, 0xDE, 0xF1, 0x22, 0x33, 0x44]
    assert_eq!(addr.to_be_bytes(), [0xF0, 0xDE, 0xF1, 0x22, 0x33, 0x44]);
}

#[test]
fn test_advertising_data_payload_structure() {
    let ad = AdvertisingData::new()
        .with_flags(flags::LE_GENERAL_DISCOVERABLE | flags::BR_EDR_NOT_SUPPORTED)
        .with_name("MyTestDevice")
        .with_service_uuid_16(0x180F) // Battery Service
        .with_manufacturer_data(0x00E0, &[0x01, 0x02, 0x03]); // Google Company ID

    let encoded = ad.to_bytes();

    // 1. Flags: Len=2, Type=0x01, Value=0x06
    assert_eq!(&encoded[0..3], &[0x02, 0x01, 0x06]);

    // 2. Name: Len=13, Type=0x09, Value="MyTestDevice"
    assert_eq!(encoded[3], 13);
    assert_eq!(encoded[4], 0x09);
    assert_eq!(&encoded[5..17], b"MyTestDevice");

    // 3. Service UUIDs: Len=3, Type=0x03, Value=0x0F, 0x18
    assert_eq!(encoded[17], 3);
    assert_eq!(encoded[18], 0x03);
    assert_eq!(&encoded[19..21], &[0x0F, 0x18]);

    // 4. Manufacturer data: Len=6, Type=0xFF, CompanyID=0x00E0, Data=[1,2,3]
    assert_eq!(encoded[21], 6);
    assert_eq!(encoded[22], 0xFF);
    assert_eq!(&encoded[23..28], &[0xE0, 0x00, 0x01, 0x02, 0x03]);
}
