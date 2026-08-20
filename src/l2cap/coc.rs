// Copyright 2026 The Android Open Source Project
// SPDX-License-Identifier: Apache-2.0

//! L2CAP Connection-Oriented Channels (CoC) and Credit-Based Flow Control.

use crate::types::SimbleError;
use std::collections::HashMap;

/// An active L2CAP Connection-Oriented Channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoCChannel {
    pub cid: u16,
    pub peer_cid: u16,
    pub spsm: u16,
    pub mtu: u16,
    pub mps: u16,
    pub local_credits: u16,
    pub peer_credits: u16,
}

impl CoCChannel {
    pub fn new(
        cid: u16,
        peer_cid: u16,
        spsm: u16,
        mtu: u16,
        mps: u16,
        initial_local_credits: u16,
        initial_peer_credits: u16,
    ) -> Self {
        Self {
            cid,
            peer_cid,
            spsm,
            mtu,
            mps,
            local_credits: initial_local_credits,
            peer_credits: initial_peer_credits,
        }
    }

    /// Checks if we have available credits to transmit an SDU fragment to the peer.
    pub fn can_send(&self) -> bool {
        self.peer_credits > 0
    }

    /// Consumes one transmit credit before sending an L2CAP packet to peer.
    pub fn consume_tx_credit(&mut self) -> Result<(), SimbleError> {
        if self.peer_credits == 0 {
            return Err(SimbleError::DeviceError(
                "L2CAP CoC: Insufficient peer credits".into(),
            ));
        }
        self.peer_credits -= 1;
        Ok(())
    }

    /// Adds credits granted by the peer via an LE Flow Control Credit packet.
    pub fn add_tx_credits(&mut self, credits: u16) {
        self.peer_credits = self.peer_credits.saturating_add(credits);
    }

    /// Adds local credits after receiving packets to allow peer to send more.
    pub fn add_rx_credits(&mut self, credits: u16) {
        self.local_credits = self.local_credits.saturating_add(credits);
    }
}

/// Dynamic CID allocator and channel manager for L2CAP CoC / EATT bearers.
#[derive(Debug, Default)]
pub struct CoCManager {
    next_cid: u16,
    channels: HashMap<u16, CoCChannel>,
}

impl CoCManager {
    pub const MIN_DYNAMIC_CID: u16 = 0x0040;
    pub const MAX_DYNAMIC_CID: u16 = 0x007F;

    pub fn new() -> Self {
        Self {
            next_cid: Self::MIN_DYNAMIC_CID,
            channels: HashMap::new(),
        }
    }

    /// Allocates the next dynamic CID and registers the channel.
    pub fn create_channel(
        &mut self,
        peer_cid: u16,
        spsm: u16,
        mtu: u16,
        mps: u16,
        initial_credits: u16,
    ) -> Result<u16, SimbleError> {
        let cid = self.allocate_cid()?;
        let channel = CoCChannel::new(
            cid,
            peer_cid,
            spps_or_spsm(spsm),
            mtu,
            mps,
            initial_credits,
            initial_credits,
        );
        self.channels.insert(cid, channel);
        Ok(cid)
    }

    fn allocate_cid(&mut self) -> Result<u16, SimbleError> {
        for _ in 0..(Self::MAX_DYNAMIC_CID - Self::MIN_DYNAMIC_CID + 1) {
            let cid = self.next_cid;
            self.next_cid = if self.next_cid >= Self::MAX_DYNAMIC_CID {
                Self::MIN_DYNAMIC_CID
            } else {
                self.next_cid + 1
            };

            if !self.channels.contains_key(&cid) {
                return Ok(cid);
            }
        }
        Err(SimbleError::DeviceError(
            "L2CAP CoC: All dynamic CIDs exhausted".into(),
        ))
    }

    pub fn get_channel(&self, cid: u16) -> Option<&CoCChannel> {
        self.channels.get(&cid)
    }

    pub fn get_channel_mut(&mut self, cid: u16) -> Option<&mut CoCChannel> {
        self.channels.get_mut(&cid)
    }

    pub fn remove_channel(&mut self, cid: u16) -> Option<CoCChannel> {
        self.channels.remove(&cid)
    }
}

fn spps_or_spsm(s: u16) -> u16 {
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_coc_channel_flow_control() {
        let mut channel = CoCChannel::new(0x0040, 0x0045, 0x0025, 512, 251, 10, 2);
        assert!(channel.can_send());
        assert_eq!(channel.peer_credits, 2);

        channel.consume_tx_credit().unwrap();
        assert_eq!(channel.peer_credits, 1);

        channel.consume_tx_credit().unwrap();
        assert_eq!(channel.peer_credits, 0);
        assert!(!channel.can_send());

        assert!(channel.consume_tx_credit().is_err());

        // Grant 5 more credits
        channel.add_tx_credits(5);
        assert_eq!(channel.peer_credits, 5);
        assert!(channel.can_send());
    }

    #[test]
    fn test_coc_manager_allocation() {
        let mut mgr = CoCManager::new();
        let cid1 = mgr.create_channel(0x0040, 0x0025, 512, 251, 10).unwrap();
        let cid2 = mgr.create_channel(0x0041, 0x0025, 512, 251, 10).unwrap();

        assert_eq!(cid1, 0x0040);
        assert_eq!(cid2, 0x0041);
        assert!(mgr.get_channel(cid1).is_some());

        mgr.remove_channel(cid1);
        assert!(mgr.get_channel(cid1).is_none());
    }
}
