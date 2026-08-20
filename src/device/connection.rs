// Copyright 2026 The Android Open Source Project
// SPDX-License-Identifier: Apache-2.0

//! LE Connection state tracking and write buffer queue.

use crate::types::Address;

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
    pub mtu: u16,
    pub prepare_write_queue: Vec<PrepareWriteChunk>,
    pub pending_indication: bool,
}

impl ConnectionState {
    /// Creates a new connection state.
    pub fn new(handle: u16, peer_address: Address, mtu: u16) -> Self {
        Self {
            handle,
            peer_address,
            mtu,
            prepare_write_queue: Vec::new(),
            pending_indication: false,
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
        assert!(conn.prepare_write_queue.is_empty());
        assert!(!conn.pending_indication);
    }
}
