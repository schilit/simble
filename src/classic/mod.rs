// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Bluetooth Classic (BR/EDR) host-stack support: SDP, RFCOMM, HFP,
//! AVDTP/A2DP, and HID. Controller-layer link establishment (LMP) lives in
//! `crate::controller`.

pub mod a2dp;
pub mod at;
pub mod avc;
pub mod avctp;
pub mod avdtp;
pub mod avrcp;
pub mod hfp;
pub mod hid;
pub mod rfcomm;
pub mod sdp;
