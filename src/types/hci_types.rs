// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

use std::fmt;

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

/// Generic enable/disable.
#[derive(
    Debug, Copy, Clone, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
#[repr(transparent)]
pub struct Enable(pub u8);

impl Enable {
    pub const DISABLED: Self = Self(0x00);
    pub const ENABLED: Self = Self(0x01);
}

/// Advertising Type for LE Set Advertising Parameters.
#[derive(
    Debug, Copy, Clone, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
#[repr(transparent)]
pub struct AdvertisingType(pub u8);

impl AdvertisingType {
    pub const ADV_IND: Self = Self(0x00);
    pub const ADV_DIRECT_IND_HIGH: Self = Self(0x01);
    pub const ADV_SCAN_IND: Self = Self(0x02);
    pub const ADV_NONCONN_IND: Self = Self(0x03);
    pub const ADV_DIRECT_IND_LOW: Self = Self(0x04);
}

/// Own Address Type.
#[derive(
    Debug, Copy, Clone, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
#[repr(transparent)]
pub struct OwnAddressType(pub u8);

impl OwnAddressType {
    pub const PUBLIC_DEVICE_ADDRESS: Self = Self(0x00);
    pub const RANDOM_DEVICE_ADDRESS: Self = Self(0x01);
    pub const RESOLVABLE_OR_PUBLIC_ADDRESS: Self = Self(0x02);
    pub const RESOLVABLE_OR_RANDOM_ADDRESS: Self = Self(0x03);
}

/// Peer Address Type.
#[derive(
    Debug, Copy, Clone, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
#[repr(transparent)]
pub struct PeerAddressType(pub u8);

impl PeerAddressType {
    pub const PUBLIC_DEVICE_OR_IDENTITY_ADDRESS: Self = Self(0x00);
    pub const RANDOM_DEVICE_OR_IDENTITY_ADDRESS: Self = Self(0x01);
}

/// Advertising Filter Policy.
#[derive(
    Debug, Copy, Clone, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
#[repr(transparent)]
pub struct AdvertisingFilterPolicy(pub u8);

impl AdvertisingFilterPolicy {
    pub const ALL_DEVICES: Self = Self(0x00);
    pub const LISTED_SCAN: Self = Self(0x01);
    pub const LISTED_CONNECT: Self = Self(0x02);
    pub const LISTED_SCAN_AND_CONNECT: Self = Self(0x03);
}

/// LE Scan Type.
#[derive(
    Debug, Copy, Clone, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
#[repr(transparent)]
pub struct LeScanType(pub u8);

impl LeScanType {
    pub const PASSIVE: Self = Self(0x00);
    pub const ACTIVE: Self = Self(0x01);
}

/// LE Scanning Filter Policy.
#[derive(
    Debug, Copy, Clone, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
#[repr(transparent)]
pub struct LeScanningFilterPolicy(pub u8);

impl LeScanningFilterPolicy {
    pub const ACCEPT_ALL: Self = Self(0x00);
    pub const FILTER_ACCEPT_LIST_ONLY: Self = Self(0x01);
    pub const CHECK_INITIATORS_IDENTITY: Self = Self(0x02);
    pub const FILTER_ACCEPT_LIST_AND_INITIATORS_IDENTITY: Self = Self(0x03);
}

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::str::FromStr;

/// Standard 6-byte Bluetooth Address.
#[derive(
    Debug,
    Copy,
    Clone,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    FromBytes,
    IntoBytes,
    Unaligned,
    Immutable,
    KnownLayout,
)]
#[repr(C)]
pub struct Address {
    pub bytes: [u8; 6],
}

impl Address {
    /// An all-zero Bluetooth address.
    pub const ANY: Self = Self { bytes: [0; 6] };

    /// Creates an address from a little-endian byte array.
    pub const fn new(bytes: [u8; 6]) -> Self {
        Self { bytes }
    }

    /// Creates an address from big-endian bytes (human readable order).
    pub fn from_be_bytes(bytes: [u8; 6]) -> Self {
        Self {
            bytes: [bytes[5], bytes[4], bytes[3], bytes[2], bytes[1], bytes[0]],
        }
    }

    /// Returns the address as a byte slice (little-endian).
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the address in big-endian byte array.
    pub fn to_be_bytes(&self) -> [u8; 6] {
        [
            self.bytes[5],
            self.bytes[4],
            self.bytes[3],
            self.bytes[2],
            self.bytes[1],
            self.bytes[0],
        ]
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            self.bytes[5],
            self.bytes[4],
            self.bytes[3],
            self.bytes[2],
            self.bytes[1],
            self.bytes[0]
        )
    }
}

impl FromStr for Address {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() != 6 {
            return Err(format!("Invalid Bluetooth address format: {s}"));
        }

        let mut bytes = [0u8; 6];
        for (i, part) in parts.iter().enumerate() {
            bytes[5 - i] = u8::from_str_radix(part, 16)
                .map_err(|_| format!("Invalid hex byte in address: {part}"))?;
        }

        Ok(Self { bytes })
    }
}

macro_rules! impl_display_fromstr_serde {
    ($t:ident) => {
        impl Serialize for $t {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.to_string())
            }
        }

        impl<'de> Deserialize<'de> for $t {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let s = String::deserialize(deserializer)?;
                $t::from_str(&s).map_err(serde::de::Error::custom)
            }
        }
    };
}

impl_display_fromstr_serde!(Address);

/// Address type for LE advertising and connections.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AddressType {
    /// Public device address.
    Public = 0x00,
    /// Random device address (Static, RPA, or NRPA).
    Random = 0x01,
    /// Public Identity address (corresponds to resolved RPA).
    PublicIdentity = 0x02,
    /// Random Identity address (corresponds to resolved RPA).
    RandomIdentity = 0x03,
}

/// Represents a 16-bit or 128-bit Bluetooth UUID.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Uuid {
    /// 16-bit Bluetooth SIG assigned UUID.
    Uuid16(u16),
    /// Full 128-bit custom UUID.
    Uuid128([u8; 16]),
}

/// Lets every one of the ~25 `Uuid::from_u16(0x____)` call sites across
/// profile modules shrink to a bare literal wherever the target accepts
/// `impl Into<Uuid>` (see `GattDatabase::add_service`, etc.) — and lets a
/// bare `u16` be passed directly, not just written as `Uuid::from_u16(...)`.
impl From<u16> for Uuid {
    fn from(value: u16) -> Self {
        Self::Uuid16(value)
    }
}

/// Same reasoning as `From<u16>`, for the 128-bit case.
impl From<[u8; 16]> for Uuid {
    fn from(value: [u8; 16]) -> Self {
        Self::Uuid128(value)
    }
}

impl Uuid {
    /// Bluetooth SIG base UUID: 00000000-0000-1000-8000-00805F9B34FB
    pub const BASE_UUID: [u8; 16] = [
        0xFB, 0x34, 0x9B, 0x5F, 0x80, 0x00, 0x00, 0x80, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00,
    ];

    /// Primary Service Declaration UUID (0x2800).
    pub const PRIMARY_SERVICE: Self = Self::Uuid16(0x2800);
    /// Secondary Service Declaration UUID (0x2801).
    pub const SECONDARY_SERVICE: Self = Self::Uuid16(0x2801);
    /// Characteristic Declaration UUID (0x2803).
    pub const CHARACTERISTIC: Self = Self::Uuid16(0x2803);
    /// Client Characteristic Configuration Descriptor UUID (0x2902).
    pub const CCCD: Self = Self::Uuid16(0x2902);
    /// Characteristic User Description UUID (0x2901).
    pub const USER_DESCRIPTION: Self = Self::Uuid16(0x2901);

    /// Creates a 16-bit UUID.
    pub const fn from_u16(val: u16) -> Self {
        Self::Uuid16(val)
    }

    /// Creates a 128-bit UUID from little-endian bytes.
    pub const fn from_u128_bytes(bytes: [u8; 16]) -> Self {
        Self::Uuid128(bytes)
    }

    /// Parses a 16-bit or 128-bit UUID from a little-endian byte slice.
    pub fn from_bytes(slice: &[u8]) -> Option<Self> {
        if slice.len() == 2 {
            Some(Self::Uuid16(u16::from_le_bytes([slice[0], slice[1]])))
        } else if slice.len() == 16 {
            let mut b = [0u8; 16];
            b.copy_from_slice(slice);
            Some(Self::Uuid128(b))
        } else {
            None
        }
    }

    /// Converts this UUID to a 128-bit little-endian byte array.
    pub fn to_128_bit_bytes(&self) -> [u8; 16] {
        match self {
            Self::Uuid16(val) => {
                let mut bytes = Self::BASE_UUID;
                bytes[12] = (*val & 0xFF) as u8;
                bytes[13] = ((*val >> 8) & 0xFF) as u8;
                bytes
            }
            Self::Uuid128(bytes) => *bytes,
        }
    }

    /// Returns the length of this UUID in bytes (2 or 16).
    pub fn len(&self) -> usize {
        match self {
            Self::Uuid16(_) => 2,
            Self::Uuid128(_) => 16,
        }
    }

    /// Returns true if this is an empty / zero UUID.
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Uuid16(v) => *v == 0,
            Self::Uuid128(b) => b.iter().all(|&x| x == 0),
        }
    }
}

impl fmt::Display for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Uuid16(val) => write!(f, "{val:04X}"),
            Self::Uuid128(b) => {
                write!(
                    f,
                    "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
                    b[15],
                    b[14],
                    b[13],
                    b[12],
                    b[11],
                    b[10],
                    b[9],
                    b[8],
                    b[7],
                    b[6],
                    b[5],
                    b[4],
                    b[3],
                    b[2],
                    b[1],
                    b[0]
                )
            }
        }
    }
}

impl fmt::Debug for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Uuid({self})")
    }
}

impl FromStr for Uuid {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let clean = s.replace('-', "");
        if clean.len() == 4 {
            let val =
                u16::from_str_radix(&clean, 16).map_err(|_| format!("Invalid 16-bit UUID: {s}"))?;
            Ok(Self::Uuid16(val))
        } else if clean.len() == 32 {
            let mut bytes = [0u8; 16];
            for i in 0..16 {
                let byte_str = &clean[i * 2..i * 2 + 2];
                let byte = u8::from_str_radix(byte_str, 16)
                    .map_err(|_| format!("Invalid hex in 128-bit UUID: {s}"))?;
                bytes[15 - i] = byte;
            }
            Ok(Self::Uuid128(bytes))
        } else {
            Err(format!("Invalid UUID length: {s}"))
        }
    }
}

impl_display_fromstr_serde!(Uuid);

/// Event Type for LE Advertising Report.
#[derive(
    Debug, Copy, Clone, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
#[repr(transparent)]
pub struct LeAdvertisingEventType(pub u8);

impl LeAdvertisingEventType {
    pub const ADV_IND: Self = Self(0x00);
    pub const ADV_DIRECT_IND: Self = Self(0x01);
    pub const ADV_SCAN_IND: Self = Self(0x02);
    pub const ADV_NONCONN_IND: Self = Self(0x03);
    pub const SCAN_RSP: Self = Self(0x04);
}

impl fmt::Display for LeAdvertisingEventType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::ADV_IND => write!(f, "ADV_IND"),
            Self::ADV_DIRECT_IND => write!(f, "ADV_DIRECT_IND"),
            Self::ADV_SCAN_IND => write!(f, "ADV_SCAN_IND"),
            Self::ADV_NONCONN_IND => write!(f, "ADV_NONCONN_IND"),
            Self::SCAN_RSP => write!(f, "SCAN_RSP"),
            _ => write!(f, "UNKNOWN ({:#04X})", self.0),
        }
    }
}

/// Generic Access Profile (GAP) Data Types for Advertising Data payload
/// parsing.
#[derive(
    Debug, Copy, Clone, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
#[repr(transparent)]
pub struct GapDataType(pub u8);

impl GapDataType {
    pub const FLAGS: Self = Self(0x01);
    pub const INCOMPLETE_16BIT_UUIDS: Self = Self(0x02);
    pub const COMPLETE_16BIT_UUIDS: Self = Self(0x03);
    pub const INCOMPLETE_32BIT_UUIDS: Self = Self(0x04);
    pub const COMPLETE_32BIT_UUIDS: Self = Self(0x05);
    pub const INCOMPLETE_128BIT_UUIDS: Self = Self(0x06);
    pub const COMPLETE_128BIT_UUIDS: Self = Self(0x07);
    pub const SHORTENED_LOCAL_NAME: Self = Self(0x08);
    pub const COMPLETE_LOCAL_NAME: Self = Self(0x09);
    pub const TX_POWER_LEVEL: Self = Self(0x0A);
    pub const SERVICE_DATA_16BIT: Self = Self(0x16);
    pub const APPEARANCE: Self = Self(0x19);
    pub const SERVICE_DATA_32BIT: Self = Self(0x20);
    pub const SERVICE_DATA_128BIT: Self = Self(0x21);
    pub const MANUFACTURER_SPECIFIC: Self = Self(0xFF);
}

impl fmt::Display for GapDataType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::FLAGS => write!(f, "Flags"),
            Self::INCOMPLETE_16BIT_UUIDS => write!(f, "Incomplete 16-bit UUIDs"),
            Self::COMPLETE_16BIT_UUIDS => write!(f, "Complete 16-bit UUIDs"),
            Self::INCOMPLETE_32BIT_UUIDS => write!(f, "Incomplete 32-bit UUIDs"),
            Self::COMPLETE_32BIT_UUIDS => write!(f, "Complete 32-bit UUIDs"),
            Self::INCOMPLETE_128BIT_UUIDS => write!(f, "Incomplete 128-bit UUIDs"),
            Self::COMPLETE_128BIT_UUIDS => write!(f, "Complete 128-bit UUIDs"),
            Self::SHORTENED_LOCAL_NAME => write!(f, "Shortened Local Name"),
            Self::COMPLETE_LOCAL_NAME => write!(f, "Complete Local Name"),
            Self::TX_POWER_LEVEL => write!(f, "Tx Power Level"),
            Self::SERVICE_DATA_16BIT => write!(f, "Service Data 16-bit"),
            Self::APPEARANCE => write!(f, "Appearance"),
            Self::SERVICE_DATA_32BIT => write!(f, "Service Data 32-bit"),
            Self::SERVICE_DATA_128BIT => write!(f, "Service Data 128-bit"),
            Self::MANUFACTURER_SPECIFIC => write!(f, "Manufacturer Specific Data"),
            _ => write!(f, "Unknown ({:#04X})", self.0),
        }
    }
}
