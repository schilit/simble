// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! LE Connection state tracking and write buffer queue.

use crate::smp::PairingSession;
use crate::types::Address;

/// Which GAP role the local device plays on one connection. Per-connection
/// rather than device-level, matching NimBLE's `ble_gap_conn_desc.role` — a
/// device can be Central on one link and Peripheral on another
/// simultaneously.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionRole {
    Central,
    Peripheral,
}

/// Buffered chunk for queued long writes (PrepareWrite / ExecuteWrite).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrepareWriteChunk {
    pub handle: u16,
    pub offset: u16,
    pub data: Vec<u8>,
}

/// State of an active LE connection.
#[derive(Debug, Clone)]
pub struct ConnectionState {
    pub handle: u16,
    pub peer_address: Address,
    /// The local device's GAP role on this connection.
    pub role: ConnectionRole,
    pub mtu: u16,
    pub prepare_write_queue: Vec<PrepareWriteChunk>,
    pub pending_indication: bool,
    /// The in-progress or completed SMP pairing exchange for this
    /// connection, if pairing has been initiated by either side.
    pub pairing_session: Option<PairingSession>,
    /// Whether the link is currently secured with an LTK/STK (set once a
    /// pairing session's Confirm/Random or DHKey Check exchange completes).
    pub is_encrypted: bool,
    /// The key currently securing the link, mirrored from
    /// `pairing_session` once encryption is established.
    pub ltk: Option<[u8; 16]>,
}

impl ConnectionState {
    /// Creates a new connection state in the Peripheral role — the only
    /// role `VirtualDevice` played before roles existed, so pre-role
    /// callers keep their behavior unchanged.
    pub fn new(handle: u16, peer_address: Address, mtu: u16) -> Self {
        Self::new_with_role(handle, peer_address, mtu, ConnectionRole::Peripheral)
    }

    /// Creates a new connection state with an explicit local role.
    pub fn new_with_role(
        handle: u16,
        peer_address: Address,
        mtu: u16,
        role: ConnectionRole,
    ) -> Self {
        Self {
            handle,
            peer_address,
            role,
            mtu,
            prepare_write_queue: Vec::new(),
            pending_indication: false,
            pairing_session: None,
            is_encrypted: false,
            ltk: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connection_state_initialization() {
        let addr = Address::from_be_bytes([1, 2, 3, 4, 5, 6]);
        let conn = ConnectionState::new(0x0040, addr, 23);
        assert_eq!(conn.handle, 0x0040);
        assert_eq!(conn.mtu, 23);
        assert_eq!(conn.role, ConnectionRole::Peripheral);
        assert!(conn.prepare_write_queue.is_empty());
        assert!(!conn.pending_indication);
        assert!(conn.pairing_session.is_none());
        assert!(!conn.is_encrypted);
        assert!(conn.ltk.is_none());
    }
}
