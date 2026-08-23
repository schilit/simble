// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Virtual Bluetooth Device state machine and packet processor.

pub(crate) mod att_server;
pub mod big_broadcaster;
pub mod big_receiver;
pub(crate) mod bond_store;
pub mod car_kit;
pub mod central;
pub mod channel_sounding;
pub mod cis_central;
pub mod classic_host;
pub(crate) mod connection;
pub mod hid_host;
pub mod host;
pub(crate) mod observer;
pub mod ranging_scene;
pub(crate) mod virtual_device;

pub use big_broadcaster::{BigBroadcaster, BroadcastConfig, BroadcastState};
pub use big_receiver::{BigReceiver, FoundBroadcast, ReceiverConfig, ReceiverState};
pub use bond_store::{BondSecurity, BondStore, MemoryBondStore};
pub use car_kit::{AtLine, CallPhase, CarKit, CarKitEvent, LinkPhase};
pub use central::{CentralEvent, CentralPhase, LeCentral};
pub use channel_sounding::{CsInitiator, CsReflector, CsState};
pub use cis_central::{CisCentral, CisConfig, CisState};
pub use classic_host::{
    ClassicHost, DiscoveredDevice, DlcWindow, ProtocolHandler, RfcommHandler, RfcommPort,
    ScoConnection, ScoPolicy, SdpHandler, SdpQueryHandler, SdpQueryResults, SharedRfcommPort,
    SharedSdpQueryResults,
};
pub use connection::{ConnectionRole, ConnectionState, PrepareWriteChunk};
pub use hid_host::{HidEvent, HidHost, HidKind, HidPlan};
pub use host::LeHost;
pub use observer::{AttServerObserver, SubscriptionReason};
pub use ranging_scene::RangingScene;
pub use virtual_device::VirtualDevice;
