// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Zero-copy Bluetooth packet structures and serializers.

pub mod att;
pub mod ext_adv;
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
pub use ext_adv::{
    AdvSetError, AdvertisingEnableEntry, AdvertisingSet, AdvertisingSets,
    ExtendedAdvertisingReportHeader, LeAdvertisingSetTerminatedEvent,
    LeExtendedAdvertisingReportEvent, LePeriodicAdvertisingCreateSync,
    LePeriodicAdvertisingReportEventHeader, LePeriodicAdvertisingSyncEstablishedEvent,
    LePeriodicAdvertisingSyncLostEvent, LeScanRequestReceivedEvent,
    LeSetExtendedAdvertisingDataHeader, LeSetExtendedAdvertisingEnableHeader,
    LeSetExtendedAdvertisingParameters, LeSetExtendedScanParametersHeader,
    LeSetPeriodicAdvertisingDataHeader, LeSetPeriodicAdvertisingParameters, ScanPhyParameters, U24,
    adv_event_properties, adv_phy, data_operation, ext_adv_opcode, ext_adv_report_event_type,
    ext_adv_subevent_code,
};
pub use hci::*;
pub use l2cap_frame::{AclPacketBoundary, HciAclHeader, L2capHeader, cid as l2cap_cid};
pub use l2cap_signaling::{
    ConfigurationRequestHeader, ConfigurationResponseHeader, ConnectionRequestHeader,
    ConnectionResponseHeader, DisconnectionRequest, DisconnectionResponse, L2capSignalingHeader,
    LeCreditBasedConnectionRequestHeader, LeCreditBasedConnectionResponseHeader,
    LeFlowControlCredit, signaling_code,
};
pub use smp::{
    SmpPairingFailed, SmpPairingPacket, auth_req as smp_auth_req, error_code as smp_error_code,
    io_capability as smp_io_capability, key_distribution as smp_key_distribution,
    opcode as smp_opcode,
};
