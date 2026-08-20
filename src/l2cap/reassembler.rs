// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Multi-fragment HCI ACL data packet reassembly engine for L2CAP frames.

use crate::types::SimbleError;
use std::collections::HashMap;

/// Manages reassembly of fragmented L2CAP frames arriving over HCI ACL packets.
#[derive(Debug, Default, Clone)]
pub struct AclReassembler {
    buffers: HashMap<u16, (usize, Vec<u8>)>, // (expected_total_len, accumulated_bytes)
}

impl AclReassembler {
    /// Creates a new ACL reassembler.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds an incoming ACL payload fragment for the given connection handle.
    ///
    /// Returns `Some(complete_l2cap_frame)` when the final fragment arrives.
    pub fn push_fragment(
        &mut self,
        handle: u16,
        is_first: bool,
        fragment: &[u8],
    ) -> Result<Option<Vec<u8>>, SimbleError> {
        if is_first {
            if fragment.len() < 4 {
                return Err(SimbleError::PacketParseError(
                    "First ACL fragment too short to contain L2CAP header".into(),
                ));
            }

            let len = u16::from_le_bytes([fragment[0], fragment[1]]) as usize;
            let expected_total_len = 4 + len;

            if fragment.len() >= expected_total_len {
                self.buffers.remove(&handle);
                return Ok(Some(fragment[..expected_total_len].to_vec()));
            }

            let mut buf = Vec::with_capacity(expected_total_len);
            buf.extend_from_slice(fragment);
            self.buffers.insert(handle, (expected_total_len, buf));
            Ok(None)
        } else {
            let (expected_total_len, buf) = self.buffers.get_mut(&handle).ok_or_else(|| {
                SimbleError::PacketParseError(format!(
                    "Received continuation fragment for connection {handle:#06x} without first fragment"
                ))
            })?;

            buf.extend_from_slice(fragment);

            if buf.len() >= *expected_total_len {
                let complete = buf[..*expected_total_len].to_vec();
                self.buffers.remove(&handle);
                Ok(Some(complete))
            } else {
                Ok(None)
            }
        }
    }

    /// Clears any in-progress reassembly buffer for a disconnected handle.
    pub fn on_disconnected(&mut self, handle: u16) {
        self.buffers.remove(&handle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::l2cap::{L2capHeader, cid};

    #[test]
    fn test_single_packet_reassembly() {
        let mut reassembler = AclReassembler::new();
        let payload = [0x01, 0x02, 0x03];
        let l2cap = L2capHeader::serialize(cid::ATT, &payload);

        let result = reassembler.push_fragment(0x0001, true, &l2cap).unwrap();
        assert_eq!(result, Some(l2cap));
    }

    #[test]
    fn test_multi_fragment_reassembly() {
        let mut reassembler = AclReassembler::new();
        let payload = vec![0xEE; 100];
        let l2cap = L2capHeader::serialize(cid::ATT, &payload);

        // Split into 3 fragments: 20 bytes, 50 bytes, 34 bytes (total 104)
        let frag1 = &l2cap[0..20];
        let frag2 = &l2cap[20..70];
        let frag3 = &l2cap[70..104];

        assert_eq!(
            reassembler.push_fragment(0x0001, true, frag1).unwrap(),
            None
        );
        assert_eq!(
            reassembler.push_fragment(0x0001, false, frag2).unwrap(),
            None
        );
        let completed = reassembler.push_fragment(0x0001, false, frag3).unwrap();

        assert_eq!(completed, Some(l2cap));
    }
}
