// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Virtual Bluetooth Device state machine and packet processor.

pub(crate) mod att_server;
pub(crate) mod bond_store;
pub(crate) mod connection;
pub mod car_kit;
pub mod central;
pub mod classic_host;
pub mod channel_sounding;
pub mod cis_central;
pub mod hid_host;
pub mod host;
pub mod ranging_scene;
pub(crate) mod observer;
pub(crate) mod virtual_device;

pub use bond_store::{BondSecurity, BondStore, MemoryBondStore};
pub use car_kit::{AtLine, CallPhase, CarKit, CarKitEvent, LinkPhase};
pub use central::{CentralEvent, CentralPhase, LeCentral};
pub use classic_host::{
    ClassicHost, ProtocolHandler, RfcommHandler, RfcommPort, SdpHandler, SharedRfcommPort,
};
pub use channel_sounding::{CsInitiator, CsReflector, CsState};
pub use cis_central::{CisCentral, CisConfig, CisState};
pub use hid_host::{HidEvent, HidHost, HidKind, HidPlan};
pub use host::LeHost;
pub use ranging_scene::RangingScene;
pub use connection::{ConnectionRole, ConnectionState, PrepareWriteChunk};
pub use observer::{AttServerObserver, SubscriptionReason};
pub use virtual_device::VirtualDevice;
