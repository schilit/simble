// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! The Rhai *test/web* runtime: the extension bindings a scripted device runs
//! with (`register_web_extensions`) and the compile/run entry points
//! ([`run_test_script`], [`lint_script`]) an agent uses to check a script.
//!
//! `register_web_extensions` and its small conversion helpers were split out of
//! the browser transport so the scripted-device engine and the CLI can share
//! one definition of the web scripting surface.

use crate::scripting::bindings::{dynamic_to_bytes, runtime_error};
use crate::scripting::{ScriptGattServer, new_engine};
use crate::types::Uuid;
use rhai::{Array, Blob, Dynamic, Engine, EvalAltResult, Map, Scope};

pub(crate) fn find_value_handle(server: &ScriptGattServer, uuid: Uuid) -> Option<u16> {
    // Services built by a Rust profile registrar (add_ras, add_pacs,
    // add_ascs) exist only in the GATT database, so fall back to it when
    // the script's own service list does not know the UUID.
    if let Some(handle) = server.with_server(|s| s.device.gatt_db.value_handle_for_uuid(uuid)) {
        return Some(handle);
    }
    server.with_server(|s| {
        s.get_services()
            .iter()
            .flat_map(|service| service.characteristics.clone())
            .find(|characteristic| characteristic.uuid == uuid)
            .and_then(|characteristic| characteristic.value_handle)
    })
}

/// Narrows a Rhai integer to a `u8`, so a script that passes 300 for a volume gets a
/// runtime error naming the parameter instead of a silent truncation. Rhai has one integer
/// type, and every profile field below is a byte.
fn to_u8(name: &str, value: i64) -> Result<u8, Box<EvalAltResult>> {
    u8::try_from(value).map_err(|_| runtime_error(format!("{name}: not a byte (0..=255): {value}")))
}

/// Reads a 16-byte key (a CSIP SIRK) out of a script value.
fn to_key_128(name: &str, value: Dynamic) -> Result<[u8; 16], Box<EvalAltResult>> {
    let bytes = dynamic_to_bytes(value)?;
    <[u8; 16]>::try_from(bytes.as_slice())
        .map_err(|_| runtime_error(format!("{name}: expected 16 bytes, got {}", bytes.len())))
}

/// Registers the web runtime's scripting extension: `server.update_value(
/// uuid, bytes)` writes a characteristic's value into the live GattDatabase.
/// This exists because `notify_characteristic_changed` doesn't persist its
/// value into the database, and the web glue treats the database as the one
/// source of truth (the page UI reads it, and value changes become wire
/// notifications) — pending real advertising/notification Rhai bindings.
pub(crate) fn register_web_extensions(engine: &mut Engine) {
    engine.register_fn(
        "update_value",
        |server: &mut ScriptGattServer,
         uuid: Uuid,
         value: Dynamic|
         -> Result<(), Box<EvalAltResult>> {
            let bytes = dynamic_to_bytes(value)?;
            let handle = find_value_handle(server, uuid)
                .ok_or_else(|| runtime_error(format!("no characteristic with UUID {uuid}")))?;
            server
                .with_server(|s| s.device.gatt_db.set_value(handle, &bytes))
                .map_err(|status| runtime_error(format!("set_value failed: ATT error {status}")))
        },
    );
    // The read half: `server.value(uuid)` returns the characteristic's
    // current bytes from the live GattDatabase — including values a central
    // wrote. A script's `fn tick` has no variables that survive between
    // calls, so the database is the only state it can carry; this getter is
    // what lets a device react to writes (setpoints, control points).
    // Real profile implementations, callable from a script. The protocol
    // lives in Rust (`crate::profiles`) — a state machine with tests — and
    // the script just composes a device out of them. `add_ascs` in
    // particular installs the ASE control-point handler, so a peer's Config
    // Codec / Enable writes actually drive the endpoint state machine
    // instead of landing on inert bytes.
    engine.register_fn(
        "add_pacs",
        |server: &mut ScriptGattServer, sink_location: i64, source_location: i64| {
            server.with_server(|s| {
                crate::profiles::PublishedAudioCapabilitiesService::register(
                    &mut s.device.gatt_db,
                    sink_location as u32,
                    source_location as u32,
                );
            });
        },
    );
    // Several profiles carry IEEE-754 floats (ranging distance, weight
    // scales, some sensor readings) and a script has no other way to lay
    // one out. Little-endian, as everything on the wire is.
    engine.register_fn("f32_le", |value: f64| -> Blob {
        (value as f32).to_le_bytes().to_vec()
    });

    engine.register_fn(
        // Ranging Service (185B) — the GATT half of Bluetooth 6.0 Channel
        // Sounding: a responder publishes distance estimates a peer reads or
        // subscribes to. The ranging procedure itself is a controller
        // feature; this is what a phone actually talks to.
        "add_ras",
        |server: &mut ScriptGattServer| {
            server.with_server(|s| {
                crate::profiles::RangingService::register(&mut s.device.gatt_db);
            });
        },
    );
    engine.register_fn(
        "add_ascs",
        |server: &mut ScriptGattServer,
         sink_ase_ids: Dynamic,
         source_ase_ids: Dynamic|
         -> Result<(), Box<EvalAltResult>> {
            let sink = dynamic_to_bytes(sink_ase_ids)?;
            let source = dynamic_to_bytes(source_ase_ids)?;
            server.with_server(|s| {
                crate::profiles::AudioStreamControlService::register(
                    &mut s.device.gatt_db,
                    &sink,
                    &source,
                );
            });
            Ok(())
        },
    );
    // ---- Volume Control: Android's `BluetoothVolumeControl` -------------
    //
    // Three services, because that is how the spec splits them: VCS is the
    // device's own volume, and a device includes one VOCS per audio output
    // and one AICS per audio input. `BluetoothVolumeControl` is the single
    // Android proxy over all three (`setVolume`, `setVolumeOffset`,
    // `setDeviceVolume`), so all three carry its name.
    //
    // Each control point is a state machine with a change counter, and a
    // rejected write becomes a real ATT Error Response. That is the part a
    // script cannot hand-build: Rhai has no way to fail an ATT write, so a
    // hand-rolled VCS silently accepts a stale change counter that a real
    // one rejects with 0x80.
    engine.register_fn(
        "add_vcs",
        |server: &mut ScriptGattServer,
         initial_volume: i64,
         initial_mute: i64|
         -> Result<(), Box<EvalAltResult>> {
            let volume = to_u8("add_vcs initial_volume", initial_volume)?;
            let mute = to_u8("add_vcs initial_mute", initial_mute)?;
            server.with_server(|s| {
                crate::profiles::VolumeControlService::register(
                    &mut s.device.gatt_db,
                    volume,
                    mute,
                );
            });
            Ok(())
        },
    );
    // One audio output's offset trim (VOCS). `audio_location` is a Bluetooth
    // Audio Location bitmap — 0x01 front left, 0x02 front right.
    engine.register_fn(
        "add_vocs",
        |server: &mut ScriptGattServer,
         audio_location: i64,
         description: &str|
         -> Result<(), Box<EvalAltResult>> {
            let location = u32::try_from(audio_location).map_err(|_| {
                runtime_error(format!(
                    "add_vocs audio_location: not a bitmap: {audio_location}"
                ))
            })?;
            server.with_server(|s| {
                crate::profiles::vocs::VolumeOffsetControlService::register(
                    &mut s.device.gatt_db,
                    location,
                    description,
                );
            });
            Ok(())
        },
    );
    // One audio input's gain (AICS). The input is typed `Bluetooth` and
    // `Active`, the case an LE Audio sink actually has; the gain range is the
    // script's, because that is the part that differs per device.
    engine.register_fn(
        "add_aics",
        |server: &mut ScriptGattServer,
         gain_minimum: i64,
         gain_maximum: i64,
         description: &str|
         -> Result<(), Box<EvalAltResult>> {
            let gain_settings_minimum = to_u8("add_aics gain_minimum", gain_minimum)?;
            let gain_settings_maximum = to_u8("add_aics gain_maximum", gain_maximum)?;
            if gain_settings_minimum > gain_settings_maximum {
                return Err(runtime_error(format!(
                    "add_aics: gain_minimum {gain_settings_minimum} exceeds gain_maximum {gain_settings_maximum}"
                )));
            }
            server.with_server(|s| {
                crate::profiles::aics::AudioInputControlService::register(
                    &mut s.device.gatt_db,
                    crate::profiles::aics::GainSettingsProperties {
                        gain_settings_units: 1,
                        gain_settings_minimum,
                        gain_settings_maximum,
                    },
                    crate::profiles::aics::AudioInputType::Bluetooth,
                    crate::profiles::aics::AudioInputStatus::Active,
                    description,
                );
            });
            Ok(())
        },
    );

    // ---- Coordinated sets: Android's `BluetoothCsipSetCoordinator` -------
    //
    // The half a script cannot build is the crypto: a set member is found by
    // resolving the Resolvable Set Identifier in its advertisement against a
    // SIRK, which is AES-CMAC and AES-128 (`csip::sih`). Rhai has neither.
    engine.register_fn(
        "add_csis",
        |server: &mut ScriptGattServer,
         sirk: Dynamic,
         set_size: i64,
         rank: i64|
         -> Result<(), Box<EvalAltResult>> {
            let sirk = to_key_128("add_csis sirk", sirk)?;
            let set_size = to_u8("add_csis set_size", set_size)?;
            let rank = to_u8("add_csis rank", rank)?;
            if rank == 0 || rank > set_size {
                return Err(runtime_error(format!(
                    "add_csis: rank {rank} is outside 1..={set_size} (CSIP Section 3.3: ranks are 1-based and unique within the set)"
                )));
            }
            server.with_server(|s| {
                crate::profiles::CoordinatedSetIdentificationService::register(
                    &mut s.device.gatt_db,
                    sirk,
                    set_size,
                    rank,
                );
            });
            Ok(())
        },
    );
    // Stages the Resolvable Set Identifier AD structure (0x2E) this member
    // advertises, so a coordinator holding the SIRK can recognise it. Without
    // this the device is discoverable but not identifiable as a set member,
    // which is the whole point of CSIP.
    engine.register_fn(
        "advertise_set_identity",
        |server: &mut ScriptGattServer,
         sirk: Dynamic,
         prand: Dynamic|
         -> Result<(), Box<EvalAltResult>> {
            let sirk = to_key_128("advertise_set_identity sirk", sirk)?;
            let prand_bytes = dynamic_to_bytes(prand)?;
            let prand = <[u8; 3]>::try_from(prand_bytes.as_slice()).map_err(|_| {
                runtime_error(format!(
                    "advertise_set_identity prand: expected 3 bytes, got {}",
                    prand_bytes.len()
                ))
            })?;
            let rsi = crate::profiles::csip::rsi(&sirk, &prand);
            server.with_server(|s| {
                s.device
                    .advertising_data
                    .get_or_insert_with(crate::gap::AdvertisingData::new)
                    .resolvable_set_identifier = Some(rsi.to_vec());
            });
            Ok(())
        },
    );

    // ---- Hearing Access: Android's `BluetoothHapClient` ------------------
    //
    // Presets are a list a client pages through with Read Presets Request and
    // switches with Set Active Preset, all over one control point that
    // *indicates* its responses. `presets` is an array of maps:
    //   #{ index: 1, name: "Universal", writable: true, available: true }
    engine.register_fn(
        "add_has",
        |server: &mut ScriptGattServer,
         features: i64,
         presets: Array|
         -> Result<(), Box<EvalAltResult>> {
            let features = to_u8("add_has features", features)?;
            let features = crate::profiles::hap::HearingAidFeatures::from_byte(features)
                .ok_or_else(|| {
                    runtime_error(format!(
                        "add_has features: 0x{features:02X} has no valid hearing-aid type in bits 0-1"
                    ))
                })?;
            // `HearingAccessService::register` panics on an empty list or an
            // over-long name. A panic in a script binding takes down the whole
            // engine, so the same conditions are checked here and returned as
            // script errors instead.
            if presets.is_empty() {
                return Err(runtime_error(
                    "add_has: needs at least one preset (HAS Section 2.8)".to_string(),
                ));
            }
            let mut records = Vec::with_capacity(presets.len());
            for (n, preset) in presets.into_iter().enumerate() {
                let map = preset.try_cast::<Map>().ok_or_else(|| {
                    runtime_error(format!("add_has preset {n}: expected a map"))
                })?;
                let get_int = |key: &str| -> Option<i64> {
                    map.get(key).and_then(|v| v.as_int().ok())
                };
                let index = to_u8(
                    &format!("add_has preset {n} index"),
                    get_int("index").ok_or_else(|| {
                        runtime_error(format!("add_has preset {n}: missing `index`"))
                    })?,
                )?;
                let name = map
                    .get("name")
                    .and_then(|v| v.clone().into_string().ok())
                    .ok_or_else(|| {
                        runtime_error(format!("add_has preset {n}: missing `name`"))
                    })?;
                if name.is_empty() || name.len() > 40 {
                    return Err(runtime_error(format!(
                        "add_has preset {n}: name is 1..=40 bytes (HAS Section 2.8), got {}",
                        name.len()
                    )));
                }
                let flag = |key: &str, default: bool| {
                    map.get(key).and_then(|v| v.as_bool().ok()).unwrap_or(default)
                };
                records.push(crate::profiles::hap::PresetRecord {
                    index,
                    writable: flag("writable", true),
                    available: flag("available", true),
                    name,
                });
            }
            server.with_server(|s| {
                crate::profiles::HearingAccessService::register(
                    &mut s.device.gatt_db,
                    features,
                    &records,
                );
            });
            Ok(())
        },
    );

    // ---- Media Control --------------------------------------------------
    //
    // Android has NO Bluetooth profile proxy for this. An app controls media
    // through `MediaSession`/`MediaController` and the Bluetooth stack bridges
    // to MCS internally, so there is no `BluetoothMediaControl` to mirror and
    // this binding is named for the service instead.
    //
    // `add_gmcs` registers the Generic Media Control Service (the device-wide
    // player a client finds without knowing any app); `add_mcs` registers a
    // per-player instance, identified by its Content Control ID.
    engine.register_fn(
        "add_gmcs",
        |server: &mut ScriptGattServer,
         player_name: &str,
         ccid: i64|
         -> Result<(), Box<EvalAltResult>> {
            let ccid = to_u8("add_gmcs ccid", ccid)?;
            server.with_server(|s| {
                crate::profiles::MediaControlService::register_generic(
                    &mut s.device.gatt_db,
                    player_name,
                    ccid,
                );
            });
            Ok(())
        },
    );
    engine.register_fn(
        "add_mcs",
        |server: &mut ScriptGattServer,
         player_name: &str,
         ccid: i64|
         -> Result<(), Box<EvalAltResult>> {
            let ccid = to_u8("add_mcs ccid", ccid)?;
            server.with_server(|s| {
                crate::profiles::MediaControlService::register(
                    &mut s.device.gatt_db,
                    player_name,
                    ccid,
                );
            });
            Ok(())
        },
    );

    // Adds a 16-bit UUID to the advertisement. Services registered by the
    // Rust profile registrars live in the GATT database rather than the
    // script's service list, so they need advertising explicitly.
    engine.register_fn(
        "advertise_service_uuid",
        |server: &mut ScriptGattServer, uuid16: i64| -> Result<(), Box<EvalAltResult>> {
            let uuid16 = u16::try_from(uuid16).map_err(|_| {
                runtime_error(format!(
                    "advertise_service_uuid: not a 16-bit uuid: {uuid16}"
                ))
            })?;
            server.with_server(|s| {
                s.device
                    .advertising_data
                    .get_or_insert_with(crate::gap::AdvertisingData::new)
                    .service_uuids_16
                    .push(uuid16);
            });
            Ok(())
        },
    );

    // The return path of the event channel: a script tells the host (a page,
    // a test) something that isn't GATT state — a log line, a decoded frame,
    // a state transition. Payloads cross as JSON.
    engine.register_fn(
        "emit",
        |server: &mut ScriptGattServer,
         kind: &str,
         payload: Dynamic|
         -> Result<(), Box<EvalAltResult>> {
            let value: serde_json::Value = rhai::serde::from_dynamic(&payload)
                .map_err(|e| runtime_error(format!("emit payload is not serializable: {e}")))?;
            let message = serde_json::json!({ "event": kind, "payload": value });
            server.push_emitted(message.to_string());
            Ok(())
        },
    );

    // The LE Audio media plane, script side. `send_audio` streams one SDU to
    // the connected peer; `take_audio` drains what a sink has received.
    engine.register_fn(
        "send_audio",
        |server: &mut ScriptGattServer, sdu: Dynamic| -> Result<bool, Box<EvalAltResult>> {
            let bytes = dynamic_to_bytes(sdu)?;
            Ok(server.with_server(|s| {
                let handle = s.device.audio_handle();
                match handle.and_then(|h| s.device.build_audio_packet(h, &bytes)) {
                    Some(packet) => {
                        s.device.audio_tx_pending.push(packet);
                        true
                    }
                    None => false,
                }
            }))
        },
    );
    engine.register_fn(
        "take_audio",
        |server: &mut ScriptGattServer| -> rhai::Array {
            server.with_server(|s| {
                s.device
                    .take_audio()
                    .into_iter()
                    .map(Dynamic::from_blob)
                    .collect()
            })
        },
    );
    engine.register_fn(
        "value",
        |server: &mut ScriptGattServer, uuid: Uuid| -> Result<Blob, Box<EvalAltResult>> {
            let handle = find_value_handle(server, uuid)
                .ok_or_else(|| runtime_error(format!("no characteristic with UUID {uuid}")))?;
            // Host-side read: a device sees its own attributes regardless of
            // client permissions, so a script can read the write-only control
            // point a peer just wrote.
            server
                .with_server(|s| s.device.gatt_db.value(handle).map(Blob::from))
                .ok_or_else(|| runtime_error(format!("no value for characteristic {uuid}")))
        },
    );
    // `catalog::*` and `assert_over`, which build on the bindings above; see
    // `crate::scripting::register_host_extensions`.
    crate::scripting::register_host_extensions(engine);
}

/// Runs a Rhai *test* script — one that builds devices and calls `assert(...)`
/// — in a fresh engine, returning `Ok(())` if every assertion passed or the
/// error message (a failed assert, or a compile/runtime error) otherwise.
///
/// Unlike `ScriptedPeripheral::run_script` this does not require the script to
/// build a server: a pure-assertion test is valid. The same script is a device,
/// a test, and a CI fixture — this is the runner for the "test" role.
pub fn run_test_script(script: &str) -> Result<(), String> {
    let mut engine = new_engine();
    register_web_extensions(&mut engine);
    let ast = engine
        .compile(script)
        .map_err(|e| format!("compile error: {e}"))?;
    let mut scope = Scope::new();
    engine
        .run_ast_with_scope(&mut scope, &ast)
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Lints a Rhai script without running it: compiles it in the same engine
/// [`run_test_script`] would use (so `android::*` / `uuid::*` and the test
/// bindings are all in scope) and reports a syntax/parse error if any, with
/// its position. Side-effect-free — nothing executes — so it's the cheap
/// pre-flight for an agent's generate-check-fix loop (`simble --no-run`).
pub fn lint_script(script: &str) -> Result<(), String> {
    let mut engine = new_engine();
    register_web_extensions(&mut engine);
    engine
        .compile(script)
        .map(|_| ())
        .map_err(|e| e.to_string())
}
