// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Shared dynamic-CID ring allocator and channel table, used by both the
//! Classic (`ClassicChannelManager`) and LE CoC (`CoCManager`) managers.

use std::collections::HashMap;

/// Ring allocator over a dynamic CID range, plus the CID -> channel table it
/// guards. [`CidAllocator::allocate`] scans forward from the last handed-out
/// CID (wrapping) and never returns a CID still present in the table.
#[derive(Debug)]
pub(crate) struct CidAllocator<T> {
    min_cid: u16,
    max_cid: u16,
    next_cid: u16,
    channels: HashMap<u16, T>,
}

impl<T> CidAllocator<T> {
    /// Creates an allocator over the inclusive dynamic CID range `min_cid..=max_cid`.
    pub fn new(min_cid: u16, max_cid: u16) -> Self {
        Self {
            min_cid,
            max_cid,
            next_cid: min_cid,
            channels: HashMap::new(),
        }
    }

    /// Allocates the next free CID, or `None` when the range is exhausted.
    pub fn allocate(&mut self) -> Option<u16> {
        for _ in 0..=(self.max_cid - self.min_cid) {
            let cid = self.next_cid;
            self.next_cid = if self.next_cid >= self.max_cid {
                self.min_cid
            } else {
                self.next_cid + 1
            };
            if !self.channels.contains_key(&cid) {
                return Some(cid);
            }
        }
        None
    }

    /// Registers `channel` under the given `cid` in the channel table.
    pub fn insert(&mut self, cid: u16, channel: T) {
        self.channels.insert(cid, channel);
    }

    /// Returns a shared reference to the channel registered under `cid`.
    pub fn get(&self, cid: u16) -> Option<&T> {
        self.channels.get(&cid)
    }

    /// Returns a mutable reference to the channel registered under `cid`.
    pub fn get_mut(&mut self, cid: u16) -> Option<&mut T> {
        self.channels.get_mut(&cid)
    }

    /// Removes and returns the channel registered under `cid`.
    pub fn remove(&mut self, cid: u16) -> Option<T> {
        self.channels.remove(&cid)
    }
}
