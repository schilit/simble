// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Virtual Bluetooth Device state machine and packet processor.

pub mod a2dp;
pub(crate) mod att_server;
pub mod big_broadcaster;
pub mod big_receiver;
pub(crate) mod bond_store;
pub mod car_kit;
pub mod central;
pub mod channel_sounding;
pub mod cis_central;
pub mod classic_hid;
pub mod classic_host;
pub(crate) mod connection;
pub mod hid_host;
pub mod host;
pub mod keyboard_scene;
pub(crate) mod observer;
pub mod profile_scene;
pub mod ranging_scene;
pub mod speaker_scene;
pub(crate) mod virtual_device;

pub use a2dp::{A2dpSink, A2dpSource, SourcePhase};
pub use big_broadcaster::{BigBroadcaster, BroadcastConfig, BroadcastState};
pub use big_receiver::{BigReceiver, FoundBroadcast, ReceiverConfig, ReceiverState};
pub use bond_store::{BondSecurity, BondStore, MemoryBondStore};
pub use car_kit::{AtLine, CallPhase, CarKit, CarKitEvent, LinkPhase};
pub use central::{CentralEvent, CentralPhase, LeCentral};
pub use channel_sounding::{CsInitiator, CsReflector, CsState};
pub use cis_central::{CisCentral, CisConfig, CisState};
pub use classic_hid::{ClassicHidDevice, ClassicHidHost, HidInput};
pub use classic_host::{
    ClassicHost, DiscoveredDevice, DlcWindow, HandlerChannel, LinkKey, LinkSecurity,
    ProtocolHandler, RfcommHandler, RfcommPort, ScoConnection, ScoPolicy, SdpHandler,
    SdpQueryHandler, SdpQueryResults, SharedRfcommPort, SharedSdpQueryResults,
};
pub use connection::{ConnectionRole, ConnectionState, PrepareWriteChunk};
pub use hid_host::{HidEvent, HidHost, HidKind, HidPlan};
pub use host::LeHost;
pub use keyboard_scene::KeyboardScene;
pub use observer::{AttServerObserver, SubscriptionReason};
pub use profile_scene::{DeviceSpec, LinkPhase as ClassicLinkPhase, ProfileScene};
pub use ranging_scene::RangingScene;
pub use speaker_scene::SpeakerScene;
pub use virtual_device::VirtualDevice;
