// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! The **scene file**: a JSON description of which devices exist, how they
//! are placed, and how they are wired to each other.
//!
//! Before this module a "scene" existed three incompatible ways — an array of
//! specs inside the web Scene page, a sequence of `add_peripheral` calls over
//! MCP, and nothing at all in CI. A file collapses those into one artifact
//! that can be committed, diffed, reviewed and replayed.
//!
//! # The one rule
//!
//! **Topology in JSON, behaviour in Rhai.** A scene declares what exists and
//! how it is wired; it never says *what happens*. There is no `at t=2s write
//! …, then assert …` — that is what a device script is for, and the same
//! script serves as device *and* test by adding `assert(...)`. Encoding
//! actions here would invent a second, worse scripting language in JSON.
//!
//! # What a device is versus where it is placed
//!
//! [`DeviceSpec`] keeps the two apart deliberately:
//!
//! - **What it is** — `device` (a name in [`crate::devices::catalog`]) or
//!   `script` (inline Rhai, pinned).
//! - **Where it is placed** — `address`, `name`, `role`, and the scene's
//!   `controller`.
//!
//! This is not cosmetic. A script carries a placeholder address; the on-air
//! address is stamped over it by `set_identity`, and SMP mixes that on-air
//! address into the pairing crypto. A scene that let a script own its address
//! would reproduce a bug this repo has already paid for.
//!
//! # Reading order
//!
//! [`Scene`] is the parsed file. [`Scene::resolve`] validates it and produces
//! a [`ResolvedScene`]: every address concrete, every `device` name looked up
//! to a script, every `target` id turned into a peer address, and every bond
//! materialized into a per-device [`MemoryBondStore`]. [`runner`] then hosts a
//! `ResolvedScene` on a controller.

#[cfg(not(target_arch = "wasm32"))]
pub mod runner;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use crate::device::{BondSecurity, BondStore, MemoryBondStore};
use crate::devices::catalog;
use crate::types::Address;

/// The only scene-file version this build understands. A file states its
/// version so a loader can refuse a future one outright rather than
/// silently ignoring fields it does not know.
pub const VERSION: u32 = 1;

/// The default encryption key size, in bytes, for a bond that does not state
/// one. 16 is the maximum and what a Secure Connections pairing negotiates in
/// practice, so it is the least surprising thing for a hand-written fixture.
const DEFAULT_KEY_SIZE: u8 = 16;

/// Deterministic scene address for the `n`th device (1-based): identical
/// input produces an identical device, which is what makes a committed
/// fixture reproducible and an agent loop converge.
///
/// `F0:DE:C0:…` has both top bits set, so it is a well-formed random-static
/// address rather than something a stack may reject.
pub fn auto_address(n: u16) -> Address {
    let [hi, lo] = n.to_be_bytes();
    Address::from_be_bytes([0xF0, 0xDE, 0xC0, 0x00, hi, lo])
}

// --- errors ----------------------------------------------------------------

/// Why a scene could not be loaded. Every message names the offending device
/// or bond, because "invalid scene" on a 40-line file is not a diagnosis.
#[derive(Debug, thiserror::Error)]
pub enum SceneError {
    /// The bytes are not the JSON this format expects. Carries serde's
    /// message, which includes the line and column.
    #[error("not a valid scene file: {0}")]
    Parse(String),
    /// The file states a version this build does not implement.
    #[error("scene version {found} is not supported (this simble reads version {VERSION})")]
    Version {
        /// The version the file declared.
        found: u32,
    },
    /// Something about one device is wrong.
    #[error("device {device:?}: {message}")]
    Device {
        /// The offending device's `id`.
        device: String,
        /// What is wrong with it.
        message: String,
    },
    /// Something about one bond is wrong.
    #[error("bond {between:?}: {message}")]
    Bond {
        /// The bond's `between` pair, rendered for the message.
        between: String,
        /// What is wrong with it.
        message: String,
    },
    /// Something about the scene as a whole is wrong.
    #[error("scene: {0}")]
    Scene(String),
    /// The scene is well-formed but this build cannot host it.
    #[error("{0}")]
    Unsupported(String),
}

impl SceneError {
    fn device(id: &str, message: impl Into<String>) -> Self {
        Self::Device {
            device: id.to_string(),
            message: message.into(),
        }
    }

    fn bond(between: &[String; 2], message: impl Into<String>) -> Self {
        Self::Bond {
            between: format!("{} <-> {}", between[0], between[1]),
            message: message.into(),
        }
    }
}

// --- roles -----------------------------------------------------------------

/// What a device *is* in the scene, independently of what it runs.
///
/// New roles are additive: add a variant, add its string here, and every
/// scene written before it keeps parsing. The loader instantiates only some
/// of these — see [`Role::is_instantiated`] and `docs/scene-format.md`. The
/// format deliberately accepts the whole vocabulary so a scene can be written
/// against a role that is still being built, and fails with a precise message
/// at instantiation time rather than a confusing one at parse time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Role {
    /// A GATT server that advertises and answers a central. The default.
    #[default]
    Peripheral,
    /// A GATT client that connects to `target` and discovers it.
    Central,
    /// A passive observer that collects advertising reports.
    Scanner,
    /// An LE Audio Unicast Client: configures `target`'s ASE and opens a CIS
    /// to stream to it (`device::cis_central` + `profiles::ascs_client`).
    AudioSource,
    /// A HID host that connects to `target` and consumes its input reports.
    HidHost,
    /// A hands-free car kit (Classic: HFP/A2DP) paired with `target`.
    CarKit,
}

impl Role {
    /// Every role name, in declaration order.
    pub const NAMES: &'static [&'static str] = &[
        "peripheral",
        "central",
        "scanner",
        "audio_source",
        "hid_host",
        "car_kit",
    ];

    /// The wire name used in a scene file.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Peripheral => "peripheral",
            Self::Central => "central",
            Self::Scanner => "scanner",
            Self::AudioSource => "audio_source",
            Self::HidHost => "hid_host",
            Self::CarKit => "car_kit",
        }
    }

    /// Whether the loader can actually bring this role up today. A role that
    /// is expressible but not instantiated is refused with a message naming
    /// what is missing, never silently skipped.
    pub fn is_instantiated(self) -> bool {
        matches!(self, Self::Peripheral | Self::Central | Self::Scanner)
    }

    /// Whether the role runs a Rhai device script (and so needs exactly one
    /// of `device` / `script`).
    pub fn is_scripted(self) -> bool {
        matches!(self, Self::Peripheral)
    }

    /// Whether the role drives a peer and therefore requires a `target`.
    pub fn needs_target(self) -> bool {
        matches!(
            self,
            Self::Central | Self::AudioSource | Self::HidHost | Self::CarKit
        )
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Role {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "peripheral" => Ok(Self::Peripheral),
            "central" => Ok(Self::Central),
            "scanner" => Ok(Self::Scanner),
            "audio_source" => Ok(Self::AudioSource),
            "hid_host" => Ok(Self::HidHost),
            "car_kit" => Ok(Self::CarKit),
            other => Err(format!(
                "unknown role {other:?} (known roles: {})",
                Role::NAMES.join(", ")
            )),
        }
    }
}

/// Where a scene's devices run. The names match the MCP `run_on` targets, so
/// a scene file and a `run_on` call cannot disagree about what "netsim"
/// means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Controller {
    /// This process hosts every device on an in-process radio: deterministic,
    /// no setup, and the only controller that can host centrals and scanners.
    #[default]
    InProcess,
    /// The Android emulator's netsim, one WebSocket per device. The far side
    /// (a phone, Bumble, another netsim client) plays the central.
    Netsim,
    /// A real USB dongle. Not wired yet.
    Usb,
}

impl Controller {
    /// Every controller name, in declaration order.
    pub const NAMES: &'static [&'static str] = &["self", "netsim", "usb"];

    /// The wire name used in a scene file.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InProcess => "self",
            Self::Netsim => "netsim",
            Self::Usb => "usb",
        }
    }
}

impl fmt::Display for Controller {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Controller {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "self" => Ok(Self::InProcess),
            "netsim" => Ok(Self::Netsim),
            "usb" => Ok(Self::Usb),
            other => Err(format!(
                "unknown controller {other:?} (known controllers: {})",
                Controller::NAMES.join(", ")
            )),
        }
    }
}

/// Serde for the string-valued enums above. Hand-written rather than derived
/// so an unknown value's error message lists the valid ones — serde's own
/// "unknown variant" message is fine, but this is the error a human hits most
/// often and it is worth spelling out.
macro_rules! string_enum_serde {
    ($t:ident) => {
        impl Serialize for $t {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $t {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let raw = String::deserialize(deserializer)?;
                $t::from_str(&raw).map_err(serde::de::Error::custom)
            }
        }
    };
}

string_enum_serde!(Role);
string_enum_serde!(Controller);

// --- the file ---------------------------------------------------------------

/// A parsed scene file.
///
/// `deny_unknown_fields` throughout: a misspelt `"adress"` is a typo whose
/// symptom would otherwise be a device that quietly advertises the wrong
/// identity, which is exactly the class of bug this project keeps paying for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scene {
    /// Format version. Must be [`VERSION`].
    pub version: u32,
    /// Human-readable scene name, for listings and log lines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// What the scene is for. Prose, ignored by the loader.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Where the devices run. Overridable at the command line, so a fixture
    /// committed as `netsim` can still be exercised in CI on `self`.
    #[serde(default)]
    pub controller: Controller,
    /// The devices, in the order they are brought up.
    pub devices: Vec<DeviceSpec>,
    /// Pre-existing bonds, so a scene can start where pairing already
    /// succeeded and exercise what comes *after* it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bonds: Vec<Bond>,
}

/// One device: what it is, and where it is placed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceSpec {
    /// Scene-local identifier. Unique, and what `target` and `bonds`
    /// reference — never an address, so addresses can change without
    /// breaking links.
    pub id: String,
    /// What this device is in the scene. Defaults to `peripheral`.
    #[serde(default)]
    pub role: Role,
    /// The node name the controller registers this device under (the label
    /// `netsim devices` prints). Placement, not identity: it does *not*
    /// change the device's advertised GATT name, which comes from the script.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The on-air address. Omitted means a deterministic scene address (see
    /// [`auto_address`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<Address>,
    /// A name in [`crate::devices::catalog`] — the scene stays small and the
    /// device improves underneath it. Mutually exclusive with `script`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// Inline Rhai, pinning an exact copy of the device. Mutually exclusive
    /// with `device`; use it for a CI fixture that must not drift.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
    /// The peer this device drives, by `id`. Required for client roles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Role-specific placement parameters — stream parameters for an audio
    /// source, for instance. Never device *behaviour*: that belongs in the
    /// script. Opaque to the loader; each role reads the keys it knows.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub config: Map<String, Value>,
}

/// One CCCD subscription to restore for a bonded peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CccdRecord {
    /// The CCCD's attribute handle in the *server's* database.
    pub handle: u16,
    /// The stored value: 1 = notifications, 2 = indications.
    pub value: u16,
}

/// A pre-existing bond between two devices.
///
/// A bond is a relationship, so it is declared once at scene level and
/// materialized into both devices' stores on load. `known_by` narrows that to
/// one side, because "the peer forgot the bond" is a real failure mode and a
/// scene should be able to state it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bond {
    /// The two device ids this bond is between.
    pub between: [String; 2],
    /// The key material and metadata both sides hold, unless a `sides` entry
    /// overrides it.
    #[serde(default)]
    pub security: BondSecurity,
    /// Subscriptions to restore on reconnect. Meaningful on the GATT-server
    /// side of the pair; applied to every side that remembers the bond unless
    /// a `sides` entry overrides it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cccds: Vec<CccdRecord>,
    /// Which sides remember the bond. Omitted means both — the symmetric
    /// case stays a one-liner. Listing one id is how a scene says the other
    /// side forgot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_by: Option<Vec<String>>,
    /// Per-side overrides, keyed by device id. Each field given here replaces
    /// the shared one for that side. This is where genuinely asymmetric
    /// material goes: an IRK is per *device*, so the record A holds about B
    /// carries B's IRK, not A's.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sides: BTreeMap<String, BondSide>,
}

/// One side's overrides within a [`Bond`].
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BondSide {
    /// Replaces the bond's shared `security` for this side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security: Option<BondSecurity>,
    /// Replaces the bond's shared `cccds` for this side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cccds: Option<Vec<CccdRecord>>,
}

// --- resolution -------------------------------------------------------------

/// One device with everything looked up: a concrete address, the script text
/// itself rather than a catalog name, the peer's address rather than its id,
/// and a bond store already holding whatever the scene said this device
/// remembers.
#[derive(Debug)]
pub struct Placement {
    /// The scene-local id, kept for messages and for bond lookups.
    pub id: String,
    /// What this device is.
    pub role: Role,
    /// The on-air address, explicit or auto-assigned.
    pub address: Address,
    /// The controller-side node name, if the scene named one.
    pub node_name: Option<String>,
    /// The Rhai source, for scripted roles.
    pub script: Option<String>,
    /// The peer this device drives: its id and its resolved address.
    pub target: Option<(String, Address)>,
    /// Role-specific placement parameters, verbatim.
    pub config: Map<String, Value>,
    /// The bonds this device remembers, keyed by peer identity address —
    /// exactly the runtime type `VirtualDevice` consults on reconnection.
    pub bonds: MemoryBondStore,
}

impl Placement {
    /// How many bonded peers this device's store holds.
    pub fn bonded_peers(&self) -> usize {
        self.bonds.peers().len()
    }
}

/// A validated scene, ready to host.
#[derive(Debug)]
pub struct ResolvedScene {
    /// The scene's name, if it had one.
    pub name: Option<String>,
    /// Where it runs.
    pub controller: Controller,
    /// The devices, in file order.
    pub devices: Vec<Placement>,
}

impl Scene {
    /// Parses a scene from JSON. Structure only — see [`Self::resolve`] for
    /// the checks that need the whole scene in view.
    pub fn from_json(text: &str) -> Result<Self, SceneError> {
        let mut scene: Self =
            serde_json::from_str(text).map_err(|e| SceneError::Parse(e.to_string()))?;
        // An omitted key size means "the maximum", and normalizing here (not
        // at use time) keeps a round-tripped file explicit about it.
        for bond in &mut scene.bonds {
            normalize_key_size(&mut bond.security);
            for side in bond.sides.values_mut() {
                if let Some(security) = side.security.as_mut() {
                    normalize_key_size(security);
                }
            }
        }
        Ok(scene)
    }

    /// Serializes the scene back to pretty JSON — the save half of the
    /// round trip, and what a future `save_scene` would emit.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("a Scene is always serializable")
    }

    /// Validates the scene and resolves every reference: catalog names to
    /// scripts, ids to addresses, bonds to per-device stores.
    pub fn resolve(&self) -> Result<ResolvedScene, SceneError> {
        if self.version != VERSION {
            return Err(SceneError::Version {
                found: self.version,
            });
        }
        if self.devices.is_empty() {
            return Err(SceneError::Scene(
                "no devices — a scene must declare at least one".to_string(),
            ));
        }

        let addresses = self.resolve_addresses()?;
        let mut stores = self.resolve_bonds(&addresses)?;

        let mut devices = Vec::with_capacity(self.devices.len());
        for spec in &self.devices {
            let script = self.resolve_script(spec)?;
            let target = self.resolve_target(spec, &addresses)?;
            if let Some(name) = spec.name.as_deref()
                && name.trim().is_empty()
            {
                return Err(SceneError::device(&spec.id, "name is empty"));
            }
            devices.push(Placement {
                id: spec.id.clone(),
                role: spec.role,
                address: addresses[&spec.id],
                node_name: spec.name.clone(),
                script,
                target,
                config: spec.config.clone(),
                bonds: stores.remove(&spec.id).unwrap_or_default(),
            });
        }

        Ok(ResolvedScene {
            name: self.name.clone(),
            controller: self.controller,
            devices,
        })
    }

    /// Checks ids, then hands every device a concrete address — the one it
    /// declared, or a deterministic scene address. Collisions are an error
    /// either way: two devices at one address is always a bug, and on the air
    /// it looks like an intermittent one.
    fn resolve_addresses(&self) -> Result<HashMap<String, Address>, SceneError> {
        let mut by_id: HashMap<String, Address> = HashMap::new();
        let mut seen_ids: HashSet<&str> = HashSet::new();
        let mut used: HashMap<Address, String> = HashMap::new();

        for spec in &self.devices {
            validate_id(&spec.id)?;
            if !seen_ids.insert(spec.id.as_str()) {
                return Err(SceneError::device(
                    &spec.id,
                    "duplicate id — ids must be unique within a scene",
                ));
            }
        }
        // Explicit addresses are claimed first, so an auto-assigned one can
        // step around them instead of colliding by accident of ordering.
        for spec in &self.devices {
            if let Some(address) = spec.address {
                if let Some(other) = used.insert(address, spec.id.clone()) {
                    return Err(SceneError::device(
                        &spec.id,
                        format!("address {address} is already used by device {other:?}"),
                    ));
                }
                by_id.insert(spec.id.clone(), address);
            }
        }
        let mut next = 1u16;
        for spec in &self.devices {
            if spec.address.is_some() {
                continue;
            }
            let address = loop {
                let candidate = auto_address(next);
                next += 1;
                if !used.contains_key(&candidate) {
                    break candidate;
                }
            };
            used.insert(address, spec.id.clone());
            by_id.insert(spec.id.clone(), address);
        }
        Ok(by_id)
    }

    /// Turns `device` / `script` into the script text, enforcing that a role
    /// gets exactly the source it needs and no more.
    fn resolve_script(&self, spec: &DeviceSpec) -> Result<Option<String>, SceneError> {
        match (spec.device.as_deref(), spec.script.as_deref()) {
            (Some(_), Some(_)) => Err(SceneError::device(
                &spec.id,
                "has both \"device\" and \"script\" — name a catalog device or inline one, \
                 not both",
            )),
            (Some(name), None) => match catalog::script(name) {
                Some(script) => Ok(Some(script.to_string())),
                None => Err(SceneError::device(
                    &spec.id,
                    format!(
                        "unknown device {name:?} — the catalog has: {}",
                        catalog::names_joined()
                    ),
                )),
            },
            (None, Some(script)) => {
                if script.trim().is_empty() {
                    return Err(SceneError::device(&spec.id, "\"script\" is empty"));
                }
                Ok(Some(script.to_string()))
            }
            (None, None) if spec.role.is_scripted() => Err(SceneError::device(
                &spec.id,
                format!(
                    "role {} needs a device: give it \"device\": \"<catalog name>\" or an \
                     inline \"script\"",
                    spec.role
                ),
            )),
            (None, None) => Ok(None),
        }
    }

    /// Resolves `target` to a peer address, enforcing that only client roles
    /// have one.
    fn resolve_target(
        &self,
        spec: &DeviceSpec,
        addresses: &HashMap<String, Address>,
    ) -> Result<Option<(String, Address)>, SceneError> {
        match spec.target.as_deref() {
            Some(target) if !spec.role.needs_target() => Err(SceneError::device(
                &spec.id,
                format!(
                    "role {} does not connect to anything, so it cannot have a \"target\" (found {target:?})",
                    spec.role
                ),
            )),
            Some(target) if target == spec.id => {
                Err(SceneError::device(&spec.id, "targets itself"))
            }
            Some(target) => match addresses.get(target) {
                Some(&address) => Ok(Some((target.to_string(), address))),
                None => Err(SceneError::device(
                    &spec.id,
                    format!(
                        "target {target:?} is not a device in this scene (have: {})",
                        self.device_ids().join(", ")
                    ),
                )),
            },
            None if spec.role.needs_target() => Err(SceneError::device(
                &spec.id,
                format!("role {} drives a peer, so it needs a \"target\"", spec.role),
            )),
            None => Ok(None),
        }
    }

    /// Materializes every bond into per-device stores keyed by the *peer's*
    /// address, which is how [`crate::device::BondStore`] is keyed at runtime.
    fn resolve_bonds(
        &self,
        addresses: &HashMap<String, Address>,
    ) -> Result<HashMap<String, MemoryBondStore>, SceneError> {
        let mut stores: HashMap<String, MemoryBondStore> = HashMap::new();
        let mut pairs: HashSet<[&str; 2]> = HashSet::new();

        for bond in &self.bonds {
            let [a, b] = &bond.between;
            if a == b {
                return Err(SceneError::bond(
                    &bond.between,
                    "a device cannot be bonded to itself",
                ));
            }
            for id in &bond.between {
                if !addresses.contains_key(id) {
                    return Err(SceneError::bond(
                        &bond.between,
                        format!("{id:?} is not a device in this scene"),
                    ));
                }
            }
            // Order-independent, so one file cannot declare the same
            // relationship twice with different keys.
            let mut key = [a.as_str(), b.as_str()];
            key.sort_unstable();
            if !pairs.insert(key) {
                return Err(SceneError::bond(
                    &bond.between,
                    "declared twice — one bond per pair of devices",
                ));
            }

            for id in bond.sides.keys() {
                if !bond.between.contains(id) {
                    return Err(SceneError::bond(
                        &bond.between,
                        format!("\"sides\" names {id:?}, which is not one of the two devices"),
                    ));
                }
            }
            let known_by: Vec<&String> = match bond.known_by.as_ref() {
                Some(list) => {
                    if list.is_empty() {
                        return Err(SceneError::bond(
                            &bond.between,
                            "\"known_by\" is empty — a bond neither side remembers is not a \
                             bond; delete it",
                        ));
                    }
                    for id in list {
                        if !bond.between.contains(id) {
                            return Err(SceneError::bond(
                                &bond.between,
                                format!(
                                    "\"known_by\" names {id:?}, which is not one of the two \
                                     devices"
                                ),
                            ));
                        }
                    }
                    list.iter().collect()
                }
                None => bond.between.iter().collect(),
            };

            for id in known_by {
                let peer = if id == a { b } else { a };
                let side = bond.sides.get(id);
                let security = side
                    .and_then(|s| s.security.clone())
                    .unwrap_or_else(|| bond.security.clone());
                validate_security(&bond.between, &security)?;
                let cccds = side
                    .and_then(|s| s.cccds.clone())
                    .unwrap_or_else(|| bond.cccds.clone());

                let store = stores.entry(id.clone()).or_default();
                store.store_security(addresses[peer], security);
                for record in &cccds {
                    if record.handle == 0 {
                        return Err(SceneError::bond(
                            &bond.between,
                            "CCCD handle 0 is not a valid attribute handle",
                        ));
                    }
                    if record.value == 0 {
                        return Err(SceneError::bond(
                            &bond.between,
                            format!(
                                "CCCD handle {} has value 0, which records nothing — an \
                                 unsubscribed CCCD and an absent one are indistinguishable on \
                                 restore, so omit it instead",
                                record.handle
                            ),
                        ));
                    }
                    store.store_cccd(addresses[peer], record.handle, record.value);
                }
            }
        }
        Ok(stores)
    }

    /// Every device id, in file order — for "have: …" error tails.
    fn device_ids(&self) -> Vec<&str> {
        self.devices.iter().map(|d| d.id.as_str()).collect()
    }
}

/// Ids travel into netsim node names and log lines, so they are restricted to
/// characters that survive a URL query string unescaped.
fn validate_id(id: &str) -> Result<(), SceneError> {
    if id.is_empty() {
        return Err(SceneError::Scene("a device has an empty id".to_string()));
    }
    if let Some(bad) = id
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '_' || *c == '-' || *c == '.'))
    {
        return Err(SceneError::device(
            id,
            format!("id contains {bad:?} — ids may use letters, digits, '_', '-' and '.'"),
        ));
    }
    Ok(())
}

fn normalize_key_size(security: &mut BondSecurity) {
    if security.key_size == 0 {
        security.key_size = DEFAULT_KEY_SIZE;
    }
}

fn validate_security(between: &[String; 2], security: &BondSecurity) -> Result<(), SceneError> {
    if !(7..=16).contains(&security.key_size) {
        return Err(SceneError::bond(
            between,
            format!(
                "key_size {} is outside the spec's 7..=16 range",
                security.key_size
            ),
        ));
    }
    let keys = &security.keys;
    if keys.ltk.is_none() && keys.ltk_central.is_none() && keys.ltk_peripheral.is_none() {
        return Err(SceneError::bond(
            between,
            "no long-term key — a bond without an LTK cannot start encryption; give \
             keys.ltk (Secure Connections) or keys.ltk_central / keys.ltk_peripheral (legacy)",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
