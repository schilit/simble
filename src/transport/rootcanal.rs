// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Bidirectional HCI pipe driver for a real (out-of-process) Rootcanal
//! controller, as opposed to `hci_adapter::HciChannel`'s in-memory bridge.
//!
//! Rootcanal's external transport is a plain byte stream (TCP or a Unix
//! domain socket both work — this module is generic over any `Read + Write`)
//! carrying H4-framed HCI packets in both directions, no additional envelope.
//! [`H4FrameReader`] turns that raw byte stream into complete H4 packets;
//! [`RootcanalTransport`] owns the stream and a reader, and [`pump`] moves
//! packets between the stream and an [`HciChannel`] so the rest of Simble
//! never has to know whether it's talking to the in-memory bridge or a real
//! Rootcanal process.
//!
//! [`pump`]: RootcanalTransport::pump

use crate::transport::h4_type;
use crate::types::SimbleError;
use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;

/// The number of header bytes needed, and where the payload-length field
/// lives within them, to know how long a complete H4 packet is (Bluetooth
/// Core Spec Vol 4, Part A). Returns `None` for an unrecognized H4 type.
fn header_len_and_payload_len(h4_type: u8, header: &[u8]) -> Option<(usize, usize)> {
    match h4_type {
        h4_type::HCI_COMMAND if header.len() >= 3 => Some((3, header[2] as usize)),
        h4_type::HCI_ACL_DATA if header.len() >= 4 => {
            Some((4, u16::from_le_bytes([header[2], header[3]]) as usize))
        }
        h4_type::HCI_SCO_DATA if header.len() >= 3 => Some((3, header[2] as usize)),
        h4_type::HCI_EVENT if header.len() >= 2 => Some((2, header[1] as usize)),
        h4_type::HCI_ISO_DATA if header.len() >= 4 => {
            let raw = u16::from_le_bytes([header[2], header[3]]);
            Some((4, (raw & 0x3FFF) as usize))
        }
        _ => None,
    }
}

/// Header length in bytes for each H4 type, excluding the leading type byte
/// itself. Mirrors the `match` in [`header_len_and_payload_len`].
fn header_len(h4_type: u8) -> Option<usize> {
    match h4_type {
        h4_type::HCI_COMMAND | h4_type::HCI_SCO_DATA => Some(3),
        h4_type::HCI_ACL_DATA | h4_type::HCI_ISO_DATA => Some(4),
        h4_type::HCI_EVENT => Some(2),
        _ => None,
    }
}

/// Incrementally reassembles complete H4 packets from an arbitrarily-chunked
/// byte stream. Bytes arrive via [`feed`](Self::feed); complete packets
/// (type byte + header + payload, the same shape `HciChannel` expects) come
/// out via [`next_packet`](Self::next_packet).
#[derive(Debug, Default)]
pub struct H4FrameReader {
    buffer: Vec<u8>,
    /// Read cursor: bytes before it belong to already-returned packets.
    /// Compacted away in [`feed`](Self::feed) rather than per packet, so
    /// popping a burst of packets stays O(n) instead of O(n^2).
    offset: usize,
}

impl H4FrameReader {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        if self.offset > 0 {
            self.buffer.drain(..self.offset);
            self.offset = 0;
        }
        self.buffer.extend_from_slice(bytes);
    }

    /// Pops and returns one complete H4 packet from the front of the buffer,
    /// if one is fully available yet.
    pub fn next_packet(&mut self) -> Result<Option<Vec<u8>>, SimbleError> {
        let buffer = &self.buffer[self.offset..];
        let Some(&h4_type) = buffer.first() else {
            return Ok(None);
        };
        let Some(min_header) = header_len(h4_type) else {
            return Err(SimbleError::Transport(format!(
                "unrecognized H4 packet type {h4_type:#04x}"
            )));
        };
        if buffer.len() < 1 + min_header {
            return Ok(None);
        }
        let (header_len, payload_len) =
            header_len_and_payload_len(h4_type, &buffer[1..]).expect("checked above");
        let total_len = 1 + header_len + payload_len;
        if buffer.len() < total_len {
            return Ok(None);
        }
        let packet = buffer[..total_len].to_vec();
        self.offset += total_len;
        if self.offset == self.buffer.len() {
            self.buffer.clear();
            self.offset = 0;
        }
        Ok(Some(packet))
    }
}

/// Reads H4-framed packets one blocking [`Read::read`] at a time into a
/// [`H4FrameReader`] until a complete packet is available, then returns it.
pub fn read_h4_packet<R: Read>(reader: &mut R) -> Result<Vec<u8>, SimbleError> {
    let mut framer = H4FrameReader::new();
    let mut chunk = [0u8; 512];
    loop {
        if let Some(packet) = framer.next_packet()? {
            return Ok(packet);
        }
        let n = reader
            .read(&mut chunk)
            .map_err(|e| SimbleError::Transport(e.to_string()))?;
        if n == 0 {
            return Err(SimbleError::Transport(
                "stream closed mid-packet".to_string(),
            ));
        }
        framer.feed(&chunk[..n]);
    }
}

/// Writes an already H4-framed packet (type byte + header + payload) to the
/// stream verbatim — Rootcanal's pipe carries no additional envelope.
pub fn write_h4_packet<W: Write>(writer: &mut W, packet: &[u8]) -> Result<(), SimbleError> {
    writer
        .write_all(packet)
        .map_err(|e| SimbleError::Transport(e.to_string()))
}

/// Bidirectional HCI pipe to an out-of-process Rootcanal controller, generic
/// over any `Read + Write` stream (a `TcpStream`, matching Rootcanal's usual
/// external test-port interface, or a Unix domain socket).
pub struct RootcanalTransport<S: Read + Write> {
    stream: S,
    framer: H4FrameReader,
}

impl RootcanalTransport<TcpStream> {
    /// Connects to a Rootcanal instance listening on `addr` (e.g.
    /// `"127.0.0.1:6402"`), putting the socket in non-blocking mode so
    /// [`pump`](Self::pump) never stalls waiting on the controller.
    pub fn connect(addr: &str) -> Result<Self, SimbleError> {
        let stream = TcpStream::connect(addr).map_err(|e| SimbleError::Transport(e.to_string()))?;
        stream
            .set_nonblocking(true)
            .map_err(|e| SimbleError::Transport(e.to_string()))?;
        Ok(Self::new(stream))
    }
}

impl<S: Read + Write> RootcanalTransport<S> {
    pub fn new(stream: S) -> Self {
        Self {
            stream,
            framer: H4FrameReader::new(),
        }
    }

    /// Moves packets in both directions between the pipe and `channel`:
    /// drains every packet `channel` has queued for the controller and
    /// writes it to the stream, then reads whatever bytes are currently
    /// available from the stream and hands any complete packets they form
    /// to `channel` as coming from the controller. Non-blocking: a stream
    /// with nothing to read is not an error, just zero packets delivered
    /// this call.
    pub fn pump(&mut self, channel: &super::HciChannel) -> Result<(), SimbleError> {
        while let Some(packet) = channel.poll_host_packet() {
            write_h4_packet(&mut self.stream, &packet)?;
        }

        let mut chunk = [0u8; 4096];
        loop {
            match self.stream.read(&mut chunk) {
                Ok(0) => return Err(SimbleError::Transport("Rootcanal pipe closed".to_string())),
                Ok(n) => self.framer.feed(&chunk[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) => return Err(SimbleError::Transport(e.to_string())),
            }
        }
        while let Some(packet) = self.framer.next_packet()? {
            channel.receive_from_controller(packet)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_h4_frame_reader_splits_command_event_and_acl_from_one_chunk() {
        let reset_cmd = [h4_type::HCI_COMMAND, 0x03, 0x0C, 0x00];
        let cmd_complete_evt = [h4_type::HCI_EVENT, 0x0E, 0x04, 0x01, 0x03, 0x0C, 0x00];
        let acl = [h4_type::HCI_ACL_DATA, 0x40, 0x00, 0x02, 0x00, 0xAA, 0xBB];

        let mut framer = H4FrameReader::new();
        framer.feed(&reset_cmd);
        framer.feed(&cmd_complete_evt);
        framer.feed(&acl);

        assert_eq!(framer.next_packet().unwrap().unwrap(), reset_cmd);
        assert_eq!(framer.next_packet().unwrap().unwrap(), cmd_complete_evt);
        assert_eq!(framer.next_packet().unwrap().unwrap(), acl);
        assert_eq!(framer.next_packet().unwrap(), None);
    }

    #[test]
    fn test_h4_frame_reader_handles_byte_at_a_time_delivery() {
        let evt = [h4_type::HCI_EVENT, 0x05, 0x02, 0xDE, 0xAD];
        let mut framer = H4FrameReader::new();
        for &byte in &evt[..evt.len() - 1] {
            framer.feed(&[byte]);
            assert_eq!(framer.next_packet().unwrap(), None);
        }
        framer.feed(&evt[evt.len() - 1..]);
        assert_eq!(framer.next_packet().unwrap().unwrap(), evt);
    }

    #[test]
    fn test_h4_frame_reader_rejects_unknown_type() {
        let mut framer = H4FrameReader::new();
        framer.feed(&[0xFF, 0x00]);
        assert!(framer.next_packet().is_err());
    }

    #[test]
    fn test_read_h4_packet_from_blocking_reader() {
        let evt = [h4_type::HCI_EVENT, 0x03, 0x02, 0x02, 0x03];
        let mut cursor = Cursor::new(evt.to_vec());
        let packet = read_h4_packet(&mut cursor).unwrap();
        assert_eq!(packet, evt);
    }

    #[test]
    fn test_write_h4_packet_writes_bytes_verbatim() {
        let cmd = [h4_type::HCI_COMMAND, 0x01, 0x02, 0x00];
        let mut out = Vec::new();
        write_h4_packet(&mut out, &cmd).unwrap();
        assert_eq!(out, cmd);
    }

    #[test]
    fn test_rootcanal_transport_pump_bridges_both_directions() {
        // A duplex-ish stand-in stream: reads from `inbound`, writes into `outbound`.
        struct DuplexMock {
            inbound: Cursor<Vec<u8>>,
            outbound: Vec<u8>,
        }
        impl Read for DuplexMock {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let n = self.inbound.read(buf)?;
                if n == 0 {
                    Err(std::io::Error::from(ErrorKind::WouldBlock))
                } else {
                    Ok(n)
                }
            }
        }
        impl Write for DuplexMock {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.outbound.extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let evt_from_controller = [h4_type::HCI_EVENT, 0x0E, 0x04, 0x01, 0x03, 0x0C, 0x00];
        let stream = DuplexMock {
            inbound: Cursor::new(evt_from_controller.to_vec()),
            outbound: Vec::new(),
        };
        let mut transport = RootcanalTransport::new(stream);

        let channel = super::super::HciChannel::new();
        channel.send_command(&[0x03, 0x0C, 0x00]).unwrap();

        transport.pump(&channel).unwrap();

        assert_eq!(
            transport.stream.outbound,
            vec![h4_type::HCI_COMMAND, 0x03, 0x0C, 0x00]
        );
        assert_eq!(
            channel.poll_controller_packet().unwrap(),
            evt_from_controller
        );
    }
}
