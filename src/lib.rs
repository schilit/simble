// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! # SimBLE: a native, zero-copy virtual Bluetooth host stack
//!
//! SimBLE creates virtual Bluetooth devices for testing — a simulated
//! heart-rate monitor, keyboard, LE Audio earbud, hands-free car kit, and more
//! — that other software can scan, connect to, and pair with. It implements the
//! Bluetooth host stack (HCI, L2CAP, ATT/GATT, SMP, plus the Classic and LE
//! Audio profiles) in pure, dependency-light Rust, with no async runtime and no
//! C.
//!
//! Devices can be built from the Rust API, an Android-shaped API, or short
//! [Rhai](https://rhai.rs) scripts. SimBLE connects to
//! [netsim](https://android.googlesource.com/platform/tools/netsim), the Android
//! emulator's network simulator, over a WebSocket, and can also reach real
//! hardware through a USB Bluetooth dongle.
//!
//! # Four surfaces
//!
//! One stack, four frontends:
//!
//! - **MCP (agent-first)** — [`mcp`] serves `simble mcp` over stdio, so an AI
//!   agent builds, runs and tests devices as tool calls without a checkout or
//!   a build step. This is the surface designed for agents.
//! - **Web** — the crate compiled for the browser, as `wasm32-unknown-unknown`
//!   (Rust's `arch-vendor-os` target name: the two `unknown`s mean no vendor
//!   and no host OS, because the code runs in a browser sandbox rather than on
//!   an operating system). See [`transport::wasm_ws`] for the bindings.
//! - **Native** — this library API and the `simble` CLI, for tests and CI.
//! - **Android** — a standalone app (`android/app/`, plain Java, no crate
//!   linkage) that puts a real phone controller on either end of a bulk BLE
//!   transfer; it talks to the rest only over BLE and HTTP. See
//!   `docs/phone-to-phone.md`. (A future headless backend that would run the
//!   scripting engine on-device over JNI is scaffolded in `android/rust/`.)
//!
//! The first three share this crate's engine, so they cannot diverge:
//! `run_test_script` and `lint_script` back the CLI, the browser's Testing
//! page, and the MCP `run_test`/`lint` tools alike.
//!
//! # What is public, and what that means
//!
//! The modules below are in two tiers. See `docs/api-surface.md` for the
//! measurement they came from.
//!
//! **Supported** — [`device`], [`devices`], [`scene`], [`scripting`],
//! [`types`], [`transport`], [`api`], [`service`], [`client`], [`gatt`],
//! [`profiles`], [`android`], [`classic`], [`controller`], [`cs`]. Every one
//! of these is imported by an `examples/` program, a `src/bin/` binary, or the
//! scripting layer's own public API, which is the only evidence this crate has
//! of what a consumer actually needs. Breaking changes here are still possible
//! before 1.0, but they are changes we owe you a note about.
//!
//! **Exposed for inspection, no stability promise** — `packets`, `att`,
//! `l2cap`, `gap`, `smp`, `crypto`, `df`, `audio`, `obex`. These are the wire
//! format and the protocol plumbing: field offsets, PDU builders, parser
//! internals. Nothing outside this crate imports them except this crate's own
//! `tests/`, so **they are `pub(crate)` unless the `testing` feature is on**,
//! and that feature is not for downstream use. Anything from them worth
//! depending on is re-exported at the crate root below, and the re-export is
//! the supported spelling. (Deliberately not intra-doc links: they would
//! resolve only in the wide-open build and fail in the one we publish.)
//!
//! The split is enforced, not merely documented: CI builds and clippies the
//! crate with **no** features so the closed surface stays compilable, because
//! `--all-features` would switch `testing` on and the closed build would never
//! run.

#![allow(clippy::type_complexity, clippy::too_many_arguments)]
// Every public item carries a doc comment; combined with CI's `-D warnings`
// this makes a new undocumented public item fail the build.
#![warn(missing_docs)]

// --- Supported tier ---------------------------------------------------------
pub mod android;
pub mod api;
pub mod classic;
pub mod client;
#[cfg(any(test, feature = "testing"))]
pub mod test_support;

pub mod controller;
pub mod cs;
pub mod device;
pub mod devices;
pub mod gatt;
// The MCP server (`simble mcp`) uses std stdin/stdout and native transports.
#[cfg(not(target_arch = "wasm32"))]
pub mod mcp;
pub mod profiles;
pub mod scene;
pub mod scripting;
pub mod service;
pub mod transport;
pub mod types;

// --- Exposed for inspection, no stability promise ---------------------------
//
// Each of these is `pub` only with the `testing` feature, which `cargo test`
// and `cargo build --examples` turn on through the self-dev-dependency in
// Cargo.toml and a downstream consumer does not. Rust cannot `cfg` a
// visibility, so the declaration is written twice; the alternative -- a
// `pub mod for_testing { pub use ... }` shim -- would have rewritten the
// import path in all 60 test files and hidden which module an item came from.
//
// Anything here that a consumer genuinely needs is re-exported at the crate
// root below; re-exporting a public item out of a private module is exactly
// how that is meant to work, and the root spelling is the supported one.
//
// The two `allow`s on the closed arm restore exactly the status quo: while
// these modules were unconditionally `pub`, neither lint could fire in them at
// all, because a `pub` item is reachable by definition. Making the module
// crate-private is what wakes them, and what they then report is almost
// entirely wire-format types and spec tables that `tests/` uses -- real
// consumers rustc cannot see, because `tests/` is a separate crate. Left as
// errors they would say "delete the PDU definitions your own test suite
// parses". The open arm is what every `cargo test` and CI's `--all-features`
// clippy compile, and it is unchanged.
//
// What the lints *would* have found is not lost, only moved: `docs/api-
// surface.md` lists every item nothing in the tree references, measured by
// running this build with the allows off.
macro_rules! plumbing_mod {
    ($name:ident) => {
        #[cfg(feature = "testing")]
        pub mod $name;
        #[cfg(not(feature = "testing"))]
        #[allow(unused_imports, dead_code)]
        pub(crate) mod $name;
    };
}

plumbing_mod!(att);
plumbing_mod!(audio);
plumbing_mod!(crypto);
plumbing_mod!(df);
plumbing_mod!(gap);
plumbing_mod!(l2cap);
plumbing_mod!(obex);
plumbing_mod!(packets);
plumbing_mod!(smp);

pub use api::{
    CreateDeviceRequest, CreateDeviceResponse, DeviceEvent, DeviceRole, SetAdvertisingRequest,
};
pub use client::{DiscoveredCharacteristic, DiscoveredDescriptor, DiscoveredService, GattClient};
pub use cs::{
    CsConfig, CsDistanceEstimate, CsMainMode, CsRole, CsStepResult, compute_pbr_distance,
};
pub use device::VirtualDevice;
pub use devices::{BleKeyboard, BleMouse, EddystoneUidBeacon, HeartRateMonitor, IBeacon};
pub use gap::AdvertisingData;
pub use gatt::{AttributePermissions, CharacteristicProperties, GattDatabase};
pub use l2cap::{AclReassembler, CoCChannel, CoCManager};
pub use profiles::{
    BatteryService, BodySensorLocation, CoordinatedSetIdentificationService,
    DeviceInformationService, GenericAttributeProfileService, HeartRateService,
    PublishedAudioCapabilitiesService, RangingService, VolumeControlService,
};
pub use service::{DeviceSummary, ManagedDevice, SimbleManager};
pub use smp::{KeyStore, PairingConfig, PairingKey, PairingKeys, PairingSession, Role as SmpRole};
pub use transport::{HciChannel, h4_type};
pub use types::{Address, AddressType, SimbleError, Uuid};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simble_integration_smoke_test() {
        let addr = Address::from_be_bytes([0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
        let mut dev = VirtualDevice::new("SimbleSmoke", addr, AddressType::Random);

        let svc_h = dev.gatt_db.add_service(Uuid::from_u16(0x180F), true); // Battery Service
        let (_, val_h) = dev.gatt_db.add_characteristic(
            Uuid::from_u16(0x2A19), // Battery Level
            CharacteristicProperties(
                CharacteristicProperties::READ | CharacteristicProperties::NOTIFY,
            ),
            vec![100], // 100%
            AttributePermissions::default(),
        );

        assert_eq!(svc_h, 1);
        assert_eq!(val_h, 3);
        assert_eq!(dev.gatt_db.read(val_h, 0).unwrap(), &[100]);
    }
}
