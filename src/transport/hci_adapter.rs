// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! In-memory asynchronous HCI transport bridging Simble Virtual Devices with Rootcanal.
//!
//! Exposes bidirectional packet channels compatible with Netsim's PacketStream and PacketSink
//! without requiring network sockets or IPC serialization overhead.

use crate::types::SimbleError;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender, channel};

/// H4 Packet Types (Bluetooth Core Specification Vol 4, Part A)
pub mod h4_type {
    pub const HCI_COMMAND: u8 = 0x01;
    pub const HCI_ACL_DATA: u8 = 0x02;
    pub const HCI_SCO_DATA: u8 = 0x03;
    pub const HCI_EVENT: u8 = 0x04;
    pub const HCI_ISO_DATA: u8 = 0x05;
}

/// In-memory bidirectional HCI channel pair connecting Simble Host to Rootcanal Controller.
pub struct HciChannel {
    /// Outgoing channel (Host -> Controller)
    pub host_to_ctrl_tx: Sender<Vec<u8>>,
    pub host_to_ctrl_rx: Mutex<Receiver<Vec<u8>>>,

    /// Incoming channel (Controller -> Host)
    pub ctrl_to_host_tx: Sender<Vec<u8>>,
    pub ctrl_to_host_rx: Mutex<Receiver<Vec<u8>>>,
}

impl HciChannel {
    /// Creates a new paired in-memory HCI transport channel.
    pub fn new() -> Self {
        let (host_to_ctrl_tx, host_to_ctrl_rx) = channel();
        let (ctrl_to_host_tx, ctrl_to_host_rx) = channel();

        Self {
            host_to_ctrl_tx,
            host_to_ctrl_rx: Mutex::new(host_to_ctrl_rx),
            ctrl_to_host_tx,
            ctrl_to_host_rx: Mutex::new(ctrl_to_host_rx),
        }
    }

    fn send_h4(&self, h4_type: u8, payload: &[u8]) -> Result<(), SimbleError> {
        let mut h4_packet = Vec::with_capacity(1 + payload.len());
        h4_packet.push(h4_type);
        h4_packet.extend_from_slice(payload);
        self.host_to_ctrl_tx
            .send(h4_packet)
            .map_err(|e| SimbleError::Transport(e.to_string()))
    }

    /// Sends an HCI Command packet (prefixed with H4 byte 0x01) to the Controller.
    pub fn send_command(&self, cmd: &[u8]) -> Result<(), SimbleError> {
        self.send_h4(h4_type::HCI_COMMAND, cmd)
    }

    /// Sends an HCI ACL Data packet (prefixed with H4 byte 0x02) to the Controller.
    pub fn send_acl_data(&self, acl: &[u8]) -> Result<(), SimbleError> {
        self.send_h4(h4_type::HCI_ACL_DATA, acl)
    }

    /// Polls for the next H4 packet from Host to Controller (non-blocking).
    pub fn poll_host_packet(&self) -> Option<Vec<u8>> {
        let rx = self.host_to_ctrl_rx.lock().ok()?;
        rx.try_recv().ok()
    }

    /// Injects an H4 packet received from the Controller (Event 0x04 or ACL Data 0x02) to Host.
    pub fn receive_from_controller(&self, h4_packet: Vec<u8>) -> Result<(), SimbleError> {
        self.ctrl_to_host_tx
            .send(h4_packet)
            .map_err(|e| SimbleError::Transport(e.to_string()))
    }

    /// Polls for the next H4 packet from Controller to Host (non-blocking).
    pub fn poll_controller_packet(&self) -> Option<Vec<u8>> {
        let rx = self.ctrl_to_host_rx.lock().ok()?;
        rx.try_recv().ok()
    }
}

impl Default for HciChannel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hci_channel_command_and_event_routing() {
        let channel = HciChannel::new();

        // 1. Host sends HCI Reset Command (OpCode: 0x0C03, Param Len: 0)
        let reset_cmd = [0x03, 0x0C, 0x00];
        channel.send_command(&reset_cmd).unwrap();

        let host_pkt = channel.poll_host_packet().expect("packet available");
        assert_eq!(host_pkt[0], h4_type::HCI_COMMAND);
        assert_eq!(&host_pkt[1..], &reset_cmd);

        // 2. Controller responds with Command Complete Event
        let cmd_complete_evt = [h4_type::HCI_EVENT, 0x0E, 0x04, 0x01, 0x03, 0x0C, 0x00];
        channel
            .receive_from_controller(cmd_complete_evt.to_vec())
            .unwrap();

        let ctrl_pkt = channel.poll_controller_packet().expect("event available");
        assert_eq!(ctrl_pkt, cmd_complete_evt);
    }
}
