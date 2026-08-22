// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! GATT (Generic Attribute Profile) Server Database and handle management.

use crate::att::error_code;
use crate::types::Uuid;
use std::collections::BTreeMap;

/// Standard GATT UUIDs for declarations and descriptors.
pub mod service_uuid {
    /// Primary Service declaration UUID (0x2800).
    pub const PRIMARY_SERVICE: u16 = 0x2800;
    /// Secondary Service declaration UUID (0x2801).
    pub const SECONDARY_SERVICE: u16 = 0x2801;
    /// Include declaration UUID (0x2802).
    pub const INCLUDE: u16 = 0x2802;
    /// Characteristic declaration UUID (0x2803).
    pub const CHARACTERISTIC: u16 = 0x2803;
}

/// Standard GATT descriptor UUIDs.
pub mod desc_uuid {
    /// Client Characteristic Configuration Descriptor (CCCD, 0x2902).
    pub const CLIENT_CHARACTERISTIC_CONFIGURATION: u16 = 0x2902;
    /// Characteristic User Description descriptor (0x2901).
    pub const CHARACTERISTIC_USER_DESCRIPTION: u16 = 0x2901;
}

/// Bitmask for characteristic properties.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CharacteristicProperties(
    /// The raw characteristic-properties bitmask.
    pub u8,
);

impl CharacteristicProperties {
    /// Broadcast property bit.
    pub const BROADCAST: u8 = 0x01;
    /// Read property bit.
    pub const READ: u8 = 0x02;
    /// Write Without Response property bit.
    pub const WRITE_WITHOUT_RESPONSE: u8 = 0x04;
    /// Write property bit.
    pub const WRITE: u8 = 0x08;
    /// Notify property bit.
    pub const NOTIFY: u8 = 0x10;
    /// Indicate property bit.
    pub const INDICATE: u8 = 0x20;
    /// Authenticated Signed Writes property bit.
    pub const AUTHENTICATED_SIGNED_WRITES: u8 = 0x40;
    /// Extended Properties bit.
    pub const EXTENDED_PROPERTIES: u8 = 0x80;
}

/// Attribute permissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttributePermissions {
    /// Whether the attribute may be read.
    pub read: bool,
    /// Whether the attribute may be written.
    pub write: bool,
    /// Whether reads require an authenticated connection.
    pub read_auth: bool,
    /// Whether writes require an authenticated connection.
    pub write_auth: bool,
    /// Whether reads require an encrypted connection.
    pub encrypt_read: bool,
    /// Whether writes require an encrypted connection.
    pub encrypt_write: bool,
}

impl AttributePermissions {
    /// Read-only attribute permissions without authentication/encryption.
    pub const fn read_only() -> Self {
        Self {
            read: true,
            write: false,
            read_auth: false,
            write_auth: false,
            encrypt_read: false,
            encrypt_write: false,
        }
    }

    /// Write-only attribute permissions without authentication/encryption.
    pub const fn write_only() -> Self {
        Self {
            read: false,
            write: true,
            read_auth: false,
            write_auth: false,
            encrypt_read: false,
            encrypt_write: false,
        }
    }
}

impl Default for AttributePermissions {
    fn default() -> Self {
        Self {
            read: true,
            write: true,
            read_auth: false,
            write_auth: false,
            encrypt_read: false,
            encrypt_write: false,
        }
    }
}

/// A handler owning a specific attribute's write behavior, attached at
/// registration time via [`GattDatabase::set_handler`]. Mirrors how
/// embedded BLE stacks (e.g. NimBLE's per-characteristic `access_cb`) let a
/// characteristic own its own behavior instead of a central dispatcher
/// special-casing it: a profile with control-point-style validation (ASCS,
/// AICS, VOCS) registers one of these instead of exposing a bespoke
/// `write_control_point`-shaped method that callers must know to invoke
/// outside the normal ATT write path — any write to a handled attribute
/// flows through the same `GattDatabase::write` every other attribute
/// uses.
///
/// Requires `Send + Sync` for the same reason `AttServerObserver`
/// (`crate::device::observer`) does: `GattDatabase` ends up behind
/// `Arc<Mutex<_>>` via `VirtualDevice` in `service::manager`.
pub trait AttributeHandler: std::fmt::Debug + Send + Sync {
    /// Called instead of overwriting the stored value directly. The handler
    /// itself is detached from the attribute for the duration of the call
    /// (avoiding a self-referential borrow), but the attribute stays in the
    /// database — so `db.set_value(own_handle, ...)` works, and a handler
    /// that affects other handles (e.g. ASCS's control point updating
    /// several ASE characteristics) writes to them via `db` directly.
    /// `GattDatabase::write` never overwrites a handled attribute's value
    /// automatically; state changes are entirely the handler's job.
    fn on_write(&mut self, db: &mut GattDatabase, value: &[u8]) -> Result<(), u8>;
}

/// A GATT attribute stored in the database.
#[derive(Debug)]
pub struct Attribute {
    /// The attribute's handle (its address in the database).
    pub handle: u16,
    /// The attribute type UUID.
    pub uuid: Uuid,
    /// The attribute's stored value.
    pub value: Vec<u8>,
    /// Access permissions governing reads and writes.
    pub permissions: AttributePermissions,
    /// Optional owner of this attribute's write behavior. See
    /// `AttributeHandler`.
    pub handler: Option<Box<dyn AttributeHandler>>,
}

impl Clone for Attribute {
    fn clone(&self) -> Self {
        Self {
            handle: self.handle,
            uuid: self.uuid,
            value: self.value.clone(),
            permissions: self.permissions,
            // Trait objects aren't `Clone`; a cloned attribute starts unhandled,
            // same tradeoff `VirtualDevice`'s `observer` field already makes.
            handler: None,
        }
    }
}

impl PartialEq for Attribute {
    /// Ignores `handler` — trait objects have no meaningful equality, and
    /// every other field fully identifies an attribute's observable state.
    fn eq(&self, other: &Self) -> bool {
        self.handle == other.handle
            && self.uuid == other.uuid
            && self.value == other.value
            && self.permissions == other.permissions
    }
}

impl Eq for Attribute {}

/// The local GATT Database representing all registered services, characteristics,
/// and descriptors on the device.
#[derive(Debug, Clone, Default)]
pub struct GattDatabase {
    /// All registered attributes keyed by handle, in ascending handle order.
    pub attributes: BTreeMap<u16, Attribute>,
    next_handle: u16,
}

impl GattDatabase {
    /// Creates a new, empty GATT database.
    pub fn new() -> Self {
        Self {
            attributes: BTreeMap::new(),
            next_handle: 0x0001,
        }
    }

    /// Allocates the next available attribute handle.
    fn allocate_handle(&mut self) -> u16 {
        let h = self.next_handle;
        self.next_handle = self.next_handle.checked_add(1).expect("Handle overflow");
        h
    }

    /// Adds a Primary or Secondary Service declaration.
    pub fn add_service(&mut self, uuid: impl Into<Uuid>, primary: bool) -> u16 {
        let uuid = uuid.into();
        let handle = self.allocate_handle();
        let decl_uuid = if primary {
            Uuid::from(service_uuid::PRIMARY_SERVICE)
        } else {
            Uuid::from(service_uuid::SECONDARY_SERVICE)
        };

        let val = match uuid {
            Uuid::Uuid16(u) => u.to_le_bytes().to_vec(),
            Uuid::Uuid128(u) => u.to_vec(),
        };

        self.attributes.insert(
            handle,
            Attribute {
                handle,
                uuid: decl_uuid,
                value: val,
                permissions: AttributePermissions::read_only(),
                handler: None,
            },
        );

        handle
    }

    /// Adds an Include declaration (0x2802) referencing another service's
    /// attribute group, so one service can pull in another (e.g. a
    /// secondary battery service) without re-declaring it.
    pub fn add_include(
        &mut self,
        included_service_handle: u16,
        end_group_handle: u16,
        service_uuid: Option<Uuid>,
    ) -> u16 {
        // Core Spec Vol 3, Part G, Section 3.2: value = included service
        // attribute handle (LE) + end group handle (LE) + Service UUID,
        // where the UUID field is present only for 16-bit UUIDs — a
        // 128-bit included-service UUID is omitted and discovered from the
        // included service's own declaration instead.
        let mut value = Vec::with_capacity(6);
        value.extend_from_slice(&included_service_handle.to_le_bytes());
        value.extend_from_slice(&end_group_handle.to_le_bytes());
        if let Some(Uuid::Uuid16(u)) = service_uuid {
            value.extend_from_slice(&u.to_le_bytes());
        }
        self.add_attribute(
            Uuid::from_u16(service_uuid::INCLUDE),
            AttributePermissions::read_only(),
            value,
        )
    }

    /// Adds a Characteristic declaration followed by its Value attribute.
    pub fn add_characteristic(
        &mut self,
        uuid: impl Into<Uuid>,
        properties: CharacteristicProperties,
        initial_value: Vec<u8>,
        permissions: AttributePermissions,
    ) -> (u16, u16) {
        let uuid = uuid.into();
        let decl_handle = self.allocate_handle();
        let val_handle = self.allocate_handle();

        // 1. Characteristic Declaration Attribute (0x2803)
        // Value = [properties (1B), value_handle (2B), uuid (2B or 16B)]
        let mut decl_val = Vec::with_capacity(3 + 16);
        decl_val.push(properties.0);
        decl_val.extend_from_slice(&val_handle.to_le_bytes());
        match uuid {
            Uuid::Uuid16(u) => decl_val.extend_from_slice(&u.to_le_bytes()),
            Uuid::Uuid128(u) => decl_val.extend_from_slice(&u),
        }

        self.attributes.insert(
            decl_handle,
            Attribute {
                handle: decl_handle,
                uuid: Uuid::from(service_uuid::CHARACTERISTIC),
                value: decl_val,
                permissions: AttributePermissions::read_only(),
                handler: None,
            },
        );

        // 2. Characteristic Value Attribute
        self.attributes.insert(
            val_handle,
            Attribute {
                handle: val_handle,
                uuid,
                value: initial_value,
                permissions,
                handler: None,
            },
        );

        (decl_handle, val_handle)
    }

    /// Adds a Client Characteristic Configuration Descriptor (CCCD, 0x2902).
    pub fn add_cccd(&mut self) -> u16 {
        let handle = self.allocate_handle();
        self.attributes.insert(
            handle,
            Attribute {
                handle,
                uuid: Uuid::from_u16(desc_uuid::CLIENT_CHARACTERISTIC_CONFIGURATION),
                value: vec![0x00, 0x00],
                permissions: AttributePermissions::default(),
                handler: None,
            },
        );
        handle
    }

    /// Adds a custom descriptor.
    pub fn add_descriptor(
        &mut self,
        uuid: impl Into<Uuid>,
        value: Vec<u8>,
        permissions: AttributePermissions,
    ) -> u16 {
        self.add_attribute(uuid, permissions, value)
    }

    /// Adds a raw attribute directly with custom permissions and value.
    pub fn add_attribute(
        &mut self,
        uuid: impl Into<Uuid>,
        permissions: AttributePermissions,
        value: Vec<u8>,
    ) -> u16 {
        let handle = self.allocate_handle();
        self.attributes.insert(
            handle,
            Attribute {
                handle,
                uuid: uuid.into(),
                value,
                permissions,
                handler: None,
            },
        );
        handle
    }

    /// Reads an attribute value starting at `offset`.
    pub fn read(&self, handle: u16, offset: usize) -> Result<&[u8], u8> {
        let attr = self
            .attributes
            .get(&handle)
            .ok_or(error_code::INVALID_HANDLE)?;
        if !attr.permissions.read {
            return Err(error_code::READ_NOT_PERMITTED);
        }
        if offset > attr.value.len() {
            return Err(error_code::INVALID_OFFSET);
        }
        Ok(&attr.value[offset..])
    }

    /// Adds a characteristic together with the Client Characteristic
    /// Configuration descriptor its properties require.
    ///
    /// A characteristic that declares Notify or Indicate is useless without
    /// a CCCD — a client has nowhere to write its subscription, so it can
    /// never receive a value (Core Spec Vol 3, Part G, Section 3.3.3.3).
    /// Registrars kept forgetting it one characteristic at a time, so the
    /// pairing is expressed once, here.
    pub fn add_characteristic_with_cccd(
        &mut self,
        uuid: impl Into<Uuid>,
        properties: CharacteristicProperties,
        value: Vec<u8>,
        permissions: AttributePermissions,
    ) -> (u16, u16) {
        let handles = self.add_characteristic(uuid, properties, value, permissions);
        let notifying = CharacteristicProperties::NOTIFY | CharacteristicProperties::INDICATE;
        if properties.0 & notifying != 0 {
            self.add_descriptor(
                desc_uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
                vec![0x00, 0x00],
                AttributePermissions::default(),
            );
        }
        handles
    }

    /// The value handle of the first characteristic with `uuid`.
    ///
    /// Characteristic *declarations* carry UUID 0x2803 and the value
    /// attribute carries the characteristic's own UUID, so matching on the
    /// UUID finds the value attribute directly. This is how a script
    /// addresses characteristics created by a Rust profile registrar, which
    /// writes into the database rather than into the script's service list.
    pub fn value_handle_for_uuid(&self, uuid: Uuid) -> Option<u16> {
        self.attributes
            .iter()
            .find(|(_, attr)| attr.uuid == uuid)
            .map(|(&handle, _)| handle)
    }

    /// Reads an attribute value from the host simulation, without checking
    /// client permissions — the read-side mirror of [`Self::set_value`]. A
    /// device's own logic must see its whole database, including write-only
    /// attributes like a control point the peer just wrote.
    pub fn value(&self, handle: u16) -> Option<&[u8]> {
        self.attributes.get(&handle).map(|attr| attr.value.as_slice())
    }

    fn check_write_permitted(&mut self, handle: u16) -> Result<&mut Attribute, u8> {
        let attr = self
            .attributes
            .get_mut(&handle)
            .ok_or(error_code::INVALID_HANDLE)?;
        if !attr.permissions.write {
            return Err(error_code::WRITE_NOT_PERMITTED);
        }
        Ok(attr)
    }

    /// Writes a value to an attribute. If a handler is attached (see
    /// [`Self::set_handler`]), the write is delegated to it instead of
    /// overwriting the stored value directly — the attribute is removed
    /// from `self` for the duration of the call so the handler can take
    /// `&mut GattDatabase` without a self-referential borrow, then
    /// reinserted afterward.
    pub fn write(&mut self, handle: u16, value: &[u8]) -> Result<(), u8> {
        let attr = self.check_write_permitted(handle)?;
        if attr.handler.is_none() {
            attr.value = value.to_vec();
            return Ok(());
        }

        // Handler attached: take the handler out (avoiding the
        // self-referential borrow) but leave the attribute itself in place,
        // so a handler's `db.set_value(own_handle, ...)` updates the real
        // stored value — two profile implementations independently relied on
        // the documented contract and found the earlier remove/reinsert
        // approach silently discarded their self-writes.
        let attr = self
            .attributes
            .get_mut(&handle)
            .ok_or(error_code::INVALID_HANDLE)?;
        let mut handler = attr.handler.take().expect("handler checked above");
        let result = handler.on_write(self, value);
        if let Some(attr) = self.attributes.get_mut(&handle) {
            attr.handler = Some(handler);
        }
        result
    }

    /// Attaches an `AttributeHandler` to `handle`, so future writes to it
    /// (via [`Self::write`]) dispatch to the handler instead of overwriting
    /// the stored value directly.
    pub fn set_handler(
        &mut self,
        handle: u16,
        handler: Box<dyn AttributeHandler>,
    ) -> Result<(), u8> {
        let attr = self
            .attributes
            .get_mut(&handle)
            .ok_or(error_code::INVALID_HANDLE)?;
        attr.handler = Some(handler);
        Ok(())
    }

    /// Sets an attribute value directly from the host simulation without checking client permissions.
    pub fn set_value(&mut self, handle: u16, value: &[u8]) -> Result<(), u8> {
        let attr = self
            .attributes
            .get_mut(&handle)
            .ok_or(error_code::INVALID_HANDLE)?;
        attr.value = value.to_vec();
        Ok(())
    }

    /// Writes a value to an attribute starting at a specific byte offset.
    pub fn write_offset(&mut self, handle: u16, offset: usize, value: &[u8]) -> Result<(), u8> {
        let attr = self.check_write_permitted(handle)?;
        if attr.value.len() < offset + value.len() {
            attr.value.resize(offset + value.len(), 0);
        }
        attr.value[offset..offset + value.len()].copy_from_slice(value);
        Ok(())
    }

    /// Finds attributes within handle range `[start..=end]` matching `uuid`.
    /// An inverted range matches nothing (`BTreeMap::range` would panic).
    pub fn read_by_type(&self, start: u16, end: u16, uuid: Uuid) -> Vec<(u16, &[u8])> {
        if start > end {
            return Vec::new();
        }
        self.attributes
            .range(start..=end)
            .filter(|(_, attr)| attr.uuid == uuid && attr.permissions.read)
            .map(|(&h, attr)| (h, attr.value.as_slice()))
            .collect()
    }

    /// The last handle in the service group that begins at `service_handle`
    /// (Core Spec Vol 3, Part G, 2.5.2): every attribute up to — but not
    /// including — the next service declaration, or the end of the database.
    ///
    /// A Read By Group Type Response must carry this, not a blanket 0xFFFF:
    /// a client told that the first service spans the whole database will
    /// find every later service's characteristics inside it, and report the
    /// same characteristic once per service.
    pub fn group_end_handle(&self, service_handle: u16) -> u16 {
        let primary = Uuid::from(service_uuid::PRIMARY_SERVICE);
        let secondary = Uuid::from(service_uuid::SECONDARY_SERVICE);
        let mut end = service_handle;
        for (&handle, attr) in self.attributes.range((service_handle + 1)..) {
            if attr.uuid == primary || attr.uuid == secondary {
                break;
            }
            end = handle;
        }
        end
    }

    /// Finds information (Handle + UUID) within handle range `[start..=end]`.
    /// An inverted range matches nothing (`BTreeMap::range` would panic).
    pub fn find_information(&self, start: u16, end: u16) -> Vec<(u16, Uuid)> {
        if start > end {
            return Vec::new();
        }
        self.attributes
            .range(start..=end)
            .map(|(&h, attr)| (h, attr.uuid))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gatt_database_service_and_characteristic_creation() {
        let mut db = GattDatabase::new();

        let svc_handle = db.add_service(Uuid::from_u16(0x180D), true); // Heart Rate Service
        assert_eq!(svc_handle, 0x0001);

        let (decl_h, val_h) = db.add_characteristic(
            Uuid::from_u16(0x2A37), // Heart Rate Measurement
            CharacteristicProperties(CharacteristicProperties::NOTIFY),
            vec![0x00, 0x50],
            AttributePermissions::default(),
        );
        assert_eq!(decl_h, 0x0002);
        assert_eq!(val_h, 0x0003);

        let cccd_h = db.add_cccd();
        assert_eq!(cccd_h, 0x0004);

        // Read initial value
        let val = db.read(val_h, 0).expect("Read valid");
        assert_eq!(val, &[0x00, 0x50]);

        // Write new value
        db.write(val_h, &[0x00, 0x55]).expect("Write valid");
        assert_eq!(db.read(val_h, 0).unwrap(), &[0x00, 0x55]);
    }
}
