// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! SMP PDU definitions, cryptographic operations, and persistent KeyStore.

pub(crate) mod keystore;
pub(crate) mod pairing;

pub use crate::packets::{
    SmpPairingFailed, SmpPairingPacket, smp_auth_req as auth_req, smp_error_code as error_code,
    smp_io_capability as io_capability, smp_key_distribution as key_distribution,
    smp_opcode as opcode,
};
pub use keystore::{KeyStore, PairingKey, PairingKeys};
pub use pairing::{
    IdentityAddressPreference, PairingConfig, PairingSession, Role, SMP_DEBUG_KEY_PUBLIC_X,
    SMP_DEBUG_KEY_PUBLIC_Y, resolve_identity_address,
};
