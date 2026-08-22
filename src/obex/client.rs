// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! A minimal OBEX client — enough to drive [`super::server::ObexServer`]
//! through a whole transfer, so the server's continuation handling is
//! exercised by something that behaves like a peer rather than by a test
//! asserting its own expectations.
//!
//! It is deliberately small: simble's devices are servers (a phone pushes
//! *to* them). This exists to test them and to let a scripted device push an
//! object back where a profile calls for it.

use super::header::Header;
use super::packet::{PacketError, Request, Response, response};
use super::server::put_packets;

/// Where a client is in its exchange with a server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientState {
    /// No session; a CONNECT has not been answered.
    Idle,
    /// A session is established.
    Connected,
    /// A PUT is in flight with packets still to send.
    Putting,
    /// The last operation finished.
    Done,
    /// The server rejected something; the code is carried.
    Failed(u8),
}

/// A transport-agnostic OBEX client.
#[derive(Debug, Clone)]
pub struct ObexClient {
    state: ClientState,
    max_packet_length: u16,
    /// Packets of the transfer in flight, in order, not yet sent.
    queued: Vec<Vec<u8>>,
    /// Whether the response now expected answers a CONNECT, which changes
    /// how it parses.
    awaiting_connect: bool,
}

impl Default for ObexClient {
    fn default() -> Self {
        Self::new(0x2000)
    }
}

impl ObexClient {
    /// Creates a client advertising `max_packet_length` in its CONNECT.
    pub fn new(max_packet_length: u16) -> Self {
        Self {
            state: ClientState::Idle,
            max_packet_length,
            queued: Vec::new(),
            awaiting_connect: false,
        }
    }

    /// The client's current state.
    pub fn state(&self) -> ClientState {
        self.state
    }

    /// Produces a CONNECT to send.
    pub fn connect(&mut self) -> Vec<u8> {
        self.awaiting_connect = true;
        Request::connect(self.max_packet_length, Vec::new()).to_bytes()
    }

    /// Produces a DISCONNECT to send.
    pub fn disconnect(&mut self) -> Vec<u8> {
        self.awaiting_connect = false;
        Request::disconnect(Vec::new()).to_bytes()
    }

    /// Queues an object for pushing and returns the first packet to send.
    ///
    /// The object is chunked against `peer_max_packet_length` — the value
    /// the server advertised in its CONNECT response, which is why a client
    /// should connect before pushing anything large.
    pub fn put(
        &mut self,
        name: Option<&str>,
        mime_type: Option<&[u8]>,
        body: &[u8],
        peer_max_packet_length: u16,
    ) -> Vec<u8> {
        self.queued = put_packets(name, mime_type, body, peer_max_packet_length);
        self.queued.reverse(); // pop from the back
        self.awaiting_connect = false;
        // Putting either way: even a single-packet push is unfinished until
        // the server's response arrives.
        self.state = ClientState::Putting;
        self.queued.pop().unwrap_or_default()
    }

    /// Feeds one response from the server, returning the next packet to send
    /// if the exchange continues.
    pub fn handle_response(&mut self, bytes: &[u8]) -> Result<Option<Vec<u8>>, PacketError> {
        let parsed = Response::parse(bytes, self.awaiting_connect)?;
        self.awaiting_connect = false;

        match parsed.code {
            response::CONTINUE => Ok(self.queued.pop()),
            response::SUCCESS => {
                if !self.queued.is_empty() {
                    // A server answering Success mid-transfer is the bug the
                    // continuation flow guards against; surface it rather
                    // than sending the rest into a finished operation.
                    self.queued.clear();
                    self.state = ClientState::Failed(response::SUCCESS);
                    return Ok(None);
                }
                self.state = if parsed.connect.is_some() {
                    ClientState::Connected
                } else {
                    ClientState::Done
                };
                Ok(None)
            }
            code => {
                self.queued.clear();
                self.state = ClientState::Failed(code);
                Ok(None)
            }
        }
    }

    /// Produces an ABORT, abandoning any queued packets.
    pub fn abort(&mut self) -> Vec<u8> {
        self.queued.clear();
        Request::abort().to_bytes()
    }

    /// A GET for `name`, for profiles that pull rather than push.
    pub fn get(&mut self, name: &str) -> Vec<u8> {
        self.awaiting_connect = false;
        Request::get(true, vec![Header::Name(name.to_string())]).to_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::obex::server::{ObexServer, ServerEvent};

    /// Drives a full push between a client and server through nothing but
    /// byte buffers — the shape a real transport would relay.
    fn push(body: &[u8], peer_max: u16) -> Vec<u8> {
        let mut client = ObexClient::new(0x2000);
        let mut server = ObexServer::default();

        let (mut response, _) = server.handle_packet(&client.connect());
        client.handle_response(&response).unwrap();
        assert_eq!(client.state(), ClientState::Connected);

        let mut packet = client.put(Some("obj.bin"), Some(b"application/octet-stream\0"), body, peer_max);
        loop {
            let (bytes, event) = server.handle_packet(&packet);
            response = bytes;
            match client.handle_response(&response).unwrap() {
                Some(next) => packet = next,
                None => {
                    assert!(
                        matches!(event, ServerEvent::ObjectReceived(_)),
                        "the transfer must end with a completed object"
                    );
                    break;
                }
            }
        }
        assert_eq!(client.state(), ClientState::Done);
        server.take_objects().pop().unwrap().body
    }

    #[test]
    fn test_client_and_server_complete_a_single_packet_push() {
        assert_eq!(push(b"small", 0x2000), b"small");
    }

    #[test]
    fn test_client_and_server_complete_a_multi_packet_push() {
        let body: Vec<u8> = (0..2000u32).map(|i| (i % 256) as u8).collect();
        assert_eq!(push(&body, 128), body);
    }

    #[test]
    fn test_client_records_a_rejection() {
        let mut client = ObexClient::default();
        let mut server = ObexServer::default();
        let response = server.handle_packet(&client.get("nothing")).0;
        client.handle_response(&response).unwrap();
        assert_eq!(
            client.state(),
            ClientState::Failed(response::NOT_IMPLEMENTED)
        );
    }

    /// A server that answers Success while the client still has packets
    /// queued has truncated the object. The client must notice rather than
    /// carry on sending into a finished operation.
    #[test]
    fn test_client_detects_a_server_that_finishes_early() {
        let mut client = ObexClient::default();
        client.put(Some("x"), None, &vec![0u8; 1000], 128);
        let early_success = Response::success(Vec::new()).to_bytes();
        assert_eq!(client.handle_response(&early_success).unwrap(), None);
        assert_eq!(client.state(), ClientState::Failed(response::SUCCESS));
    }
}
