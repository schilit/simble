// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! In-memory asynchronous HCI transport bridging Simble Virtual Devices with Rootcanal.
//!
//! Exposes bidirectional packet channels compatible with Netsim's PacketStream and PacketSink
//! without requiring network sockets or IPC serialization overhead.

use crate::types::SimbleError;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender, channel};

/// H4 Packet Types (Bluetooth Core Specification Vol 4, Part A)
pub mod h4_type {
    /// HCI Command packet type (0x01).
    pub const HCI_COMMAND: u8 = 0x01;
    /// HCI ACL Data packet type (0x02).
    pub const HCI_ACL_DATA: u8 = 0x02;
    /// HCI Synchronous (SCO) Data packet type (0x03).
    pub const HCI_SCO_DATA: u8 = 0x03;
    /// HCI Event packet type (0x04).
    pub const HCI_EVENT: u8 = 0x04;
    /// HCI Isochronous (ISO) Data packet type (0x05).
    pub const HCI_ISO_DATA: u8 = 0x05;
}

/// HCI command flow control: the host's side of the budget a controller
/// grants it (Core Spec Vol 4, Part E, Section 4.4).
///
/// **The failure this prevents is silent.** A controller states, in every
/// Command Complete and Command Status, how many command packets the host may
/// have outstanding — `Num_HCI_Command_Packets`, byte 2 of a Command
/// Complete's parameters and byte 3 of a Command Status's. A host that
/// ignores it and writes seven commands back to back does not get an error:
/// the controller answers the first and **discards the rest**. A CSR8510
/// dongle answered `Reset` and dropped the six commands behind it, so the
/// event masks were never opened and no LE Meta Event ever arrived — which
/// looks exactly like a dead radio.
///
/// Nothing catches this in simulation. Simble's own controller answers
/// instantly, never runs out of buffers, and never drops a command, so the
/// bug is invisible until real silicon is on the other end. That is why this
/// lives beside [`HciChannel`] rather than in one example.
///
/// Usage is queue-and-release: hand every outbound command to
/// [`queue`](Self::queue), hand every inbound event to
/// [`observe_event`](Self::observe_event), and send whatever
/// [`next_to_send`](Self::next_to_send) yields.
///
/// # Deadlock, and why there is no timeout here
///
/// Spending the last credit on a command the controller never answers stops
/// the queue forever. That is a real property of HCI, not of this type, and a
/// timeout here would paper over a wedged controller by sending commands it
/// declared it has no room for. Callers that need a bound should put it on
/// their own pump loop, where "the dongle stopped answering" can be reported.
#[derive(Debug)]
pub struct CommandCredits {
    /// Commands the controller currently allows outstanding. Starts at 1:
    /// Vol 4, Part E, Section 4.4 says the host may send one command after
    /// reset and learns the real budget from the first answer.
    credits: u8,
    /// Commands waiting for a credit, in the order they were queued.
    queue: VecDeque<Vec<u8>>,
    /// Commands exempt from the budget (see [`Self::is_exempt`]), kept apart
    /// so a full `queue` cannot block them.
    exempt: VecDeque<Vec<u8>>,
}

impl CommandCredits {
    /// `HCI_Host_Number_Of_Completed_Packets` (Vol 4, Part E, Section
    /// 7.3.40). The one command outside the credit system: it generates no
    /// Command Complete, so spending a credit on it would leak that credit
    /// and eventually stall the queue for good.
    const HOST_NUMBER_OF_COMPLETED_PACKETS: u16 = 0x0C35;

    /// A fresh budget: one credit, nothing queued.
    pub fn new() -> Self {
        Self {
            credits: 1,
            queue: VecDeque::new(),
            exempt: VecDeque::new(),
        }
    }

    /// Whether `opcode` is outside the credit system.
    fn is_exempt(opcode: u16) -> bool {
        opcode == Self::HOST_NUMBER_OF_COMPLETED_PACKETS
    }

    /// Holds one HCI command packet — **without** its H4 type byte, so
    /// `[opcode_lo, opcode_hi, param_len, params…]` — until a credit frees
    /// up. A command shorter than an opcode is queued as-is and treated as
    /// non-exempt; validating HCI is not this type's job.
    pub fn queue(&mut self, command: Vec<u8>) {
        let opcode = match command.get(..2) {
            Some(&[lo, hi]) => u16::from_le_bytes([lo, hi]),
            _ => 0,
        };
        if Self::is_exempt(opcode) {
            self.exempt.push_back(command);
        } else {
            self.queue.push_back(command);
        }
    }

    /// The next command that may go out now, spending a credit for it.
    /// `None` means either nothing is queued or the budget is exhausted —
    /// call again after feeding the next event to
    /// [`observe_event`](Self::observe_event).
    pub fn next_to_send(&mut self) -> Option<Vec<u8>> {
        if let Some(command) = self.exempt.pop_front() {
            return Some(command);
        }
        if self.credits == 0 {
            return None;
        }
        let command = self.queue.pop_front()?;
        self.credits -= 1;
        Some(command)
    }

    /// Refills the budget from a controller-to-host event packet — again
    /// **without** its H4 type byte, so `[event_code, param_len, params…]`.
    /// Events other than Command Complete and Command Status are ignored, so
    /// a caller can pass every event it receives.
    ///
    /// The value *replaces* the budget rather than adding to it: the field is
    /// the controller restating how many commands it can take from here,
    /// which is also how it throttles to zero and later opens back up.
    pub fn observe_event(&mut self, event: &[u8]) {
        match event.first() {
            // Command Complete (7.7.14): Num_HCI_Command_Packets first.
            Some(&0x0E) => {
                if let Some(&n) = event.get(2) {
                    self.credits = n;
                }
            }
            // Command Status (7.7.15): Status, then Num_HCI_Command_Packets.
            Some(&0x0F) => {
                if let Some(&n) = event.get(3) {
                    self.credits = n;
                }
            }
            _ => {}
        }
    }

    /// Commands the controller says it can take right now.
    pub fn credits(&self) -> u8 {
        self.credits
    }

    /// How many commands are waiting for a credit.
    pub fn queued(&self) -> usize {
        self.queue.len() + self.exempt.len()
    }
}

impl Default for CommandCredits {
    fn default() -> Self {
        Self::new()
    }
}

/// In-memory bidirectional HCI channel pair connecting Simble Host to Rootcanal Controller.
pub struct HciChannel {
    /// Sender for the Host -> Controller direction.
    pub host_to_ctrl_tx: Sender<Vec<u8>>,
    /// Receiver for the Host -> Controller direction.
    pub host_to_ctrl_rx: Mutex<Receiver<Vec<u8>>>,

    /// Sender for the Controller -> Host direction.
    pub ctrl_to_host_tx: Sender<Vec<u8>>,
    /// Receiver for the Controller -> Host direction.
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

    /// Sends an HCI Synchronous (SCO) Data packet (prefixed with H4 byte
    /// 0x03) to the Controller — call audio on a SCO/eSCO link.
    ///
    /// `sco` is the packet *without* its H4 type byte: a 12-bit connection
    /// handle plus a two-bit Packet_Status_Flag, a one-octet length, then the
    /// payload. The handle is the **synchronous** link's own, which is not
    /// the ACL handle the link was set up over; audio addressed to the ACL
    /// handle goes nowhere.
    pub fn send_sco_data(&self, sco: &[u8]) -> Result<(), SimbleError> {
        self.send_h4(h4_type::HCI_SCO_DATA, sco)
    }

    /// Injects an already-H4-framed packet from Host to Controller — the
    /// host-side mirror of [`receive_from_controller`](Self::receive_from_controller),
    /// for callers (e.g. the `usb-ble-ws` bridge) that relay complete H4
    /// packets rather than building them via [`send_command`](Self::send_command).
    pub fn inject_host_packet(&self, h4_packet: Vec<u8>) -> Result<(), SimbleError> {
        self.host_to_ctrl_tx
            .send(h4_packet)
            .map_err(|e| SimbleError::Transport(e.to_string()))
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

    /// One credit is what the host starts with, and the second command has to
    /// wait for the first to be answered. This is the whole bug: without the
    /// gate both go out, and a real controller keeps only the first.
    #[test]
    fn test_credits_start_at_one_and_hold_the_rest() {
        let mut credits = CommandCredits::new();
        credits.queue(vec![0x03, 0x0C, 0x00]); // Reset
        credits.queue(vec![0x01, 0x0C, 0x08]); // Set Event Mask

        assert_eq!(credits.next_to_send(), Some(vec![0x03, 0x0C, 0x00]));
        assert_eq!(credits.next_to_send(), None, "the budget was one command");
        assert_eq!(credits.queued(), 1);

        // Command Complete for Reset, granting one more.
        credits.observe_event(&[0x0E, 0x04, 0x01, 0x03, 0x0C, 0x00]);
        assert_eq!(credits.next_to_send(), Some(vec![0x01, 0x0C, 0x08]));
        assert_eq!(credits.queued(), 0);
    }

    /// The field is a fresh budget, not an increment: a controller that says
    /// four means four outstanding, and a host that adds instead of replacing
    /// drifts upward until it is over budget again.
    #[test]
    fn test_command_complete_replaces_the_budget() {
        let mut credits = CommandCredits::new();
        for i in 0..5u8 {
            credits.queue(vec![i, 0x0C, 0x00]);
        }
        assert!(credits.next_to_send().is_some());
        assert!(credits.next_to_send().is_none());

        // Num_HCI_Command_Packets = 3.
        credits.observe_event(&[0x0E, 0x04, 0x03, 0x03, 0x0C, 0x00]);
        assert_eq!(credits.credits(), 3);
        assert_eq!(
            (0..4).filter(|_| credits.next_to_send().is_some()).count(),
            3,
            "three credits release exactly three commands"
        );
    }

    /// Command Status carries the same field one byte later, behind Status.
    /// Reading it at the Command Complete offset would take the status byte
    /// for a budget — and a status of 0 means "no credits", which stalls the
    /// queue permanently.
    #[test]
    fn test_command_status_credit_is_read_past_the_status_byte() {
        let mut credits = CommandCredits::new();
        credits.queue(vec![0x0D, 0x20, 0x00]);
        assert!(credits.next_to_send().is_some());

        // Status 0x00 (pending), Num_HCI_Command_Packets 0x02, opcode 0x200D.
        credits.observe_event(&[0x0F, 0x04, 0x00, 0x02, 0x0D, 0x20]);
        assert_eq!(credits.credits(), 2);
    }

    /// A controller can throttle to zero and open back up later; the queue
    /// must survive that rather than treating it as a lost command.
    #[test]
    fn test_a_zero_budget_stalls_the_queue_and_a_later_event_releases_it() {
        let mut credits = CommandCredits::new();
        credits.queue(vec![0x01, 0x0C, 0x00]);
        credits.queue(vec![0x02, 0x0C, 0x00]);
        assert!(credits.next_to_send().is_some());

        credits.observe_event(&[0x0E, 0x04, 0x00, 0x01, 0x0C, 0x00]); // zero credits
        assert_eq!(credits.credits(), 0);
        assert_eq!(credits.next_to_send(), None);
        assert_eq!(credits.queued(), 1, "the command is held, not dropped");

        credits.observe_event(&[0x0E, 0x04, 0x01, 0x01, 0x0C, 0x00]);
        assert_eq!(credits.next_to_send(), Some(vec![0x02, 0x0C, 0x00]));
    }

    /// Events that are not command answers say nothing about the budget.
    #[test]
    fn test_other_events_leave_the_budget_alone() {
        let mut credits = CommandCredits::new();
        // Disconnection Complete, then an LE Meta Event.
        credits.observe_event(&[0x05, 0x04, 0x00, 0x40, 0x00, 0x13]);
        credits.observe_event(&[0x3E, 0x02, 0x02, 0x00]);
        assert_eq!(credits.credits(), 1);
    }

    /// `HCI_Host_Number_Of_Completed_Packets` is outside the credit system
    /// (Vol 4, Part E, Section 7.3.40) — it produces no Command Complete, so
    /// spending a credit on it would leak that credit for good. It also must
    /// not queue behind a stalled command: it is the command that unstalls
    /// the controller's own buffers.
    #[test]
    fn test_host_number_of_completed_packets_ignores_the_budget() {
        let mut credits = CommandCredits::new();
        credits.queue(vec![0x03, 0x0C, 0x00]);
        assert!(credits.next_to_send().is_some());
        assert_eq!(credits.credits(), 0);

        credits.queue(vec![0x01, 0x0C, 0x00]); // ordinary, will wait
        credits.queue(vec![0x35, 0x0C, 0x05, 0x01, 0x40, 0x00, 0x01, 0x00]);

        assert_eq!(
            credits.next_to_send(),
            Some(vec![0x35, 0x0C, 0x05, 0x01, 0x40, 0x00, 0x01, 0x00]),
            "the flow-control command goes out with no credits and ahead of the queue"
        );
        assert_eq!(credits.credits(), 0, "and it spends nothing");
        assert_eq!(credits.next_to_send(), None);
    }

    /// A packet too short to carry an opcode is not an excuse to panic;
    /// validating HCI is not this type's job.
    #[test]
    fn test_short_packets_are_handled_without_panicking() {
        let mut credits = CommandCredits::new();
        credits.queue(vec![0x03]);
        assert_eq!(credits.next_to_send(), Some(vec![0x03]));
        credits.observe_event(&[]);
        credits.observe_event(&[0x0E]);
        credits.observe_event(&[0x0F, 0x04, 0x00]);
        assert_eq!(credits.credits(), 0);
    }

    #[test]
    fn test_sco_data_is_framed_as_h4_type_three() {
        // 0x03 was declared here for months and routed by nothing. The
        // symptom of getting this byte wrong is not an error: an ACL-framed
        // audio packet is a *valid* ACL packet, so it is delivered to the
        // signalling channel and silently ignored by the far end.
        let channel = HciChannel::new();

        // Handle 0x0101, packet status "correctly received", three octets.
        let sco = [0x01, 0x01, 0x03, 0xAA, 0xBB, 0xCC];
        channel.send_sco_data(&sco).unwrap();

        let pkt = channel.poll_host_packet().expect("packet available");
        assert_eq!(pkt[0], h4_type::HCI_SCO_DATA);
        assert_eq!(&pkt[1..], &sco);
    }
}
