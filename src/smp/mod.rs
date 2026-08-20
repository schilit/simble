// Copyright 2026 The Android Open Source Project
// SPDX-License-Identifier: Apache-2.0

//! SMP PDU definitions, cryptographic operations, and persistent KeyStore.

pub mod keystore;

pub use crate::packets::{
    SmpPairingFailed, SmpPairingPacket, smp_io_capability as io_capability, smp_opcode as opcode,
};
pub use keystore::{KeyStore, PairingKey, PairingKeys};
