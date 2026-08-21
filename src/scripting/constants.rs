// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Constants dictionary (spec §05a: "no magic bytes in scripts"). Every
//! value here *references* a Rust const that already exists (`profiles::*`
//! uuid modules, the Android layer's `PROPERTY_*`/`GATT_*` sets, `bap`'s
//! audio bitmasks) — one source of truth, never re-declared values.
//! Registered as static-module constants, which Rhai keeps genuinely
//! immutable to scripts.

use rhai::{Engine, EvalAltResult, Module};

use crate::android::gatt_server::BluetoothProfile;
use crate::android::gatt_service::{
    BluetoothGatt, BluetoothGattCharacteristic, BluetoothGattService,
};
use crate::gatt::database::desc_uuid;
use crate::profiles::bap::{audio_location, context_type};
use crate::profiles::gatt_service::gatt_char_uuid;
use crate::profiles::{
    BatteryService, HeartRateService, aics::aics_uuid, ascs::ascs_uuid, bap::bap_uuid,
    csip::csip_uuid, dis::characteristic_uuid as dis_uuid, pacs::pacs_uuid, ras::ras_uuid,
    vcp::vcp_uuid, vocs::vocs_uuid,
};
use crate::types::Uuid;

/// Registers the `uuid::*` and `audio::location::*`/`audio::context::*`
/// modules. (`android::*` constants are folded into the `android` module
/// built in `bindings`, so scripts see one namespace for that whole layer.)
pub fn register(engine: &mut Engine) {
    engine.register_static_module("uuid", uuid_module().into());
    engine.register_static_module("audio", audio_module().into());
}

/// Fills the `android` module's constants (spec §05a lists them alongside
/// the constructors that consume them).
pub fn set_android_constants(module: &mut Module) {
    macro_rules! int_consts {
        ($($src:ty { $($name:ident),+ $(,)? })+) => {
            $($(module.set_var(stringify!($name), <$src>::$name as i64);)+)+
        };
    }

    int_consts! {
        BluetoothGattCharacteristic {
            PROPERTY_BROADCAST, PROPERTY_READ, PROPERTY_WRITE_NO_RESPONSE, PROPERTY_WRITE,
            PROPERTY_NOTIFY, PROPERTY_INDICATE, PROPERTY_SIGNED_WRITE, PROPERTY_EXTENDED_PROPS,
            PERMISSION_READ, PERMISSION_READ_ENCRYPTED, PERMISSION_READ_ENCRYPTED_MITM,
            PERMISSION_WRITE, PERMISSION_WRITE_ENCRYPTED, PERMISSION_WRITE_ENCRYPTED_MITM,
            PERMISSION_WRITE_SIGNED, PERMISSION_WRITE_SIGNED_MITM,
        }
        BluetoothGatt {
            GATT_SUCCESS, GATT_READ_NOT_PERMITTED, GATT_WRITE_NOT_PERMITTED,
            GATT_INSUFFICIENT_AUTHENTICATION, GATT_REQUEST_NOT_SUPPORTED, GATT_INVALID_OFFSET,
            GATT_INSUFFICIENT_AUTHORIZATION, GATT_INVALID_ATTRIBUTE_LENGTH,
            GATT_INSUFFICIENT_ENCRYPTION, GATT_CONNECTION_CONGESTED, GATT_FAILURE,
        }
        BluetoothGattService { SERVICE_TYPE_PRIMARY, SERVICE_TYPE_SECONDARY }
        BluetoothProfile {
            STATE_DISCONNECTED, STATE_CONNECTING, STATE_CONNECTED, STATE_DISCONNECTING,
        }
    }
}

fn uuid_module() -> Module {
    let mut module = Module::new();

    // Escape hatches for UUIDs with no named const (custom 128-bit UUIDs,
    // rarely-bound assigned numbers): parse the standard string forms, or
    // lift a bare 16-bit assigned number.
    module.set_native_fn("of", |s: &str| -> Result<Uuid, Box<EvalAltResult>> {
        s.parse::<Uuid>().map_err(|e| {
            Box::new(EvalAltResult::ErrorRuntime(
                rhai::Dynamic::from(e),
                rhai::Position::NONE,
            ))
        })
    });
    module.set_native_fn("from_u16", |n: i64| -> Result<Uuid, Box<EvalAltResult>> {
        u16::try_from(n).map(Uuid::from_u16).map_err(|_| {
            Box::new(EvalAltResult::ErrorRuntime(
                rhai::Dynamic::from(format!("uuid::from_u16: out of range: {n}")),
                rhai::Position::NONE,
            ))
        })
    });

    macro_rules! uuid_consts {
        // Consts already typed `Uuid` in their Rust module.
        ($($path:path { $($name:ident),+ $(,)? })+) => {
            $($({ use $path as m; module.set_var(stringify!($name), m::$name); })+)+
        };
    }
    macro_rules! uuid16_consts {
        // Consts declared as bare `u16` assigned numbers in their Rust module.
        ($($path:path { $($name:ident),+ $(,)? })+) => {
            $($({ use $path as m; module.set_var(stringify!($name), Uuid::from_u16(m::$name)); })+)+
        };
    }

    // Heart Rate / Battery consts live on their service builder types, not
    // in a uuid module, so they're mapped by hand to the flat script names.
    module.set_var("HEART_RATE_SERVICE", HeartRateService::SERVICE_UUID);
    module.set_var("HEART_RATE_MEASUREMENT", HeartRateService::MEASUREMENT_UUID);
    module.set_var(
        "BODY_SENSOR_LOCATION",
        HeartRateService::BODY_SENSOR_LOCATION_UUID,
    );
    module.set_var("BATTERY_SERVICE", BatteryService::SERVICE_UUID);
    module.set_var("BATTERY_LEVEL", BatteryService::BATTERY_LEVEL_UUID);

    uuid_consts! {
        pacs_uuid {
            PACS_SERVICE, SINK_PAC, SINK_AUDIO_LOCATIONS, SOURCE_PAC, SOURCE_AUDIO_LOCATIONS,
            AVAILABLE_AUDIO_CONTEXTS, SUPPORTED_AUDIO_CONTEXTS,
        }
        ascs_uuid { AUDIO_STREAM_CONTROL_SERVICE, SINK_ASE, SOURCE_ASE, ASE_CONTROL_POINT }
        bap_uuid { BASIC_AUDIO_ANNOUNCEMENT_SERVICE, BROADCAST_AUDIO_ANNOUNCEMENT_SERVICE }
        csip_uuid {
            CSIS_SERVICE, SET_IDENTITY_RESOLVING_KEY, SET_MEMBER_SIZE, SET_MEMBER_LOCK,
            SET_MEMBER_RANK,
        }
        vcp_uuid { VOLUME_CONTROL_SERVICE, VOLUME_STATE, VOLUME_CONTROL_POINT, VOLUME_FLAGS }
        vocs_uuid {
            VOLUME_OFFSET_CONTROL_SERVICE, VOLUME_OFFSET_STATE, AUDIO_LOCATION,
            VOLUME_OFFSET_CONTROL_POINT, AUDIO_OUTPUT_DESCRIPTION,
        }
        aics_uuid {
            AUDIO_INPUT_CONTROL_SERVICE, AUDIO_INPUT_STATE, GAIN_SETTINGS_ATTRIBUTE,
            AUDIO_INPUT_TYPE, AUDIO_INPUT_STATUS, AUDIO_INPUT_CONTROL_POINT,
            AUDIO_INPUT_DESCRIPTION,
        }
        ras_uuid {
            RANGING_SERVICE, RANGING_FEATURES, RANGING_REALTIME_DATA, RANGING_ON_DEMAND_DATA,
            RANGING_CONTROL_POINT,
        }
    }

    uuid16_consts! {
        gatt_char_uuid {
            GENERIC_ATTRIBUTE_SERVICE, SERVICE_CHANGED, CLIENT_SUPPORTED_FEATURES, DATABASE_HASH,
            SERVER_SUPPORTED_FEATURES,
        }
        dis_uuid {
            SYSTEM_ID, MODEL_NUMBER, SERIAL_NUMBER, FIRMWARE_REVISION, HARDWARE_REVISION,
            SOFTWARE_REVISION, MANUFACTURER_NAME, IEEE_REGULATORY, PNP_ID,
        }
        desc_uuid { CLIENT_CHARACTERISTIC_CONFIGURATION, CHARACTERISTIC_USER_DESCRIPTION }
    }

    module
}

fn audio_module() -> Module {
    let mut audio = Module::new();

    macro_rules! sub_consts {
        ($sub:ident, $path:path { $($name:ident),+ $(,)? }) => {{
            use $path as m;
            let mut sub = Module::new();
            $(sub.set_var(stringify!($name), m::$name as i64);)+
            audio.set_sub_module(stringify!($sub), sub);
        }};
    }

    sub_consts!(
        location,
        audio_location {
            NOT_ALLOWED,
            FRONT_LEFT,
            FRONT_RIGHT,
            FRONT_CENTER,
            LOW_FREQUENCY_EFFECTS_1,
            BACK_LEFT,
            BACK_RIGHT,
            FRONT_LEFT_OF_CENTER,
            FRONT_RIGHT_OF_CENTER,
            BACK_CENTER,
            LOW_FREQUENCY_EFFECTS_2,
            SIDE_LEFT,
            SIDE_RIGHT,
            TOP_FRONT_LEFT,
            TOP_FRONT_RIGHT,
            TOP_FRONT_CENTER,
            TOP_CENTER,
            TOP_BACK_LEFT,
            TOP_BACK_RIGHT,
            TOP_SIDE_LEFT,
            TOP_SIDE_RIGHT,
            TOP_BACK_CENTER,
            BOTTOM_FRONT_CENTER,
            BOTTOM_FRONT_LEFT,
            BOTTOM_FRONT_RIGHT,
            FRONT_LEFT_WIDE,
            FRONT_RIGHT_WIDE,
            LEFT_SURROUND,
            RIGHT_SURROUND,
            STEREO,
        }
    );

    sub_consts!(
        context,
        context_type {
            PROHIBITED,
            UNSPECIFIED,
            CONVERSATIONAL,
            MEDIA,
            GAME,
            INSTRUCTIONAL,
            VOICE_ASSISTANTS,
            LIVE,
            SOUND_EFFECTS,
            NOTIFICATIONS,
            RINGTONE,
            ALERTS,
            EMERGENCY_ALARM,
        }
    );

    audio
}
