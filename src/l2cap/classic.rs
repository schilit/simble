// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Classic (BR/EDR) L2CAP Basic Mode channels — the connection-oriented
//! counterpart to `coc::CoCChannel`, which is LE-only.
//!
//! Basic Mode only: no segmentation at the L2CAP layer (an SDU is one PDU),
//! no credits, no retransmission. Enhanced Retransmission Mode is new design
//! work with no gold-standard reference to port and is out of scope here.

use crate::l2cap::cid_allocator::CidAllocator;
use crate::packets::l2cap_signaling::{
    ConfigurationRequestHeader, ConfigurationResponseHeader, ConnectionRequestHeader,
    ConnectionResponseHeader, configuration_result, connection_result, encode_mtu_option,
    parse_mtu_option,
};
use crate::types::SimbleError;
use std::collections::HashSet;
use zerocopy::U16;

/// Default MTU offered locally when a spec doesn't request otherwise.
pub const DEFAULT_MTU: u16 = 2048;
/// Minimum guaranteed BR/EDR MTU, assumed for a peer until configuration completes.
pub const MIN_BR_EDR_MTU: u16 = 48;

/// Basic Mode channel connection lifecycle (Bluetooth Core Vol 3, Part A, 6.1.1),
/// trimmed to the states reachable without ERTM/streaming modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelState {
    WaitConnectRsp,
    WaitConfig,
    Open,
    WaitDisconnect,
}

/// Spec for a Classic Basic Mode L2CAP channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassicChannelSpec {
    pub psm: u16,
    pub mtu: u16,
}

impl ClassicChannelSpec {
    pub fn new(psm: u16) -> Self {
        Self {
            psm,
            mtu: DEFAULT_MTU,
        }
    }

    pub fn with_mtu(psm: u16, mtu: u16) -> Self {
        Self { psm, mtu }
    }
}

/// An active Classic Basic Mode L2CAP channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassicChannel {
    pub cid: u16,
    pub peer_cid: u16,
    pub psm: u16,
    pub mtu: u16,
    pub peer_mtu: u16,
    pub state: ChannelState,
    local_config_done: bool,
    remote_config_done: bool,
}

impl ClassicChannel {
    pub fn is_open(&self) -> bool {
        self.state == ChannelState::Open
    }

    /// Checks whether an outbound SDU of `len` bytes fits within the peer's MTU.
    pub fn can_send(&self, len: usize) -> bool {
        len <= self.peer_mtu as usize
    }

    fn maybe_open(&mut self) {
        if self.local_config_done && self.remote_config_done {
            self.state = ChannelState::Open;
        }
    }
}

/// CID allocator, PSM registry, and channel/signaling-state tracker for
/// Classic Basic Mode L2CAP channels.
#[derive(Debug)]
pub struct ClassicChannelManager {
    channels: CidAllocator<ClassicChannel>,
    servers: HashSet<u16>,
}

impl Default for ClassicChannelManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ClassicChannelManager {
    pub const MIN_DYNAMIC_CID: u16 = 0x0040;
    pub const MAX_DYNAMIC_CID: u16 = 0xFFFF;

    pub fn new() -> Self {
        Self {
            channels: CidAllocator::new(Self::MIN_DYNAMIC_CID, Self::MAX_DYNAMIC_CID),
            servers: HashSet::new(),
        }
    }

    /// Registers a PSM as accepting inbound connections.
    pub fn register_server(&mut self, psm: u16) -> Result<(), SimbleError> {
        if !self.servers.insert(psm) {
            return Err(SimbleError::DeviceError(format!(
                "L2CAP Classic: PSM {psm:#06x} already registered"
            )));
        }
        Ok(())
    }

    pub fn unregister_server(&mut self, psm: u16) {
        self.servers.remove(&psm);
    }

    pub fn is_server_registered(&self, psm: u16) -> bool {
        self.servers.contains(&psm)
    }

    fn allocate_cid(&mut self) -> Result<u16, SimbleError> {
        self.channels.allocate().ok_or_else(|| {
            SimbleError::DeviceError("L2CAP Classic: All dynamic CIDs exhausted".into())
        })
    }

    /// Starts an outbound connection (client role): allocates a local CID
    /// and returns it along with the `Connection Request` to send.
    pub fn connect(
        &mut self,
        spec: &ClassicChannelSpec,
    ) -> Result<(u16, ConnectionRequestHeader), SimbleError> {
        let cid = self.allocate_cid()?;
        self.channels.insert(
            cid,
            ClassicChannel {
                cid,
                peer_cid: 0,
                psm: spec.psm,
                mtu: spec.mtu,
                peer_mtu: MIN_BR_EDR_MTU,
                state: ChannelState::WaitConnectRsp,
                local_config_done: false,
                remote_config_done: false,
            },
        );
        Ok((
            cid,
            ConnectionRequestHeader {
                psm: U16::from(spec.psm),
                source_cid: U16::from(cid),
            },
        ))
    }

    /// Handles an inbound `Connection Request` (server role). Returns the
    /// response to send back; on success the channel is created in
    /// `WaitConfig` state, using `mtu` as our locally offered MTU.
    pub fn on_connection_request(
        &mut self,
        request: &ConnectionRequestHeader,
        mtu: u16,
    ) -> Result<ConnectionResponseHeader, SimbleError> {
        let psm = request.psm.get();
        if !self.servers.contains(&psm) {
            return Ok(ConnectionResponseHeader {
                destination_cid: U16::from(0),
                source_cid: request.source_cid,
                result: U16::from(connection_result::REFUSED_PSM_NOT_SUPPORTED),
                status: U16::from(0),
            });
        }
        let cid = self.allocate_cid()?;
        self.channels.insert(
            cid,
            ClassicChannel {
                cid,
                peer_cid: request.source_cid.get(),
                psm,
                mtu,
                peer_mtu: MIN_BR_EDR_MTU,
                state: ChannelState::WaitConfig,
                local_config_done: false,
                remote_config_done: false,
            },
        );
        Ok(ConnectionResponseHeader {
            destination_cid: U16::from(cid),
            source_cid: request.source_cid,
            result: U16::from(connection_result::SUCCESSFUL),
            status: U16::from(0),
        })
    }

    /// Handles an inbound `Connection Response` for a connection we initiated.
    pub fn on_connection_response(
        &mut self,
        local_cid: u16,
        response: &ConnectionResponseHeader,
    ) -> Result<(), SimbleError> {
        let result = response.result.get();
        if result == connection_result::PENDING {
            return Ok(());
        }
        if result != connection_result::SUCCESSFUL {
            self.channels.remove(local_cid);
            return Err(SimbleError::DeviceError(format!(
                "L2CAP Classic: connection refused, result={result:#06x}"
            )));
        }
        let channel = self
            .channels
            .get_mut(local_cid)
            .ok_or_else(|| unknown_cid(local_cid))?;
        channel.peer_cid = response.destination_cid.get();
        channel.state = ChannelState::WaitConfig;
        Ok(())
    }

    /// Builds the local `Configuration Request` (MTU-only) for a channel.
    pub fn make_configuration_request(
        &self,
        cid: u16,
    ) -> Result<(ConfigurationRequestHeader, [u8; 4]), SimbleError> {
        let channel = self.get_channel(cid).ok_or_else(|| unknown_cid(cid))?;
        Ok((
            ConfigurationRequestHeader {
                destination_cid: U16::from(channel.peer_cid),
                flags: U16::from(0),
            },
            encode_mtu_option(channel.mtu),
        ))
    }

    /// Handles an inbound `Configuration Request` addressed to our `cid`,
    /// adopting the peer's MTU option (if present) as our transmit budget.
    pub fn on_configuration_request(
        &mut self,
        cid: u16,
        options: &[u8],
    ) -> Result<ConfigurationResponseHeader, SimbleError> {
        let channel = self.channels.get_mut(cid).ok_or_else(|| unknown_cid(cid))?;
        if let Some(mtu) = parse_mtu_option(options) {
            channel.peer_mtu = mtu;
        }
        channel.remote_config_done = true;
        channel.maybe_open();
        Ok(ConfigurationResponseHeader {
            source_cid: U16::from(channel.peer_cid),
            flags: U16::from(0),
            result: U16::from(configuration_result::SUCCESS),
        })
    }

    /// Handles an inbound `Configuration Response` acking our outbound
    /// `Configuration Request`.
    pub fn on_configuration_response(
        &mut self,
        cid: u16,
        response: &ConfigurationResponseHeader,
    ) -> Result<(), SimbleError> {
        let channel = self.channels.get_mut(cid).ok_or_else(|| unknown_cid(cid))?;
        if response.result.get() != configuration_result::SUCCESS {
            return Err(SimbleError::DeviceError(format!(
                "L2CAP Classic: configuration rejected, result={:#06x}",
                response.result.get()
            )));
        }
        channel.local_config_done = true;
        channel.maybe_open();
        Ok(())
    }

    /// Starts a local disconnection, returning `(destination_cid, source_cid)`
    /// to place in a `Disconnection Request`.
    pub fn disconnect(&mut self, cid: u16) -> Result<(u16, u16), SimbleError> {
        let channel = self.channels.get_mut(cid).ok_or_else(|| unknown_cid(cid))?;
        channel.state = ChannelState::WaitDisconnect;
        Ok((channel.peer_cid, channel.cid))
    }

    pub fn get_channel(&self, cid: u16) -> Option<&ClassicChannel> {
        self.channels.get(cid)
    }

    pub fn get_channel_mut(&mut self, cid: u16) -> Option<&mut ClassicChannel> {
        self.channels.get_mut(cid)
    }

    pub fn remove_channel(&mut self, cid: u16) -> Option<ClassicChannel> {
        self.channels.remove(cid)
    }
}

fn unknown_cid(cid: u16) -> SimbleError {
    SimbleError::DeviceError(format!("L2CAP Classic: unknown CID {cid:#06x}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_server_rejects_unregistered_psm() {
        let mut mgr = ClassicChannelManager::new();
        let request = ConnectionRequestHeader {
            psm: U16::from(0x0001),
            source_cid: U16::from(0x0040),
        };
        let response = mgr.on_connection_request(&request, DEFAULT_MTU).unwrap();
        assert_eq!(
            response.result.get(),
            connection_result::REFUSED_PSM_NOT_SUPPORTED
        );
        assert_eq!(response.destination_cid.get(), 0);
    }

    #[test]
    fn test_full_connection_and_configuration_handshake() {
        let mut client = ClassicChannelManager::new();
        let mut server = ClassicChannelManager::new();
        server.register_server(0x0001).unwrap();

        let spec = ClassicChannelSpec::with_mtu(0x0001, 1024);
        let (client_cid, request) = client.connect(&spec).unwrap();
        assert_eq!(
            client.get_channel(client_cid).unwrap().state,
            ChannelState::WaitConnectRsp
        );

        let response = server.on_connection_request(&request, 900).unwrap();
        assert_eq!(response.result.get(), connection_result::SUCCESSFUL);
        let server_cid = response.destination_cid.get();
        assert_eq!(
            server.get_channel(server_cid).unwrap().state,
            ChannelState::WaitConfig
        );

        client
            .on_connection_response(client_cid, &response)
            .unwrap();
        assert_eq!(
            client.get_channel(client_cid).unwrap().state,
            ChannelState::WaitConfig
        );
        assert_eq!(client.get_channel(client_cid).unwrap().peer_cid, server_cid);

        // Client -> Server configuration.
        let (client_config_req, client_options) =
            client.make_configuration_request(client_cid).unwrap();
        let server_config_rsp = server
            .on_configuration_request(client_config_req.destination_cid.get(), &client_options)
            .unwrap();
        client
            .on_configuration_response(client_cid, &server_config_rsp)
            .unwrap();
        assert_eq!(server.get_channel(server_cid).unwrap().peer_mtu, 1024);

        // Server -> Client configuration.
        let (server_config_req, server_options) =
            server.make_configuration_request(server_cid).unwrap();
        let client_config_rsp = client
            .on_configuration_request(server_config_req.destination_cid.get(), &server_options)
            .unwrap();
        server
            .on_configuration_response(server_cid, &client_config_rsp)
            .unwrap();
        assert_eq!(client.get_channel(client_cid).unwrap().peer_mtu, 900);

        assert!(client.get_channel(client_cid).unwrap().is_open());
        assert!(server.get_channel(server_cid).unwrap().is_open());
    }

    #[test]
    fn test_disconnect_and_cid_allocation() {
        let mut mgr = ClassicChannelManager::new();
        mgr.register_server(0x1001).unwrap();
        let req1 = ConnectionRequestHeader {
            psm: U16::from(0x1001),
            source_cid: U16::from(0x0040),
        };
        let rsp1 = mgr.on_connection_request(&req1, DEFAULT_MTU).unwrap();
        let cid1 = rsp1.destination_cid.get();
        assert_eq!(cid1, ClassicChannelManager::MIN_DYNAMIC_CID);

        let req2 = ConnectionRequestHeader {
            psm: U16::from(0x1001),
            source_cid: U16::from(0x0041),
        };
        let rsp2 = mgr.on_connection_request(&req2, DEFAULT_MTU).unwrap();
        let cid2 = rsp2.destination_cid.get();
        assert_ne!(cid1, cid2);

        let (dest, src) = mgr.disconnect(cid1).unwrap();
        assert_eq!(src, cid1);
        assert_eq!(dest, 0x0040);
        assert_eq!(
            mgr.get_channel(cid1).unwrap().state,
            ChannelState::WaitDisconnect
        );

        assert!(mgr.remove_channel(cid1).is_some());
        assert!(mgr.get_channel(cid1).is_none());
        assert!(mgr.get_channel(cid2).is_some());
    }

    #[test]
    fn test_can_send_respects_peer_mtu() {
        let mut mgr = ClassicChannelManager::new();
        mgr.register_server(0x0003).unwrap();
        let req = ConnectionRequestHeader {
            psm: U16::from(0x0003),
            source_cid: U16::from(0x0050),
        };
        let rsp = mgr.on_connection_request(&req, DEFAULT_MTU).unwrap();
        let cid = rsp.destination_cid.get();
        let channel = mgr.get_channel(cid).unwrap();
        assert!(channel.can_send(MIN_BR_EDR_MTU as usize));
        assert!(!channel.can_send(MIN_BR_EDR_MTU as usize + 1));
    }
}
