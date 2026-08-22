// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Hosting a [`ResolvedScene`] on a controller: the headless half of the web
//! Scene page, and what `simble scene.json` runs.
//!
//! Two controllers, two clocks. On `self` the whole scene lives in this
//! process on a simulated radio, so time is advanced in fixed simulated steps
//! and a run is deterministic — the same file produces the same run, which is
//! what makes it usable as a CI fixture. On `netsim` the far side is a real
//! emulator with its own clock, so the loop is paced against the wall and the
//! scene is pumped between ticks, exactly as the MCP server's actor loop does.
//!
//! What a run *proves* is deliberately modest: every device compiled, came up,
//! and reported no error. Assertions live in the device scripts (a failing
//! `assert(...)` fails the script, and the device never instantiates), and
//! nothing here invents a second place to put them.

use std::time::{Duration, Instant};

use super::{Controller, Placement, ResolvedScene, Role, SceneError};
use crate::transport::netsim::{self, NetsimScene};
use crate::transport::wasm_ws::SceneEngine;

/// How long and how finely to run. Deliberately *not* part of the scene file:
/// a scene declares topology, and how long you care to watch it is a property
/// of the run, not of the scene.
#[derive(Debug, Clone, Copy)]
pub struct RunOptions {
    /// How long to run, in seconds — simulated seconds on `self`, wall
    /// seconds on `netsim`.
    pub seconds: f64,
    /// The step between ticks, in milliseconds.
    pub tick_ms: u64,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            seconds: 2.0,
            tick_ms: 100,
        }
    }
}

/// What one device did during a run.
#[derive(Debug, Clone)]
pub struct DeviceOutcome {
    /// The scene-local id.
    pub id: String,
    /// What it was.
    pub role: Role,
    /// Its on-air address.
    pub address: String,
    /// The device's own name, once its script has run.
    pub name: Option<String>,
    /// The last error the device recorded, if any. A device with one has
    /// failed the run.
    pub error: Option<String>,
    /// Bond records this device's store was given but that could not be
    /// installed (see [`RunReport::bonds_not_installed`]).
    pub bonds_declared: usize,
}

/// The result of hosting a scene.
#[derive(Debug, Clone)]
pub struct RunReport {
    /// Where it ran.
    pub controller: Controller,
    /// One entry per device, in scene order.
    pub devices: Vec<DeviceOutcome>,
    /// Seconds of scene clock covered.
    pub elapsed: f64,
}

impl RunReport {
    /// Whether every device came up and finished without recording an error —
    /// the run's exit status.
    pub fn ok(&self) -> bool {
        self.devices.iter().all(|d| d.error.is_none())
    }

    /// How many bond records the scene declared that this build could not
    /// install into a running device. Non-zero means the scene is expressing
    /// more than the loader materializes, and a caller should say so rather
    /// than let it pass silently.
    pub fn bonds_not_installed(&self) -> usize {
        self.devices.iter().map(|d| d.bonds_declared).sum()
    }
}

/// Hosts `scene` on its controller for the given duration.
pub fn run(scene: &ResolvedScene, options: &RunOptions) -> Result<RunReport, SceneError> {
    match scene.controller {
        Controller::InProcess => run_in_process(scene, options),
        Controller::Netsim => run_on_netsim(scene, options),
        Controller::Usb => Err(SceneError::Unsupported(
            "controller \"usb\" is not wired yet — use \"self\" or \"netsim\", or bridge a \
             dongle with `simble --usb` and point netsim at it"
                .to_string(),
        )),
    }
}

/// Refuses a role the format can express but this build cannot bring up. The
/// message names what is missing, so "expressible but not instantiated" never
/// looks like a bug in the scene file.
fn unsupported_role(device: &Placement, controller: Controller) -> SceneError {
    let detail = match device.role {
        Role::AudioSource => {
            "the LE Audio source role is being built (device::cis_central + \
             profiles::ascs_client); the scene format accepts it, the loader does not host it yet"
        }
        Role::HidHost => {
            "the HID host role is being built (device::hid_host); the scene format accepts it, \
             the loader does not host it yet"
        }
        Role::CarKit => {
            "the car-kit role is being built on the Classic stack; the scene format accepts it, \
             the loader does not host it yet"
        }
        Role::Central | Role::Scanner => {
            "on netsim the scene is peripheral-only: the emulator (or another netsim client) \
             plays the central, so scan and connect from there. Run this scene on \"self\" to \
             host centrals and scanners in-process"
        }
        Role::Peripheral => "unreachable: peripherals are hosted on every controller",
    };
    SceneError::Unsupported(format!(
        "device {:?}: role {} cannot run on controller {} — {}",
        device.id, device.role, controller, detail
    ))
}

fn run_in_process(scene: &ResolvedScene, options: &RunOptions) -> Result<RunReport, SceneError> {
    let mut engine = SceneEngine::new();
    let mut indices = Vec::with_capacity(scene.devices.len());

    for device in &scene.devices {
        let index = match device.role {
            Role::Peripheral => {
                let script = device.script.as_deref().unwrap_or_default();
                engine
                    .add_peripheral(device.address, script)
                    .map_err(|e| SceneError::device(&device.id, format!("script rejected: {e}")))?
            }
            Role::Scanner => engine.add_scanner(device.address),
            Role::Central => {
                // `resolve` has already proved a central has a target.
                let (_, target) = device.target.as_ref().expect("central without a target");
                engine.add_central(device.address, *target)
            }
            _ => return Err(unsupported_role(device, Controller::InProcess)),
        };
        indices.push(index);
    }

    let step = options.tick_ms as f64 / 1000.0;
    let mut t = 0.0;
    while t < options.seconds {
        t += step;
        engine.tick(t);
    }

    let devices = scene
        .devices
        .iter()
        .zip(&indices)
        .map(|(device, &index)| {
            let status = engine.peripheral_status_json(index);
            outcome(device, status.as_deref())
        })
        .collect();
    Ok(RunReport {
        controller: Controller::InProcess,
        devices,
        elapsed: t,
    })
}

fn run_on_netsim(scene: &ResolvedScene, options: &RunOptions) -> Result<RunReport, SceneError> {
    let mut netsim_scene = NetsimScene::new(netsim::DEFAULT_WS_URL);
    let mut indices = Vec::with_capacity(scene.devices.len());

    for device in &scene.devices {
        if device.role != Role::Peripheral {
            return Err(unsupported_role(device, Controller::Netsim));
        }
        let script = device.script.as_deref().unwrap_or_default();
        let index = netsim_scene
            .add_peripheral_named(device.address, script, device.node_name.as_deref())
            .map_err(|e| SceneError::device(&device.id, e))?;
        // Pump immediately so the device's HCI bring-up reaches netsim now
        // rather than at the first tick — a device that has not transmitted
        // does not appear in `netsim devices`.
        netsim_scene.pump();
        indices.push(index);
    }

    // Paced against the wall clock: the far side is a real emulator, and
    // running the loop flat out would spin a core without letting netsim
    // deliver anything between ticks.
    let step = Duration::from_millis(options.tick_ms.max(1));
    let deadline = Instant::now() + Duration::from_secs_f64(options.seconds);
    let mut t = 0.0;
    while Instant::now() < deadline {
        std::thread::sleep(step);
        t += step.as_secs_f64();
        netsim_scene.tick(step.as_secs_f64());
    }

    let devices = scene
        .devices
        .iter()
        .zip(&indices)
        .map(|(device, &index)| {
            let status = netsim_scene.peripheral_status_json(index);
            outcome(device, status.as_deref())
        })
        .collect();
    Ok(RunReport {
        controller: Controller::Netsim,
        devices,
        elapsed: t,
    })
}

/// Reads a device's post-run status JSON into an outcome. Scanners and
/// centrals have no peripheral status, which is not a failure.
fn outcome(device: &Placement, status_json: Option<&str>) -> DeviceOutcome {
    let status: Option<serde_json::Value> =
        status_json.and_then(|s| serde_json::from_str(s).ok());
    let field = |key: &str| -> Option<String> {
        status
            .as_ref()?
            .get(key)?
            .as_str()
            .map(std::string::ToString::to_string)
    };
    DeviceOutcome {
        id: device.id.clone(),
        role: device.role,
        address: device.address.to_string(),
        name: field("name").filter(|n| !n.is_empty()),
        error: field("last_error"),
        bonds_declared: device.bonded_peers(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::Scene;

    fn scene_from(json: &str) -> ResolvedScene {
        Scene::from_json(json).unwrap().resolve().unwrap()
    }

    #[test]
    fn a_catalog_peripheral_comes_up_and_reports_its_own_name() {
        let scene = scene_from(
            r#"{ "version": 1, "devices": [ { "id": "hr", "device": "hrm" } ] }"#,
        );
        let report = run(&scene, &RunOptions::default()).unwrap();
        assert!(report.ok(), "{report:?}");
        assert_eq!(report.devices[0].name.as_deref(), Some("HRM"));
        assert_eq!(report.devices[0].error, None);
    }

    #[test]
    fn a_central_discovers_the_peripheral_it_targets() {
        // The point of `target`: a scene wires two devices together and the
        // link actually forms, with no imperative step anywhere.
        let scene = scene_from(
            r#"{
                 "version": 1,
                 "devices": [
                   { "id": "hr", "device": "hrm" },
                   { "id": "phone", "role": "central", "target": "hr" }
                 ]
               }"#,
        );
        let report = run(&scene, &RunOptions::default()).unwrap();
        assert!(report.ok(), "{report:?}");
        assert_eq!(report.devices.len(), 2);
        assert_eq!(report.devices[1].role, Role::Central);
    }

    #[test]
    fn a_failing_assertion_in_a_device_script_fails_the_run() {
        // A device script is also a test; the scene must not paper over one
        // that does not hold.
        let scene = scene_from(
            r#"{ "version": 1, "devices": [
                   { "id": "bad", "script": "assert(1 == 2, \"one is not two\");" } ] }"#,
        );
        let error = run(&scene, &RunOptions::default()).unwrap_err();
        assert!(
            error.to_string().contains("one is not two"),
            "the script's own message must survive: {error}"
        );
    }

    #[test]
    fn a_role_the_format_expresses_but_the_loader_cannot_host_is_refused_by_name() {
        let scene = scene_from(
            r#"{ "version": 1, "devices": [
                   { "id": "sink", "device": "volume" },
                   { "id": "src", "role": "audio_source", "target": "sink" } ] }"#,
        );
        let error = run(&scene, &RunOptions::default()).unwrap_err().to_string();
        assert!(error.contains("audio_source"), "{error}");
        assert!(error.contains("cis_central"), "{error}");
    }

    #[test]
    fn netsim_refuses_a_central_because_the_far_side_plays_that_part() {
        let scene = scene_from(
            r#"{ "version": 1, "controller": "netsim", "devices": [
                   { "id": "hr", "device": "hrm" },
                   { "id": "phone", "role": "central", "target": "hr" } ] }"#,
        );
        let error = run(&scene, &RunOptions::default()).unwrap_err().to_string();
        assert!(error.contains("peripheral-only"), "{error}");
    }

    #[test]
    fn a_usb_scene_says_what_to_do_instead_of_failing_obscurely() {
        let scene = scene_from(
            r#"{ "version": 1, "controller": "usb", "devices": [ { "id": "hr", "device": "hrm" } ] }"#,
        );
        let error = run(&scene, &RunOptions::default()).unwrap_err().to_string();
        assert!(error.contains("not wired yet"), "{error}");
    }

    #[test]
    fn the_same_scene_run_twice_produces_the_same_devices() {
        // Determinism is what makes a scene file usable as a CI fixture:
        // addresses come from the file (or from the deterministic allocator),
        // never from a counter that survives across runs.
        let json = r#"{ "version": 1, "devices": [
                         { "id": "a", "device": "battery" },
                         { "id": "b", "device": "hrm" } ] }"#;
        let first = run(&scene_from(json), &RunOptions::default()).unwrap();
        let second = run(&scene_from(json), &RunOptions::default()).unwrap();
        let addresses = |r: &RunReport| -> Vec<String> {
            r.devices.iter().map(|d| d.address.clone()).collect()
        };
        assert_eq!(addresses(&first), addresses(&second));
    }
}
