// Copyright 2026 The Android Open Source Project
// SPDX-License-Identifier: Apache-2.0

//! Zero-copy Bluetooth packet structures and serializers.

pub mod att;
pub mod hci;
pub mod l2cap_frame;
pub mod l2cap_signaling;
pub mod smp;

pub use att::{
    AttErrorRsp, AttExchangeMtuReq, AttExchangeMtuRsp, AttExecuteWriteReq, AttFindInformationReq,
    AttHandleValueHeader, AttPdu, AttPrepareWriteReqHeader, AttReadBlobReq,
    AttReadByGroupTypeReqHeader, AttReadByTypeReqHeader, AttReadReq, AttWriteReqHeader,
    error_code as att_error_code, opcode as att_opcode,
};
pub use hci::*;
pub use l2cap_frame::{AclPacketBoundary, HciAclHeader, L2capHeader, cid as l2cap_cid};
pub use l2cap_signaling::{
    DisconnectionRequest, DisconnectionResponse, L2capSignalingHeader,
    LeCreditBasedConnectionRequestHeader, LeCreditBasedConnectionResponseHeader,
    LeFlowControlCredit, signaling_code,
};
pub use smp::{
    SmpPairingFailed, SmpPairingPacket, io_capability as smp_io_capability, opcode as smp_opcode,
};
