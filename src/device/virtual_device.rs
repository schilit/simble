// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Virtual Bluetooth Device state machine and packet processor.

use std::collections::HashMap;
use zerocopy::IntoBytes;

use crate::att::{AttHandleValueHeader, opcode};
use crate::device::bond_store::{BondSecurity, BondStore};
use crate::device::connection::{ConnectionRole, ConnectionState};
use crate::device::observer::{AttServerObserver, SubscriptionReason};
use crate::gap::AdvertisingData;
use crate::gatt::GattDatabase;
use crate::l2cap::{L2capHeader, cid};
use crate::smp::{PairingConfig, PairingSession, Role as SmpRole, opcode as smp_opcode};
use crate::types::{Address, AddressType, SimbleError, Uuid};

/// A simulated virtual Bluetooth device.
pub struct VirtualDevice {
    pub name: String,
    pub address: Address,
    pub address_type: AddressType,
    pub gatt_db: GattDatabase,
    pub advertising_data: Option<AdvertisingData>,
    pub is_advertising: bool,
    pub connections: HashMap<u16, ConnectionState>,
    pub default_mtu: u16,
    /// Optional hook for real ATT-server events (reads, writes, connection
    /// state changes, MTU changes, notifications/indications sent). Used by
    /// `crate::android` to make its callback types real, not cosmetic.
    pub observer: Option<Box<dyn AttServerObserver>>,
    /// Optional bonded-peer record store (NimBLE `ble_store` pattern).
    /// When present, completed bonding pairings and CCCD subscription
    /// state are recorded here, keyed by peer identity address, and
    /// subscriptions are restored when a bonded peer reconnects.
    pub bond_store: Option<Box<dyn BondStore>>,
}

/// The address a peer's bond records are keyed by: the identity address it
/// distributed during pairing (Core Spec Vol 3, Part H, Section 3.6.5), or
/// the connection address when none was.
fn bond_key_address(conn: &ConnectionState) -> Address {
    conn.pairing_session
        .as_ref()
        .and_then(|s| s.peer_identity_address())
        .map(|(_, addr)| addr)
        .unwrap_or(conn.peer_address)
}

impl std::fmt::Debug for VirtualDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VirtualDevice")
            .field("name", &self.name)
            .field("address", &self.address)
            .field("address_type", &self.address_type)
            .field("gatt_db", &self.gatt_db)
            .field("advertising_data", &self.advertising_data)
            .field("is_advertising", &self.is_advertising)
            .field("connections", &self.connections)
            .field("default_mtu", &self.default_mtu)
            .field("observer", &self.observer.is_some())
            .field("bond_store", &self.bond_store.is_some())
            .finish()
    }
}

impl Clone for VirtualDevice {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            address: self.address,
            address_type: self.address_type,
            gatt_db: self.gatt_db.clone(),
            advertising_data: self.advertising_data.clone(),
            is_advertising: self.is_advertising,
            connections: self.connections.clone(),
            default_mtu: self.default_mtu,
            // Trait objects aren't `Clone`; a cloned device starts unobserved
            // and unbonded.
            observer: None,
            bond_store: None,
        }
    }
}

impl VirtualDevice {
    /// Creates a new virtual device.
    pub fn new(name: impl Into<String>, address: Address, address_type: AddressType) -> Self {
        Self {
            name: name.into(),
            address,
            address_type,
            gatt_db: GattDatabase::new(),
            advertising_data: None,
            is_advertising: false,
            connections: HashMap::new(),
            default_mtu: 23,
            observer: None,
            bond_store: None,
        }
    }

    /// Handles a new connection event from the controller, assuming the
    /// Peripheral role — the only role this device played before
    /// per-connection roles existed, so existing callers keep their
    /// behavior. Central-role links use [`Self::on_connected_with_role`].
    pub fn on_connected(&mut self, handle: u16, peer_address: Address) {
        self.on_connected_with_role(handle, peer_address, ConnectionRole::Peripheral);
    }

    /// Handles a new connection event with an explicit local GAP role
    /// (per-connection, matching NimBLE's `ble_gap_conn_desc.role`).
    pub fn on_connected_with_role(
        &mut self,
        handle: u16,
        peer_address: Address,
        role: ConnectionRole,
    ) {
        self.connections.insert(
            handle,
            ConnectionState::new_with_role(handle, peer_address, self.default_mtu, role),
        );
        // Only a Peripheral was advertising to be connected to; a
        // Central-role connection says nothing about local advertising.
        if role == ConnectionRole::Peripheral {
            self.is_advertising = false;
        }
        if let Some(observer) = self.observer.as_deref_mut() {
            observer.on_connection_state_changed(handle, peer_address, true);
        }
        self.restore_bonded_subscriptions(handle, peer_address);
    }

    /// Looks up the active connection to `peer_address`, if any.
    pub fn connection_by_address(&self, peer_address: Address) -> Option<&ConnectionState> {
        self.connections
            .values()
            .find(|conn| conn.peer_address == peer_address)
    }

    /// Handles a disconnection event.
    pub fn on_disconnected(&mut self, handle: u16) {
        let removed = self.connections.remove(&handle);
        if removed.is_some() {
            // Core Spec Vol 3, Part G, Section 3.3.3.3: CCCD state is
            // per-client, so a disconnect ends the peer's subscriptions.
            // The database stores one live value per CCCD (not per-client),
            // so the sweep attributes all active subscriptions to the
            // disconnecting peer — exact for the single-connection case.
            // Bonded peers keep their state in the bond store and get it
            // back on reconnect via `restore_bonded_subscriptions`.
            for (cccd_handle, prev) in self.subscribed_cccds() {
                let _ = self.gatt_db.set_value(cccd_handle, &[0x00, 0x00]);
                self.notify_subscription(
                    handle,
                    cccd_handle,
                    prev,
                    0,
                    SubscriptionReason::Disconnect,
                );
            }
        }
        if let Some(observer) = self.observer.as_deref_mut() {
            observer.on_connection_state_changed(
                handle,
                removed.map(|c| c.peer_address).unwrap_or(Address::ANY),
                false,
            );
        }
    }

    /// Reads the current value of `handle` as a CCCD bitfield, or `None` if
    /// the attribute isn't a CCCD (0x2902).
    pub(crate) fn cccd_value(&self, handle: u16) -> Option<u16> {
        let attr = self.gatt_db.attributes.get(&handle)?;
        if attr.uuid != Uuid::CCCD {
            return None;
        }
        Some(u16::from_le_bytes([
            attr.value.first().copied().unwrap_or(0),
            attr.value.get(1).copied().unwrap_or(0),
        ]))
    }

    /// All CCCDs with any subscription bit currently set, collected up
    /// front so callers can then mutate `self` (bond store, database)
    /// without an outstanding database borrow.
    fn subscribed_cccds(&self) -> Vec<(u16, u16)> {
        self.gatt_db
            .attributes
            .keys()
            .filter_map(|&h| self.cccd_value(h).map(|v| (h, v)))
            .filter(|&(_, v)| v != 0)
            .collect()
    }

    /// Fires the observer's subscription event for a CCCD bit transition
    /// (NimBLE `BLE_GAP_EVENT_SUBSCRIBE` pattern). Silent when no bit
    /// actually changed.
    pub(crate) fn notify_subscription(
        &mut self,
        connection_handle: u16,
        cccd_handle: u16,
        prev: u16,
        cur: u16,
        reason: SubscriptionReason,
    ) {
        if prev == cur {
            return;
        }
        // CCCD bit assignments per Core Spec Vol 3, Part G, Section 3.3.3.3.
        const NOTIFY: u16 = 0x0001;
        const INDICATE: u16 = 0x0002;
        if let Some(observer) = self.observer.as_deref_mut() {
            observer.on_subscription_changed(
                connection_handle,
                cccd_handle,
                prev & NOTIFY != 0,
                cur & NOTIFY != 0,
                prev & INDICATE != 0,
                cur & INDICATE != 0,
                reason,
            );
        }
    }

    /// Completes a CCCD write from `connection_handle`: reports the bit
    /// transition and persists the new state for a bonded peer.
    pub(crate) fn on_cccd_written(&mut self, connection_handle: u16, cccd_handle: u16, prev: u16) {
        let cur = self.cccd_value(cccd_handle).unwrap_or(prev);
        self.notify_subscription(
            connection_handle,
            cccd_handle,
            prev,
            cur,
            SubscriptionReason::Write,
        );
        let Some(conn) = self.connections.get(&connection_handle) else {
            return;
        };
        let peer = bond_key_address(conn);
        if let Some(store) = self.bond_store.as_deref_mut()
            && store.is_bonded(peer)
        {
            store.store_cccd(peer, cccd_handle, cur);
        }
    }

    /// Restores a bonded peer's persisted CCCD subscriptions into the live
    /// database on reconnect, reporting each with
    /// [`SubscriptionReason::BondRestore`].
    fn restore_bonded_subscriptions(&mut self, handle: u16, peer_address: Address) {
        let restored = match self.bond_store.as_deref() {
            Some(store) if store.is_bonded(peer_address) => store.load_cccds(peer_address),
            _ => return,
        };
        for (cccd_handle, value) in restored {
            let prev = self.cccd_value(cccd_handle).unwrap_or(0);
            if self
                .gatt_db
                .set_value(cccd_handle, &value.to_le_bytes())
                .is_ok()
            {
                self.notify_subscription(
                    handle,
                    cccd_handle,
                    prev,
                    value,
                    SubscriptionReason::BondRestore,
                );
            }
        }
    }

    /// Records a completed bonding pairing into the bond store, keyed by
    /// the peer's distributed identity address (falling back to the
    /// connection address when no identity was distributed). Idempotent —
    /// `store_security` replaces — so it's safe to call after every SMP
    /// PDU without tracking a "recorded" flag.
    fn maybe_record_bond(&mut self, connection_handle: u16) {
        let Some(conn) = self.connections.get(&connection_handle) else {
            return;
        };
        let Some(session) = conn.pairing_session.as_ref() else {
            return;
        };
        if !session.is_complete() || !session.is_bonding() {
            return;
        }
        let peer = bond_key_address(conn);
        let security = BondSecurity {
            keys: session.pairing_keys(),
            secure_connections: session.is_secure_connections(),
            authenticated: session.is_authenticated(),
            key_size: session.encryption_key_size(),
        };
        // Collected before borrowing the store mutably.
        let subscribed = self.subscribed_cccds();
        let Some(store) = self.bond_store.as_deref_mut() else {
            return;
        };
        store.store_security(peer, security);
        // Sweep subscriptions made before the bond completed
        // (subscribe-then-pair) so they survive the disconnect too.
        for (cccd_handle, value) in subscribed {
            store.store_cccd(peer, cccd_handle, value);
        }
    }

    /// Processes an incoming L2CAP packet received over an ACL connection.
    ///
    /// Returns an optional response L2CAP packet to send back.
    pub fn process_l2cap_packet(
        &mut self,
        connection_handle: u16,
        raw_l2cap: &[u8],
    ) -> Result<Option<Vec<u8>>, SimbleError> {
        let (header, payload) = L2capHeader::parse(raw_l2cap)
            .ok_or_else(|| SimbleError::PacketParseError("Invalid L2CAP frame".into()))?;

        match header.cid.get() {
            cid::ATT => {
                let resp_payload = self.process_att_packet(connection_handle, payload)?;
                Ok(resp_payload.map(|p| L2capHeader::serialize(cid::ATT, &p)))
            }
            cid::SMP => {
                let resp_payload = self.process_smp_packet(connection_handle, payload)?;
                Ok(resp_payload.map(|p| L2capHeader::serialize(cid::SMP, &p)))
            }
            _ => Ok(None),
        }
    }

    /// Dispatches an incoming SMP PDU to the connection's [`PairingSession`],
    /// creating a responder-role session on the fly if a Pairing Request
    /// arrives on a connection with none yet (mirroring how a real peripheral
    /// accepts pairing initiated by the peer).
    fn process_smp_packet(
        &mut self,
        connection_handle: u16,
        pdu: &[u8],
    ) -> Result<Option<Vec<u8>>, SimbleError> {
        let conn = self
            .connections
            .get_mut(&connection_handle)
            .ok_or_else(|| {
                SimbleError::DeviceError(format!("Connection {connection_handle} not found"))
            })?;

        if conn.pairing_session.is_none() {
            if pdu.first().copied() != Some(smp_opcode::PAIRING_REQUEST) {
                return Ok(None);
            }
            conn.pairing_session = Some(PairingSession::new(
                SmpRole::Responder,
                PairingConfig::default(),
                self.address,
                self.address_type,
                conn.peer_address,
                AddressType::Random,
            ));
        }

        let session = conn.pairing_session.as_mut().expect("just ensured Some");
        let reply = session.handle_pdu(pdu)?;
        conn.is_encrypted = session.is_encrypted();
        conn.ltk = session.ltk();
        self.maybe_record_bond(connection_handle);
        Ok(reply)
    }

    /// Starts SMP pairing as the initiator on an existing connection,
    /// returning the L2CAP-framed Pairing Request to send.
    pub fn start_pairing(&mut self, connection_handle: u16) -> Result<Vec<u8>, SimbleError> {
        self.start_pairing_with_config(connection_handle, PairingConfig::default())
    }

    /// Same as [`Self::start_pairing`], but with an explicit [`PairingConfig`]
    /// (e.g. to force LE Legacy pairing by setting `sc: false`, or to enable
    /// `debug_mode`).
    pub fn start_pairing_with_config(
        &mut self,
        connection_handle: u16,
        config: PairingConfig,
    ) -> Result<Vec<u8>, SimbleError> {
        let conn = self
            .connections
            .get_mut(&connection_handle)
            .ok_or_else(|| {
                SimbleError::DeviceError(format!("Connection {connection_handle} not found"))
            })?;
        let mut session = PairingSession::new(
            SmpRole::Initiator,
            config,
            self.address,
            self.address_type,
            conn.peer_address,
            AddressType::Random,
        );
        let request = session.start();
        conn.pairing_session = Some(session);
        Ok(L2capHeader::serialize(cid::SMP, &request))
    }

    /// Drains one more queued outgoing SMP PDU from the connection's
    /// [`PairingSession`] (e.g. the follow-on PDUs of a multi-PDU key
    /// distribution phase), framed for L2CAP. Returns `None` once drained.
    pub fn poll_smp_pdu(&mut self, connection_handle: u16) -> Option<Vec<u8>> {
        let conn = self.connections.get_mut(&connection_handle)?;
        let session = conn.pairing_session.as_mut()?;
        let pdu = session.poll_pending()?;
        conn.is_encrypted = session.is_encrypted();
        conn.ltk = session.ltk();
        self.maybe_record_bond(connection_handle);
        Some(L2capHeader::serialize(cid::SMP, &pdu))
    }

    /// Creates an ATT Handle Value Notification packet for a characteristic update.
    ///
    /// This helper has no connection context of its own (the resulting PDU is
    /// meant to be broadcast to whichever connections have notifications
    /// enabled), so the observer sees a sentinel `0x0000` connection handle.
    /// Callers that know the target connection should use
    /// [`Self::create_notification_for`] instead.
    pub fn create_notification(&mut self, handle: u16, value: &[u8]) -> Vec<u8> {
        self.create_notification_for(0x0000, handle, value)
    }

    /// Same as [`Self::create_notification`], but tags the completion event
    /// with a real connection handle.
    pub fn create_notification_for(
        &mut self,
        connection_handle: u16,
        handle: u16,
        value: &[u8],
    ) -> Vec<u8> {
        let mut pdu = Vec::with_capacity(3 + value.len());
        let header = AttHandleValueHeader::new(opcode::HANDLE_VALUE_NTF, handle);
        pdu.extend_from_slice(header.as_bytes());
        pdu.extend_from_slice(value);
        if let Some(observer) = self.observer.as_deref_mut() {
            observer.on_notification_sent(connection_handle, handle, false);
        }
        L2capHeader::serialize(cid::ATT, &pdu)
    }

    /// Creates an ATT Handle Value Indication packet (which requires confirmation from central).
    pub fn create_indication(
        &mut self,
        connection_handle: u16,
        handle: u16,
        value: &[u8],
    ) -> Result<Vec<u8>, SimbleError> {
        let conn = self
            .connections
            .get_mut(&connection_handle)
            .ok_or_else(|| {
                SimbleError::DeviceError(format!("Connection {connection_handle} not found"))
            })?;

        if conn.pending_indication {
            return Err(SimbleError::DeviceError(
                "Indication already in flight, waiting for confirmation".into(),
            ));
        }

        conn.pending_indication = true;

        let mut pdu = Vec::with_capacity(3 + value.len());
        let header = AttHandleValueHeader::new(opcode::HANDLE_VALUE_IND, handle);
        pdu.extend_from_slice(header.as_bytes());
        pdu.extend_from_slice(value);
        if let Some(observer) = self.observer.as_deref_mut() {
            observer.on_notification_sent(connection_handle, handle, true);
        }
        Ok(L2capHeader::serialize(cid::ATT, &pdu))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::att::AttExecuteWriteReq;
    use crate::gatt::{AttributePermissions, CharacteristicProperties};
    use crate::types::Uuid;

    #[test]
    fn test_virtual_device_mtu_and_read_write_flow() {
        let addr = Address::from_be_bytes([0xF0, 0xDE, 0xF1, 0x22, 0x33, 0x44]);
        let mut dev = VirtualDevice::new("HRM", addr, AddressType::Random);

        let (_, val_handle) = dev.gatt_db.add_characteristic(
            Uuid::from_u16(0x2A37),
            CharacteristicProperties(
                CharacteristicProperties::READ | CharacteristicProperties::WRITE,
            ),
            vec![0x00, 0x40],
            AttributePermissions::default(),
        );

        let conn_handle = 0x0040;
        dev.on_connected(conn_handle, Address::ANY);

        // 1. Exchange MTU
        let mtu_req = [opcode::EXCHANGE_MTU_REQ, 0x00, 0x02];
        let l2cap_req = L2capHeader::serialize(cid::ATT, &mtu_req);
        let resp = dev
            .process_l2cap_packet(conn_handle, &l2cap_req)
            .unwrap()
            .unwrap();
        let (header, resp_payload) = L2capHeader::parse(&resp).unwrap();
        assert_eq!(header.cid.get(), cid::ATT);
        assert_eq!(resp_payload[0], opcode::EXCHANGE_MTU_RSP);

        // 2. Read Characteristic
        let mut read_req = vec![opcode::READ_REQ];
        read_req.extend_from_slice(&val_handle.to_le_bytes());
        let l2cap_read = L2capHeader::serialize(cid::ATT, &read_req);
        let resp = dev
            .process_l2cap_packet(conn_handle, &l2cap_read)
            .unwrap()
            .unwrap();
        let (_, read_resp) = L2capHeader::parse(&resp).unwrap();
        assert_eq!(read_resp[0], opcode::READ_RSP);
        assert_eq!(&read_resp[1..], &[0x00, 0x40]);

        // 3. Write Characteristic
        let mut write_req = vec![opcode::WRITE_REQ];
        write_req.extend_from_slice(&val_handle.to_le_bytes());
        write_req.extend_from_slice(&[0x00, 0x55]);
        let l2cap_write = L2capHeader::serialize(cid::ATT, &write_req);
        let resp = dev
            .process_l2cap_packet(conn_handle, &l2cap_write)
            .unwrap()
            .unwrap();
        let (_, write_resp) = L2capHeader::parse(&resp).unwrap();
        assert_eq!(write_resp[0], opcode::WRITE_RSP);

        // 4. Verify new value
        let val = dev.gatt_db.read(val_handle, 0).unwrap();
        assert_eq!(val, &[0x00, 0x55]);

        // 5. Generate notification
        let notif = dev.create_notification(val_handle, &[0x00, 0x60]);
        let (_, notif_payload) = L2capHeader::parse(&notif).unwrap();
        assert_eq!(notif_payload[0], opcode::HANDLE_VALUE_NTF);
        assert_eq!(&notif_payload[3..], &[0x00, 0x60]);

        // 6. Indication & Confirmation flow
        let ind = dev
            .create_indication(conn_handle, val_handle, &[0x00, 0x70])
            .unwrap();
        let (_, ind_payload) = L2capHeader::parse(&ind).unwrap();
        assert_eq!(ind_payload[0], opcode::HANDLE_VALUE_IND);

        // Second indication should fail until confirmation is received
        assert!(
            dev.create_indication(conn_handle, val_handle, &[0x00, 0x71])
                .is_err()
        );

        // Send confirmation
        let cfm_pdu = [opcode::HANDLE_VALUE_CFM];
        let cfm_l2cap = L2capHeader::serialize(cid::ATT, &cfm_pdu);
        assert!(
            dev.process_l2cap_packet(conn_handle, &cfm_l2cap)
                .unwrap()
                .is_none()
        );

        // Now next indication succeeds
        assert!(
            dev.create_indication(conn_handle, val_handle, &[0x00, 0x71])
                .is_ok()
        );
    }

    #[test]
    fn test_prepare_and_execute_write_flow() {
        let addr = Address::from_be_bytes([0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC]);
        let mut dev = VirtualDevice::new("LongWriter", addr, AddressType::Random);

        let (_, val_handle) = dev.gatt_db.add_characteristic(
            Uuid::from_u16(0x2A00),
            CharacteristicProperties(CharacteristicProperties::WRITE),
            vec![0u8; 10],
            AttributePermissions::default(),
        );

        let conn_h = 0x0001;
        dev.on_connected(conn_h, Address::ANY);

        // Prepare chunk 1: offset 0, bytes [1, 2, 3]
        let mut prep1 = vec![opcode::PREPARE_WRITE_REQ];
        prep1.extend_from_slice(&val_handle.to_le_bytes());
        prep1.extend_from_slice(&0u16.to_le_bytes()); // offset 0
        prep1.extend_from_slice(&[1, 2, 3]);
        let l2cap_prep1 = L2capHeader::serialize(cid::ATT, &prep1);
        let resp1 = dev
            .process_l2cap_packet(conn_h, &l2cap_prep1)
            .unwrap()
            .unwrap();
        let (_, payload1) = L2capHeader::parse(&resp1).unwrap();
        assert_eq!(payload1[0], opcode::PREPARE_WRITE_RSP);

        // Prepare chunk 2: offset 3, bytes [4, 5, 6]
        let mut prep2 = vec![opcode::PREPARE_WRITE_REQ];
        prep2.extend_from_slice(&val_handle.to_le_bytes());
        prep2.extend_from_slice(&3u16.to_le_bytes()); // offset 3
        prep2.extend_from_slice(&[4, 5, 6]);
        let l2cap_prep2 = L2capHeader::serialize(cid::ATT, &prep2);
        let _ = dev
            .process_l2cap_packet(conn_h, &l2cap_prep2)
            .unwrap()
            .unwrap();

        // Execute Write
        let exec = [opcode::EXECUTE_WRITE_REQ, AttExecuteWriteReq::WRITE];
        let l2cap_exec = L2capHeader::serialize(cid::ATT, &exec);
        let exec_resp = dev
            .process_l2cap_packet(conn_h, &l2cap_exec)
            .unwrap()
            .unwrap();
        let (_, exec_payload) = L2capHeader::parse(&exec_resp).unwrap();
        assert_eq!(exec_payload[0], opcode::EXECUTE_WRITE_RSP);

        // Check value in GATT DB
        let val = dev.gatt_db.read(val_handle, 0).unwrap();
        assert_eq!(&val[0..6], &[1, 2, 3, 4, 5, 6]);
    }
}
