// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! GAP (Generic Access Profile) advertising layer.

pub(crate) mod advertising;
pub mod ead;

pub use advertising::{
    AdvertisingData, MAX_ADV_LEN, ad_type, build_adv_payload, build_adv_payload_with_extras, flags,
    resolvable_set_identifier,
};
pub use ead::{
    ENCRYPTED_ADVERTISING_DATA_AD_TYPE, KEY_MATERIAL_CHARACTERISTIC_UUID, KeyMaterial, decrypt_ad,
    encrypt_ad,
};
