// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! L2CAP layer re-exports, reassembly engine, and Connection-Oriented Channels.

pub(crate) mod cid_allocator;
pub mod classic;
pub mod coc;
pub mod reassembler;

pub use crate::packets::{
    AclPacketBoundary, DisconnectionRequest, DisconnectionResponse, HciAclHeader, L2capHeader,
    L2capSignalingHeader, LeCreditBasedConnectionRequestHeader,
    LeCreditBasedConnectionResponseHeader, LeFlowControlCredit, l2cap_cid as cid, signaling_code,
};
pub use coc::{CoCChannel, CoCManager};
pub use reassembler::AclReassembler;
