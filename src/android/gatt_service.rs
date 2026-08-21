// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! `BluetoothGattService`/`BluetoothGattCharacteristic`/`BluetoothGattDescriptor`-
//! shaped builder types, mirroring their `android.bluetooth.*` namesakes,
//! including matching property/permission/status integer constant values
//! (verified against the Android SDK reference and AOSP source — see
//! constant doc comments below). Building a `BluetoothGattService` up from
//! these types and handing it to `BluetoothGattServer::add_service` is what
//! actually registers attributes in Simble's `GattDatabase`.

use crate::gatt::{AttributePermissions, CharacteristicProperties};
use crate::types::Uuid;

/// Status/type constants mirroring `android.bluetooth.BluetoothGatt`.
pub struct BluetoothGatt;

impl BluetoothGatt {
    pub const GATT_SUCCESS: i32 = 0;
    pub const GATT_READ_NOT_PERMITTED: i32 = 2;
    pub const GATT_WRITE_NOT_PERMITTED: i32 = 3;
    pub const GATT_INSUFFICIENT_AUTHENTICATION: i32 = 5;
    pub const GATT_REQUEST_NOT_SUPPORTED: i32 = 6;
    pub const GATT_INVALID_OFFSET: i32 = 7;
    pub const GATT_INSUFFICIENT_AUTHORIZATION: i32 = 8;
    pub const GATT_INVALID_ATTRIBUTE_LENGTH: i32 = 13;
    pub const GATT_INSUFFICIENT_ENCRYPTION: i32 = 15;
    pub const GATT_CONNECTION_CONGESTED: i32 = 143;
    pub const GATT_FAILURE: i32 = 257;
}

/// A `BluetoothGattService`-shaped builder, mirroring
/// `android.bluetooth.BluetoothGattService`.
#[derive(Debug, Clone)]
pub struct BluetoothGattService {
    pub uuid: Uuid,
    pub service_type: i32,
    pub characteristics: Vec<BluetoothGattCharacteristic>,
}

impl BluetoothGattService {
    /// Verified via developer.android.com/reference/android/bluetooth/BluetoothGattService.
    pub const SERVICE_TYPE_PRIMARY: i32 = 0;
    pub const SERVICE_TYPE_SECONDARY: i32 = 1;

    /// Creates a service of the given type. Mirrors the
    /// `BluetoothGattService(UUID, int)` constructor.
    pub fn new(uuid: Uuid, service_type: i32) -> Self {
        Self {
            uuid,
            service_type,
            characteristics: Vec::new(),
        }
    }

    /// Adds a characteristic to this service. Mirrors
    /// `BluetoothGattService.addCharacteristic`.
    pub fn add_characteristic(&mut self, characteristic: BluetoothGattCharacteristic) -> bool {
        self.characteristics.push(characteristic);
        true
    }
}

/// A `BluetoothGattCharacteristic`-shaped builder, mirroring
/// `android.bluetooth.BluetoothGattCharacteristic`.
#[derive(Debug, Clone)]
pub struct BluetoothGattCharacteristic {
    pub uuid: Uuid,
    pub properties: i32,
    pub permissions: i32,
    pub value: Vec<u8>,
    pub descriptors: Vec<BluetoothGattDescriptor>,
    /// Populated once this characteristic has been registered via
    /// `BluetoothGattServer::add_service`.
    pub value_handle: Option<u16>,
}

impl BluetoothGattCharacteristic {
    // PROPERTY_* values verified against
    // developer.android.com/reference/android/bluetooth/BluetoothGattCharacteristic.
    pub const PROPERTY_BROADCAST: i32 = 0x01;
    pub const PROPERTY_READ: i32 = 0x02;
    pub const PROPERTY_WRITE_NO_RESPONSE: i32 = 0x04;
    pub const PROPERTY_WRITE: i32 = 0x08;
    pub const PROPERTY_NOTIFY: i32 = 0x10;
    pub const PROPERTY_INDICATE: i32 = 0x20;
    pub const PROPERTY_SIGNED_WRITE: i32 = 0x40;
    pub const PROPERTY_EXTENDED_PROPS: i32 = 0x80;

    // PERMISSION_* values, same source.
    pub const PERMISSION_READ: i32 = 0x01;
    pub const PERMISSION_READ_ENCRYPTED: i32 = 0x02;
    pub const PERMISSION_READ_ENCRYPTED_MITM: i32 = 0x04;
    pub const PERMISSION_WRITE: i32 = 0x10;
    pub const PERMISSION_WRITE_ENCRYPTED: i32 = 0x20;
    pub const PERMISSION_WRITE_ENCRYPTED_MITM: i32 = 0x40;
    pub const PERMISSION_WRITE_SIGNED: i32 = 0x80;
    pub const PERMISSION_WRITE_SIGNED_MITM: i32 = 0x100;

    /// Creates a characteristic with the given properties and permissions.
    /// Mirrors the `BluetoothGattCharacteristic(UUID, int, int)` constructor.
    pub fn new(uuid: Uuid, properties: i32, permissions: i32) -> Self {
        Self {
            uuid,
            properties,
            permissions,
            value: Vec::new(),
            descriptors: Vec::new(),
            value_handle: None,
        }
    }

    /// Adds a descriptor to this characteristic. Mirrors
    /// `BluetoothGattCharacteristic.addDescriptor`.
    pub fn add_descriptor(&mut self, descriptor: BluetoothGattDescriptor) -> bool {
        self.descriptors.push(descriptor);
        true
    }

    /// Sets the cached value. Mirrors `BluetoothGattCharacteristic.setValue`.
    pub fn set_value(&mut self, value: impl Into<Vec<u8>>) -> bool {
        self.value = value.into();
        true
    }

    /// Returns the cached value. Mirrors `BluetoothGattCharacteristic.getValue`.
    pub fn get_value(&self) -> &[u8] {
        &self.value
    }

    /// Returns this characteristic's UUID. Mirrors
    /// `BluetoothGattCharacteristic.getUuid`.
    pub fn get_uuid(&self) -> Uuid {
        self.uuid
    }
}

/// A `BluetoothGattDescriptor`-shaped builder, mirroring
/// `android.bluetooth.BluetoothGattDescriptor`.
#[derive(Debug, Clone)]
pub struct BluetoothGattDescriptor {
    pub uuid: Uuid,
    pub permissions: i32,
    pub value: Vec<u8>,
    /// Populated once this descriptor has been registered via
    /// `BluetoothGattServer::add_service`.
    pub handle: Option<u16>,
}

impl BluetoothGattDescriptor {
    /// Creates a descriptor with the given permissions. Mirrors the
    /// `BluetoothGattDescriptor(UUID, int)` constructor.
    pub fn new(uuid: Uuid, permissions: i32) -> Self {
        Self {
            uuid,
            permissions,
            value: Vec::new(),
            handle: None,
        }
    }

    /// Sets the cached value. Mirrors `BluetoothGattDescriptor.setValue`.
    pub fn set_value(&mut self, value: impl Into<Vec<u8>>) -> bool {
        self.value = value.into();
        true
    }

    /// Returns the cached value. Mirrors `BluetoothGattDescriptor.getValue`.
    pub fn get_value(&self) -> &[u8] {
        &self.value
    }

    /// Returns this descriptor's UUID. Mirrors
    /// `BluetoothGattDescriptor.getUuid`.
    pub fn get_uuid(&self) -> Uuid {
        self.uuid
    }
}

/// Translates Android's `PROPERTY_*` bitmask onto Simble's
/// `CharacteristicProperties`. The low byte lines up bit-for-bit: both
/// Android and Simble copy the property octet straight from the Bluetooth
/// Core Spec's Characteristic Properties field, so this is a direct mask,
/// not a lookup translation.
pub(crate) fn properties_from_android(properties: i32) -> CharacteristicProperties {
    CharacteristicProperties((properties & 0xFF) as u8)
}

/// Translates Android's `PERMISSION_*` bitmask onto Simble's
/// `AttributePermissions`. Unlike properties, these do NOT line up 1:1:
/// Android has 8 independent bits (splitting encrypted vs. encrypted+MITM
/// read/write, plus a signed-write pair Simble has no equivalent for), while
/// `AttributePermissions` has 6 bools (a plain read/write gate plus separate
/// "authenticated" and "encrypted" qualifiers). We fold Android's `_MITM`
/// variant into `read_auth`/`write_auth` (authenticated = MITM-protected, in
/// BLE security terms) and its plain `_ENCRYPTED` variant into
/// `encrypt_read`/`encrypt_write`, and treat `PERMISSION_WRITE_SIGNED*` as
/// plain write capability since Simble has no signed-write permission.
pub(crate) fn permissions_from_android(permissions: i32) -> AttributePermissions {
    use BluetoothGattCharacteristic as C;

    let mut perms = AttributePermissions {
        read: false,
        write: false,
        read_auth: false,
        write_auth: false,
        encrypt_read: false,
        encrypt_write: false,
    };

    if permissions & C::PERMISSION_READ != 0 {
        perms.read = true;
    }
    if permissions & C::PERMISSION_READ_ENCRYPTED != 0 {
        perms.read = true;
        perms.encrypt_read = true;
    }
    if permissions & C::PERMISSION_READ_ENCRYPTED_MITM != 0 {
        perms.read = true;
        perms.encrypt_read = true;
        perms.read_auth = true;
    }
    if permissions & C::PERMISSION_WRITE != 0 {
        perms.write = true;
    }
    if permissions & C::PERMISSION_WRITE_ENCRYPTED != 0 {
        perms.write = true;
        perms.encrypt_write = true;
    }
    if permissions & C::PERMISSION_WRITE_ENCRYPTED_MITM != 0 {
        perms.write = true;
        perms.encrypt_write = true;
        perms.write_auth = true;
    }
    if permissions & C::PERMISSION_WRITE_SIGNED != 0 {
        perms.write = true;
    }
    if permissions & C::PERMISSION_WRITE_SIGNED_MITM != 0 {
        perms.write = true;
        perms.write_auth = true;
    }

    perms
}
