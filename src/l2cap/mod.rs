// Copyright 2026 The Android Open Source Project
// SPDX-License-Identifier: Apache-2.0

//! L2CAP layer re-exports, reassembly engine, and Connection-Oriented Channels.

pub mod coc;
pub mod reassembler;

pub use crate::packets::{
    AclPacketBoundary, DisconnectionRequest, DisconnectionResponse, HciAclHeader, L2capHeader,
    L2capSignalingHeader, LeCreditBasedConnectionRequestHeader,
    LeCreditBasedConnectionResponseHeader, LeFlowControlCredit, l2cap_cid as cid, signaling_code,
};
pub use coc::{CoCChannel, CoCManager};
pub use reassembler::AclReassembler;
