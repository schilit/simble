// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! SimBLE v1 — the control/observability protocol (see `docs/controller-routing.md`).
//!
//! This is the message layer and the dispatch over it, transport-neutral: a node
//! reads a `Request`, calls `dispatch`, and writes a `Response`. Wiring it onto a
//! ws:// socket (control is JSON, raw HCI is binary H4) is a thin layer on top
//! and lives with whichever node serves it.
//!
//! It is SimBLE's *own* first protocol; the netsim ws:// protocol it can also
//! speak for compatibility is netsim's, not an earlier version of this one.
//!
//! Implemented: the four observability lists (`list_controllers` /
//! `list_networks` / `list_devices` / `list_nodes`) and the verbs `run`, `stop`,
//! `send`, `route`, `create`, `register`, `tick`, and `get_clock` — over a
//! multi-controller `Node` (the *device* router), runnable on the deterministic
//! `link` controller with no hardware. `run` is persistent and returns a stable
//! device handle, so it already is what the design's `spawn` was for — there is
//! no separate `spawn`. The one remaining verb, `attach` (a raw H4 HCI stream for
//! an external stack), and the async *backend* router (routing raw HCI to
//! `rootcanal-rs`/netsim) are the separate crate in `docs/controller-routing.md`.

use serde::{Deserialize, Serialize};

/// A controller in the `/v1/controllers` list: an attach point a device runs on.
/// The `api_class` gates what a run may do; the `network` says which world it
/// drops a device into. See `docs/controller-routing.md` for the full model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Controller {
    /// Stable name within a session, e.g. `link` or `dongle-0`.
    pub name: String,
    /// `link` | `usb` | `rootcanal` | `netsim` | `android` | `iphone`.
    pub kind: String,
    /// The platform interface used to drive the radio: `hci` | `android` |
    /// `coreBluetooth`. Only `hci` allows attach and low-level control.
    pub api_class: String,
    /// The world this controller drops a device into (who-hears-whom).
    pub network: String,
    /// True on a real radio (a dongle, a phone), false for a simulated ether.
    pub real: bool,
    /// True when the medium is deterministic (`tick()`-driven), i.e. `Link`.
    pub deterministic: bool,
    /// Whether a host can attach a raw HCI stream (only `hci` controllers).
    pub attachable: bool,
    /// A human-readable product string, when the controller has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product: Option<String>,
}

/// A running device in the `/v1/devices` list: an instance of a script on a
/// controller.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Device {
    /// The device's index on its controller.
    pub index: usize,
    /// The controller (its route) this device runs on.
    pub controller: String,
    /// Render-ready status, if the controller reports it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<serde_json::Value>,
}

/// A world in the `/v1/networks` list: an ether, i.e. who-hears-whom. Sim ethers
/// (`link`, `rootcanal`) are created/isolated; `real` is the one shared physical
/// world; `netsim` is a leaf forward. Membership is read off `/v1/devices` (each
/// device's controller carries its `network`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Network {
    /// `link` | `real` | a `rootcanal` net name | `netsim`.
    pub name: String,
    /// `link` | `rf` | `rootcanal` | `netsim`.
    pub kind: String,
    /// True when the medium is deterministic (`tick()`-driven), i.e. `link`.
    pub deterministic: bool,
    /// True for the physical air, false for a simulated ether.
    pub real: bool,
    /// True for `real`: not creatable/isolatable, shared with all real radios.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub shared: bool,
    /// True for `netsim`: forward to it, never through it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub leaf: bool,
}

/// A participant in the `/v1/nodes` list: something that owns controllers and
/// executes runs (the local `simble` node, a phone, a browser).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeInfo {
    /// `local` for the process serving this list; a phone/browser otherwise.
    pub name: String,
    /// `router` | `android` | `iphone` | `browser`.
    pub kind: String,
    /// The names of the controllers this node owns.
    pub controllers: Vec<String>,
}

/// A v1 control request. Tagged by `op` on the wire: `{"op":"list_controllers"}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Enumerate the controllers this node offers.
    ListControllers,
    /// Enumerate the worlds (ethers) this node offers.
    ListNetworks,
    /// Enumerate the devices currently running on this node.
    ListDevices,
    /// Enumerate the participants (nodes).
    ListNodes,
    /// Run `script` as a device on `controller`. `address` is optional — a
    /// deterministic one is allocated when absent. Persistent until stopped.
    Run {
        /// The controller to run on (`netsim`, `dongle-0`, …).
        controller: String,
        /// The Rhai device source.
        script: String,
        /// An optional BD_ADDR, e.g. `CC:1E:57:00:00:06`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        address: Option<String>,
    },
    /// Advance this node's simulated clock by `advance_us` and pump once, so a
    /// deterministic scene makes progress without waiting on a wall clock.
    Tick {
        /// How far to advance, in microseconds (BLE/HCI timing is exact integers
        /// there: 625 µs slots, 1250 µs connection-interval units).
        advance_us: u64,
    },
    /// Read this node's clock: the time now and when it next needs attention, so
    /// the caller can wait *until* the deadline instead of spinning `tick`.
    GetClock,
    /// Stop (tear down) the running device at `device`, releasing its place on
    /// the controller. Its index stays a valid handle but the device goes inert.
    Stop {
        /// The device index to stop (from `run` / `list_devices`).
        device: usize,
    },
    /// Deliver an input `event` to the running device at `device` — its script
    /// handles it in `fn on_event(server, event)` on the next tick. This is how a
    /// caller triggers or mutates a device the script chose to expose.
    Send {
        /// The target device index.
        device: usize,
        /// The event name the script matches on (`event.event == …`).
        event: String,
        /// An optional JSON payload, merged into the event the script sees.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<serde_json::Value>,
    },
    /// Rebind the running device `device` onto a different `controller`, keeping
    /// its handle. Drops and re-runs (never migrates live state) — the
    /// deterministic analog of a controller switch injecting an HCI Hardware
    /// Error, so the device re-initialises on the new controller/world.
    Route {
        /// The device to move (from `run` / `list_devices`).
        device: usize,
        /// The controller to move it to (built-in or `create`d).
        controller: String,
    },
    /// Mint a private sim ether named `network` — a fresh, isolated `link` world
    /// that `run`/`route` can target. Networks are *created* (internal), while
    /// nodes are *registered*.
    Create {
        /// A name for the new ether, unique among this node's controllers.
        network: String,
    },
    /// Admit an external `node` (a phone, a browser) into this node's view, so
    /// `list_nodes` reflects it. Bookkeeping only — this node does not drive it;
    /// cross-node orchestration is the client's job.
    Register {
        /// The participant to admit.
        node: NodeInfo,
    },
}

/// A v1 control response. Tagged by `type` on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    /// The answer to `list_controllers`.
    Controllers {
        /// The controllers this node offers, in a stable order.
        controllers: Vec<Controller>,
    },
    /// The answer to `list_networks`.
    Networks {
        /// The worlds this node offers.
        networks: Vec<Network>,
    },
    /// The answer to `list_devices`.
    Devices {
        /// The devices currently running.
        devices: Vec<Device>,
    },
    /// The answer to `list_nodes`.
    Nodes {
        /// The participants.
        nodes: Vec<NodeInfo>,
    },
    /// The answer to `run`: the device is running.
    Ran {
        /// The new device's index on its controller.
        device: usize,
        /// The controller it runs on.
        controller: String,
    },
    /// The answer to `stop`: the device was torn down.
    Stopped {
        /// The stopped device's index (still a valid handle, now inert).
        device: usize,
    },
    /// The answer to `send`: the event was queued for the device's script.
    Sent {
        /// The device the event was delivered to.
        device: usize,
    },
    /// The answer to `route`: the device now runs on `controller`.
    Routed {
        /// The routed device (its handle is unchanged).
        device: usize,
        /// The controller it now runs on.
        controller: String,
    },
    /// The answer to `create`: the private ether now exists.
    Created {
        /// The new network's name.
        network: String,
    },
    /// The answer to `register`: the external node was admitted.
    Registered {
        /// The registered node's name.
        node: String,
    },
    /// The answer to `tick`: the node's clock after advancing, and the absolute
    /// clock of the next scheduled event — a deadline to wait *until* instead of
    /// spinning. Both are microseconds.
    Ticked {
        /// The node's simulated clock now, in microseconds.
        now_us: u64,
        /// The absolute clock (µs) of the next scheduled event, or `null` if
        /// nothing is scheduled (wait for a packet, or poll).
        #[serde(skip_serializing_if = "Option::is_none")]
        deadline_us: Option<u64>,
    },
    /// The answer to `get_clock`: the clock now and the absolute clock of the
    /// next scheduled event, both microseconds; `deadline_us` is `null` when
    /// nothing is scheduled (wait for a packet, or poll).
    Clock {
        /// The node's simulated clock now, in microseconds.
        now_us: u64,
        /// The absolute clock (µs) of the next event, or `None`.
        deadline_us: Option<u64>,
    },
    /// A request that could not be served.
    Error {
        /// A human-readable reason.
        message: String,
    },
}

/// The local node's deterministic in-process controller — always present, and
/// the default target of a `run` with no controller named.
pub fn link_controller() -> Controller {
    Controller {
        name: "link".to_string(),
        kind: "link".to_string(),
        api_class: "hci".to_string(),
        network: "link".to_string(),
        real: false,
        deterministic: true,
        attachable: true,
        product: None,
    }
}

/// One USB dongle as a controller on the `real` network. A dongle is a real HCI
/// controller, so it is `hci`, `real`, and `attachable`.
#[cfg(not(target_arch = "wasm32"))]
pub fn usb_controller(dongle: &crate::transport::usb::UsbDongle) -> Controller {
    Controller {
        name: format!("dongle-{}", dongle.index),
        kind: "usb".to_string(),
        api_class: "hci".to_string(),
        network: "real".to_string(),
        real: true,
        deterministic: false,
        attachable: true,
        product: dongle.product.clone(),
    }
}

/// The controllers this node offers: the deterministic `link`, plus each USB
/// dongle currently plugged in. (netsim is a network/forward, not a static
/// controller; a `rootcanal` controller is minted per device — neither is
/// enumerated here.)
pub fn list_controllers() -> Vec<Controller> {
    let mut controllers = vec![link_controller()];
    #[cfg(not(target_arch = "wasm32"))]
    if let Ok(dongles) = crate::transport::usb::list_bluetooth_dongles() {
        controllers.extend(dongles.iter().map(usb_controller));
    }
    controllers
}

/// The worlds this node offers: the deterministic `link` always, and the shared
/// `real` air when any dongle is present (real membership is only meaningful
/// with a real controller to enter it through).
pub fn list_networks() -> Vec<Network> {
    let mut networks = vec![Network {
        name: "link".to_string(),
        kind: "link".to_string(),
        deterministic: true,
        real: false,
        shared: false,
        leaf: false,
    }];
    #[cfg(not(target_arch = "wasm32"))]
    if crate::transport::usb::list_bluetooth_dongles()
        .map(|d| !d.is_empty())
        .unwrap_or(false)
    {
        networks.push(Network {
            name: "real".to_string(),
            kind: "rf".to_string(),
            deterministic: false,
            real: true,
            shared: true,
            leaf: false,
        });
    }
    networks
}

/// The participants. Only the local `simble` node so far — it owns the
/// controllers `list_controllers` reports. Registered phones/browsers will add
/// entries here.
pub fn list_nodes() -> Vec<NodeInfo> {
    vec![NodeInfo {
        name: "local".to_string(),
        kind: "router".to_string(),
        controllers: list_controllers().into_iter().map(|c| c.name).collect(),
    }]
}

/// Makes and advertises the controllers a [`Node`] can run on — the injection
/// seam for backends. The built-in factory ([`BuiltinControllers`]) knows `link`,
/// `usb` dongles, and `netsim`; an external crate (a `rootcanal-rs` backend, say)
/// implements this for its own controllers, and an application composes them with
/// [`Node::with_factory`] — so `simble-stack` never depends on the backend, only
/// on this trait. This is where `rootcanal-rs` plugs in as a controller factory.
#[cfg(not(target_arch = "wasm32"))]
pub trait ControllerFactory {
    /// Builds the controller named `name`, or an error if this factory does not
    /// offer it.
    fn create(&self, name: &str) -> Result<Box<dyn crate::transport::Scene>, String>;
    /// The controllers this factory offers, for `list_controllers`. Default: none
    /// advertised — create-by-name still works, the list just won't show them.
    fn available(&self) -> Vec<Controller> {
        Vec::new()
    }
}

/// The built-in controllers a plain [`Node`] uses: `link` (deterministic
/// in-process), `usb` dongles, and `netsim`.
#[cfg(not(target_arch = "wasm32"))]
pub struct BuiltinControllers;

#[cfg(not(target_arch = "wasm32"))]
impl ControllerFactory for BuiltinControllers {
    fn create(&self, name: &str) -> Result<Box<dyn crate::transport::Scene>, String> {
        scene_for_controller(name)
    }
    fn available(&self) -> Vec<Controller> {
        list_controllers()
    }
}

/// A [`ControllerFactory`] that tries several in order — how an app adds a
/// backend factory *alongside* the built-in one (e.g.
/// `CompositeFactory::new(vec![rootcanal_factory, Box::new(BuiltinControllers)])`).
/// `create` returns the first factory that offers the name; `available`
/// concatenates them, first wins on a name clash.
#[cfg(not(target_arch = "wasm32"))]
pub struct CompositeFactory {
    factories: Vec<Box<dyn ControllerFactory>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl CompositeFactory {
    /// Composes `factories`, tried in order.
    pub fn new(factories: Vec<Box<dyn ControllerFactory>>) -> Self {
        Self { factories }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl ControllerFactory for CompositeFactory {
    fn create(&self, name: &str) -> Result<Box<dyn crate::transport::Scene>, String> {
        let mut last = format!("no factory offers controller {name:?}");
        for factory in &self.factories {
            match factory.create(name) {
                Ok(scene) => return Ok(scene),
                Err(e) => last = e,
            }
        }
        Err(last)
    }
    fn available(&self) -> Vec<Controller> {
        let mut out: Vec<Controller> = Vec::new();
        for factory in &self.factories {
            for c in factory.available() {
                if !out.iter().any(|x| x.name == c.name) {
                    out.push(c);
                }
            }
        }
        out
    }
}

/// A v1 node: a router that owns named controllers and the devices running on
/// them. This is the "many controllers" node the design calls for — a device has
/// one **stable global handle** (its `index` on the node) that survives `stop`
/// and `route`, while the node maps it internally to a controller and that
/// controller's local index. Every controller shares the node's clock, so one
/// `tick` advances the whole node and one deadline covers it.
///
/// This is the *device* router (scripted devices over the synchronous
/// `transport::Scene` trait, `link` + `usb`, no async). The lower *backend*
/// router — routing raw HCI to `rootcanal-rs`/netsim with the `0x10` switch — is
/// the separate async crate described in `docs/controller-routing.md`.
#[cfg(not(target_arch = "wasm32"))]
pub struct Node {
    controllers: Vec<NodeController>,
    devices: Vec<DeviceRecord>,
    /// The node's simulated clock (µs); every controller is advanced in lockstep
    /// with it, and a late-added controller is caught up to it.
    clock_us: u64,
    next_addr: u16,
    /// External nodes admitted with `register` — bookkeeping the `list_nodes` op
    /// reflects; this node does not drive them (cross-node orchestration is the
    /// client's job).
    registered: Vec<NodeInfo>,
    /// Makes controllers by name (the injection seam). Defaults to the built-in
    /// `link`/`usb`/`netsim` set; an app injects a backend factory to add more.
    factory: Box<dyn ControllerFactory>,
}

/// One named controller a node owns.
#[cfg(not(target_arch = "wasm32"))]
struct NodeController {
    name: String,
    scene: Box<dyn crate::transport::Scene>,
}

/// What the node remembers about a running device so `route` can drop it and
/// re-run it on another controller: its source and address, and where it is now.
#[cfg(not(target_arch = "wasm32"))]
struct DeviceRecord {
    controller: usize,
    local: usize,
    script: String,
    address: crate::types::Address,
    stopped: bool,
}

#[cfg(not(target_arch = "wasm32"))]
impl Default for Node {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Node {
    /// An empty node using the built-in controllers (`link`/`usb`/`netsim`).
    pub fn new() -> Self {
        Self::with_factory(Box::new(BuiltinControllers))
    }

    /// An empty node whose controllers come from `factory` — the injection point
    /// for a backend (e.g. an app composing a `rootcanal-rs` factory with the
    /// built-in one). This is "create simble with a controller factory".
    pub fn with_factory(factory: Box<dyn ControllerFactory>) -> Self {
        Self {
            controllers: Vec::new(),
            devices: Vec::new(),
            clock_us: 0,
            next_addr: 1,
            registered: Vec::new(),
            factory,
        }
    }

    /// The controllers this node offers: what its factory advertises, plus any it
    /// has `create`d that the factory did not already list.
    pub fn available_controllers(&self) -> Vec<Controller> {
        let mut controllers = self.factory.available();
        for name in self.controller_names() {
            if !controllers.iter().any(|c| c.name == name) {
                controllers.push(created_controller(&name));
            }
        }
        controllers
    }

    /// The names of the controllers this node owns (built-in or `create`d).
    pub fn controller_names(&self) -> Vec<String> {
        self.controllers.iter().map(|c| c.name.clone()).collect()
    }

    /// The external nodes registered with this one.
    pub fn registered_nodes(&self) -> &[NodeInfo] {
        &self.registered
    }

    /// Finds the controller named `name`, or builds and adds it via
    /// [`scene_for_controller`] — caught up to the node clock so every controller
    /// shares one time frame. Returns its slot index.
    fn controller_index(&mut self, name: &str) -> Result<usize, String> {
        if let Some(i) = self.controllers.iter().position(|c| c.name == name) {
            return Ok(i);
        }
        let mut scene = self.factory.create(name)?;
        if self.clock_us > 0 {
            scene.tick(self.clock_us);
        }
        self.controllers.push(NodeController {
            name: name.to_string(),
            scene,
        });
        Ok(self.controllers.len() - 1)
    }

    /// Creates a new private `link` ether named `name` (a fresh in-process world,
    /// caught up to the node clock) that `run`/`route` can then target. Errors if
    /// a controller by that name already exists.
    pub fn create_network(&mut self, name: &str) -> Result<(), String> {
        if self.controllers.iter().any(|c| c.name == name) {
            return Err(format!("controller {name:?} already exists"));
        }
        let mut scene: Box<dyn crate::transport::Scene> =
            Box::new(crate::transport::link_scene::LinkScene::new());
        if self.clock_us > 0 {
            scene.tick(self.clock_us);
        }
        self.controllers.push(NodeController {
            name: name.to_string(),
            scene,
        });
        Ok(())
    }

    /// Admits an external node (a phone, a browser) into this node's view — the
    /// `list_nodes` op reflects it. Bookkeeping only: this node does not drive it.
    pub fn register_node(&mut self, node: NodeInfo) {
        match self.registered.iter_mut().find(|n| n.name == node.name) {
            Some(existing) => *existing = node,
            None => self.registered.push(node),
        }
    }

    /// Runs `script` as a device on `controller` (built if new); returns the
    /// device's stable global handle. A deterministic address is allocated when
    /// `address` is `None`.
    pub fn run_on(
        &mut self,
        controller: &str,
        script: &str,
        address: Option<crate::types::Address>,
    ) -> Result<usize, String> {
        let address = address.unwrap_or_else(|| self.next_address());
        let ci = self.controller_index(controller)?;
        let local = self.controllers[ci].scene.add_peripheral(address, script)?;
        self.devices.push(DeviceRecord {
            controller: ci,
            local,
            script: script.to_string(),
            address,
            stopped: false,
        });
        Ok(self.devices.len() - 1)
    }

    fn next_address(&mut self) -> crate::types::Address {
        let n = self.next_addr;
        self.next_addr = self.next_addr.wrapping_add(1);
        // Stable and obviously simulated, in the CC:1E:57 space simble uses.
        crate::types::Address::new([(n & 0xff) as u8, (n >> 8) as u8, 0x00, 0x57, 0x1e, 0xcc])
    }

    fn live_record(&self, id: usize) -> Result<&DeviceRecord, String> {
        match self.devices.get(id) {
            Some(r) if !r.stopped => Ok(r),
            Some(_) => Err(format!("device {id} is stopped")),
            None => Err(format!("no device {id}")),
        }
    }

    /// Moves packets both ways for every device on every controller.
    pub fn pump(&mut self) {
        for c in &mut self.controllers {
            c.scene.pump();
        }
    }

    /// Advances the whole node — every controller — by `advance_us` microseconds
    /// and returns the earliest absolute deadline (µs) across them, `None` if none
    /// is scheduled, so the caller waits *until* it rather than spinning.
    pub fn tick(&mut self, advance_us: u64) -> Option<u64> {
        self.clock_us = self.clock_us.saturating_add(advance_us);
        let mut deadline: Option<u64> = None;
        for c in &mut self.controllers {
            if let Some(d) = c.scene.tick(advance_us) {
                deadline = Some(deadline.map_or(d, |cur| cur.min(d)));
            }
        }
        deadline
    }

    /// This node's current simulated clock, in microseconds.
    pub fn now_us(&self) -> u64 {
        self.clock_us
    }

    /// The earliest absolute deadline (µs) any controller has, or `None`. A real
    /// value appears once a device declares a wake time with `wake_at`.
    pub fn next_deadline_us(&self) -> Option<u64> {
        self.controllers
            .iter()
            .filter_map(|c| c.scene.next_deadline_us())
            .reduce(u64::min)
    }

    /// The live devices on this node, by stable global handle — stopped
    /// tombstones are skipped, so a handle stays valid across teardowns.
    pub fn list_devices(&self) -> Vec<Device> {
        self.devices
            .iter()
            .enumerate()
            .filter(|(_, r)| !r.stopped)
            .map(|(id, r)| {
                let c = &self.controllers[r.controller];
                Device {
                    index: id,
                    controller: c.name.clone(),
                    status: c
                        .scene
                        .peripheral_status_json(r.local)
                        .and_then(|s| serde_json::from_str(&s).ok()),
                }
            })
            .collect()
    }

    /// Stops (tears down) the device `id`, releasing its controller slot. The
    /// handle stays valid but inert.
    pub fn stop(&mut self, id: usize) -> Result<(), String> {
        let (ci, local) = {
            let r = self.live_record(id)?;
            (r.controller, r.local)
        };
        self.devices[id].stopped = true;
        self.controllers[ci].scene.stop(local)
    }

    /// Delivers an input `event` (with an optional JSON `data` payload) to device
    /// `id` — its script sees it in `fn on_event` on the next tick.
    pub fn send(
        &mut self,
        id: usize,
        event: &str,
        data: Option<&serde_json::Value>,
    ) -> Result<(), String> {
        let data_json = data
            .map(|v| v.to_string())
            .unwrap_or_else(|| "{}".to_string());
        let (ci, local) = {
            let r = self.live_record(id)?;
            (r.controller, r.local)
        };
        self.controllers[ci].scene.send(local, event, &data_json)
    }

    /// Routes device `id` onto `to_controller` (built if new), keeping the same
    /// global handle and address. This **drops and re-runs** rather than migrating
    /// live state — the deterministic analog of a real controller switch injecting
    /// an HCI Hardware Error (`0x10`) so the host re-initialises. Returns the
    /// controller the device now runs on. A no-op (same controller) is fine.
    pub fn route(&mut self, id: usize, to_controller: &str) -> Result<String, String> {
        let (old_ci, old_local, script, address) = {
            let r = self.live_record(id)?;
            (r.controller, r.local, r.script.clone(), r.address)
        };
        if self.controllers[old_ci].name == to_controller {
            return Ok(to_controller.to_string());
        }
        // Drop on the old controller, then re-run on the new one.
        self.controllers[old_ci].scene.stop(old_local)?;
        let ci = self.controller_index(to_controller)?;
        let local = self.controllers[ci]
            .scene
            .add_peripheral(address, &script)?;
        let r = &mut self.devices[id];
        r.controller = ci;
        r.local = local;
        Ok(to_controller.to_string())
    }
}

/// Builds the scene for a named controller: `netsim` is the shared ether, a
/// dongle is `dongle-<index>`, and `link` is the deterministic in-process path
/// (MCP's `self` mode) — a hardware-free scene for `run`/`tick`/`get_clock`.
#[cfg(not(target_arch = "wasm32"))]
pub fn scene_for_controller(name: &str) -> Result<Box<dyn crate::transport::Scene>, String> {
    use crate::transport::netsim::{self, NetsimScene};
    use crate::transport::usb::{UsbScene, UsbSelector};
    if name == "netsim" {
        return Ok(Box::new(NetsimScene::new(netsim::DEFAULT_WS_URL)));
    }
    if let Some(rest) = name.strip_prefix("dongle-") {
        let index: usize = rest
            .parse()
            .map_err(|_| format!("bad dongle name {name:?} — expected dongle-<index>"))?;
        return Ok(Box::new(UsbScene::new(UsbSelector::Index(index))));
    }
    if name == "link" {
        return Ok(Box::new(crate::transport::link_scene::LinkScene::new()));
    }
    Err(format!("unknown controller {name:?}"))
}

/// Handles one v1 request against `node` — the live execution state, `None`
/// until the first `run` selects a controller.
#[cfg(not(target_arch = "wasm32"))]
pub fn dispatch(request: Request, node: &mut Option<Node>) -> Response {
    // The verbs that need a device on the node share this "no node yet" error.
    let no_node = || Response::Error {
        message: "no device running — run something first".to_string(),
    };
    match request {
        Request::ListControllers => Response::Controllers {
            controllers: merged_controllers(node),
        },
        Request::ListNetworks => Response::Networks {
            networks: merged_networks(node),
        },
        Request::ListDevices => Response::Devices {
            devices: node.as_ref().map(Node::list_devices).unwrap_or_default(),
        },
        Request::ListNodes => Response::Nodes {
            nodes: merged_nodes(node),
        },
        Request::Run {
            controller,
            script,
            address,
        } => {
            let address = match address {
                Some(s) => match s.parse::<crate::types::Address>() {
                    Ok(a) => Some(a),
                    Err(_) => {
                        return Response::Error {
                            message: format!("bad address {s:?}"),
                        };
                    }
                },
                None => None,
            };
            let node = node.get_or_insert_with(Node::new);
            match node.run_on(&controller, &script, address) {
                Ok(device) => Response::Ran { device, controller },
                Err(message) => Response::Error { message },
            }
        }
        Request::Route { device, controller } => match node.as_mut() {
            Some(node) => match node.route(device, &controller) {
                Ok(controller) => Response::Routed { device, controller },
                Err(message) => Response::Error { message },
            },
            None => no_node(),
        },
        Request::Create { network } => {
            let node = node.get_or_insert_with(Node::new);
            match node.create_network(&network) {
                Ok(()) => Response::Created { network },
                Err(message) => Response::Error { message },
            }
        }
        Request::Register { node: info } => {
            let name = info.name.clone();
            node.get_or_insert_with(Node::new).register_node(info);
            Response::Registered { node: name }
        }
        Request::Tick { advance_us } => match node.as_mut() {
            Some(node) => {
                let deadline_us = node.tick(advance_us);
                node.pump();
                Response::Ticked {
                    now_us: node.now_us(),
                    deadline_us,
                }
            }
            None => no_node(),
        },
        Request::GetClock => Response::Clock {
            now_us: node.as_ref().map(Node::now_us).unwrap_or(0),
            deadline_us: node.as_ref().and_then(Node::next_deadline_us),
        },
        Request::Stop { device } => match node.as_mut() {
            Some(node) => match node.stop(device) {
                Ok(()) => Response::Stopped { device },
                Err(message) => Response::Error { message },
            },
            None => no_node(),
        },
        Request::Send {
            device,
            event,
            data,
        } => match node.as_mut() {
            Some(node) => match node.send(device, &event, data.as_ref()) {
                Ok(()) => Response::Sent { device },
                Err(message) => Response::Error { message },
            },
            None => no_node(),
        },
    }
}

/// Whether `name` is a built-in controller (as opposed to a `create`d one).
#[cfg(not(target_arch = "wasm32"))]
fn is_builtin_controller(name: &str) -> bool {
    name == "link" || name == "netsim" || name.starts_with("dongle-")
}

/// A `create`d private-ether controller's list entry (a `link`-kind ether).
#[cfg(not(target_arch = "wasm32"))]
fn created_controller(name: &str) -> Controller {
    Controller {
        name: name.to_string(),
        kind: "link".to_string(),
        api_class: "hci".to_string(),
        network: name.to_string(),
        real: false,
        deterministic: true,
        attachable: true,
        product: None,
    }
}

/// The controllers on offer — from the node's factory (plus what it `create`d)
/// once a node exists, or the built-in set before one does.
#[cfg(not(target_arch = "wasm32"))]
fn merged_controllers(node: &Option<Node>) -> Vec<Controller> {
    match node {
        Some(node) => node.available_controllers(),
        None => list_controllers(),
    }
}

/// The built-in networks plus a private ether for each `create`d controller.
#[cfg(not(target_arch = "wasm32"))]
fn merged_networks(node: &Option<Node>) -> Vec<Network> {
    let mut networks = list_networks();
    if let Some(node) = node {
        for name in node.controller_names() {
            if !is_builtin_controller(&name) && !networks.iter().any(|n| n.name == name) {
                networks.push(Network {
                    name: name.clone(),
                    kind: "link".to_string(),
                    deterministic: true,
                    real: false,
                    shared: false,
                    leaf: false,
                });
            }
        }
    }
    networks
}

/// The local node (its controllers, built-in and `create`d) plus any external
/// nodes `register`ed with it.
#[cfg(not(target_arch = "wasm32"))]
fn merged_nodes(node: &Option<Node>) -> Vec<NodeInfo> {
    let mut controllers: Vec<String> = match node {
        Some(node) => node
            .available_controllers()
            .into_iter()
            .map(|c| c.name)
            .collect(),
        None => list_controllers().into_iter().map(|c| c.name).collect(),
    };
    let mut nodes = Vec::new();
    if let Some(node) = node {
        for name in node.controller_names() {
            if !controllers.contains(&name) {
                controllers.push(name);
            }
        }
        nodes.extend(node.registered_nodes().iter().cloned());
    }
    nodes.insert(
        0,
        NodeInfo {
            name: "local".to_string(),
            kind: "router".to_string(),
            controllers,
        },
    );
    nodes
}

// ---------------------------------------------------------------------------
// The request-handling API.
//
// v1 does not ship a server: a host that runs it — netsim, a CLI, a browser —
// already has an http/ws server, and bundling a second one would be wrong. So
// these are the entry points a host's server calls, at three levels: typed
// (`dispatch`), a JSON string in/out (`handle_json`, for a ws or any JSON
// transport), and an HTTP method+path+body → status+body (`handle_http`, for a
// REST server). Any *default* standalone server we add later is a feature on
// top of these; the API itself has no server dependency.
// ---------------------------------------------------------------------------

/// A minimal HTTP result: a status code and a JSON body. The caller writes it
/// onto its own socket (and sets `Content-Type: application/json`).
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    /// HTTP status code (200 on success, 400 on an error, 404 for an unknown
    /// route).
    pub status: u16,
    /// The JSON body — a serialized [`Response`].
    pub body: String,
}

/// The body of `POST /v1/run` (the op is the path, so it is not in the body).
#[cfg(not(target_arch = "wasm32"))]
#[derive(Deserialize)]
struct RunBody {
    controller: String,
    script: String,
    #[serde(default)]
    address: Option<String>,
}

/// The body of `POST /v1/tick`.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Deserialize)]
struct TickBody {
    advance_us: u64,
}

/// The body of `POST /v1/stop`.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Deserialize)]
struct StopBody {
    device: usize,
}

/// The body of `POST /v1/send`.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Deserialize)]
struct SendBody {
    device: usize,
    event: String,
    #[serde(default)]
    data: Option<serde_json::Value>,
}

/// The body of `POST /v1/route`.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Deserialize)]
struct RouteBody {
    device: usize,
    controller: String,
}

/// The body of `POST /v1/create`.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Deserialize)]
struct CreateBody {
    network: String,
}

#[cfg(not(target_arch = "wasm32"))]
fn json_or_empty<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

/// Handles a JSON request string and returns a JSON response string — the entry
/// point a ws:// (or any JSON) transport calls. Malformed JSON is an `Error`
/// response, not a panic.
#[cfg(not(target_arch = "wasm32"))]
pub fn handle_json(node: &mut Option<Node>, request_json: &str) -> String {
    match serde_json::from_str::<Request>(request_json) {
        Ok(request) => json_or_empty(&dispatch(request, node)),
        Err(e) => json_or_empty(&Response::Error {
            message: format!("bad request: {e}"),
        }),
    }
}

/// Routes an HTTP request (method + path + body) to a v1 op and returns the
/// status and JSON body — the entry point a REST server calls. The op is the
/// route; the body carries its parameters.
#[cfg(not(target_arch = "wasm32"))]
pub fn handle_http(node: &mut Option<Node>, method: &str, path: &str, body: &str) -> HttpResponse {
    let request = match (method, path) {
        ("GET", "/v1/controllers") => Request::ListControllers,
        ("GET", "/v1/networks") => Request::ListNetworks,
        ("GET", "/v1/devices") => Request::ListDevices,
        ("GET", "/v1/nodes") => Request::ListNodes,
        ("GET", "/v1/clock") => Request::GetClock,
        ("POST", "/v1/run") => match serde_json::from_str::<RunBody>(body) {
            Ok(b) => Request::Run {
                controller: b.controller,
                script: b.script,
                address: b.address,
            },
            Err(e) => {
                return HttpResponse {
                    status: 400,
                    body: json_or_empty(&Response::Error {
                        message: format!("bad run body: {e}"),
                    }),
                };
            }
        },
        ("POST", "/v1/tick") => match serde_json::from_str::<TickBody>(body) {
            Ok(b) => Request::Tick {
                advance_us: b.advance_us,
            },
            Err(e) => {
                return HttpResponse {
                    status: 400,
                    body: json_or_empty(&Response::Error {
                        message: format!("bad tick body: {e}"),
                    }),
                };
            }
        },
        ("POST", "/v1/stop") => match serde_json::from_str::<StopBody>(body) {
            Ok(b) => Request::Stop { device: b.device },
            Err(e) => {
                return HttpResponse {
                    status: 400,
                    body: json_or_empty(&Response::Error {
                        message: format!("bad stop body: {e}"),
                    }),
                };
            }
        },
        ("POST", "/v1/send") => match serde_json::from_str::<SendBody>(body) {
            Ok(b) => Request::Send {
                device: b.device,
                event: b.event,
                data: b.data,
            },
            Err(e) => {
                return HttpResponse {
                    status: 400,
                    body: json_or_empty(&Response::Error {
                        message: format!("bad send body: {e}"),
                    }),
                };
            }
        },
        ("POST", "/v1/route") => match serde_json::from_str::<RouteBody>(body) {
            Ok(b) => Request::Route {
                device: b.device,
                controller: b.controller,
            },
            Err(e) => {
                return HttpResponse {
                    status: 400,
                    body: json_or_empty(&Response::Error {
                        message: format!("bad route body: {e}"),
                    }),
                };
            }
        },
        ("POST", "/v1/create") => match serde_json::from_str::<CreateBody>(body) {
            Ok(b) => Request::Create { network: b.network },
            Err(e) => {
                return HttpResponse {
                    status: 400,
                    body: json_or_empty(&Response::Error {
                        message: format!("bad create body: {e}"),
                    }),
                };
            }
        },
        ("POST", "/v1/register") => match serde_json::from_str::<NodeInfo>(body) {
            Ok(info) => Request::Register { node: info },
            Err(e) => {
                return HttpResponse {
                    status: 400,
                    body: json_or_empty(&Response::Error {
                        message: format!("bad register body: {e}"),
                    }),
                };
            }
        },
        _ => {
            return HttpResponse {
                status: 404,
                body: json_or_empty(&Response::Error {
                    message: format!("no v1 route for {method} {path}"),
                }),
            };
        }
    };
    let response = dispatch(request, node);
    let status = if matches!(response, Response::Error { .. }) {
        400
    } else {
        200
    };
    HttpResponse {
        status,
        body: json_or_empty(&response),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_always_has_the_link_controller() {
        // The deterministic in-process controller is always available, with no
        // hardware — it is the default `run` target.
        let controllers = list_controllers();
        let link = controllers
            .iter()
            .find(|c| c.name == "link")
            .expect("link is always present");
        assert_eq!(link.kind, "link");
        assert_eq!(link.api_class, "hci");
        assert_eq!(link.network, "link");
        assert!(link.deterministic);
        assert!(!link.real);
        assert!(link.attachable);
    }

    #[test]
    fn dispatch_list_controllers_answers_with_the_controllers() {
        let Response::Controllers { controllers } = dispatch(Request::ListControllers, &mut None)
        else {
            panic!("list_controllers must answer with Controllers");
        };
        assert!(controllers.iter().any(|c| c.name == "link"));
    }

    #[test]
    fn request_and_response_round_trip_as_tagged_json() {
        // The request is tagged by `op`.
        let req: Request = serde_json::from_str(r#"{"op":"list_controllers"}"#).unwrap();
        assert_eq!(req, Request::ListControllers);
        assert_eq!(
            serde_json::to_string(&Request::ListControllers).unwrap(),
            r#"{"op":"list_controllers"}"#
        );

        // The response is tagged by `type`; the empty `product` is omitted.
        let resp = Response::Controllers {
            controllers: vec![link_controller()],
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.starts_with(r#"{"type":"controllers""#), "{json}");
        assert!(!json.contains("product"), "empty product should be omitted");
        let back: Response = serde_json::from_str(&json).unwrap();
        assert_eq!(back, resp);
    }

    /// A minimal device: a GATT server with no behaviour, enough to run and list.
    const BARE_DEVICE: &str = r#"let server = android::BluetoothGattServer("x");"#;

    #[test]
    fn node_run_adds_a_device_and_lists_it() {
        let mut node = Node::new();
        let index = node.run_on("link", BARE_DEVICE, None).unwrap();
        assert_eq!(index, 0);

        let devices = node.list_devices();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].index, 0);
        assert_eq!(devices[0].controller, "link");
        assert!(devices[0].status.is_some());
    }

    #[test]
    fn dispatch_run_rejects_unknown_controller_and_bad_script() {
        // An unknown controller is refused before any scene is built — no hardware.
        let mut node = None;
        let Response::Error { message } = dispatch(
            Request::Run {
                controller: "bogus".to_string(),
                script: "x".to_string(),
                address: None,
            },
            &mut node,
        ) else {
            panic!("an unknown controller must error");
        };
        assert!(message.contains("unknown controller"), "{message}");

        // `link` is wired (the deterministic self path), so it builds a scene —
        // but a broken script is still rejected rather than run.
        let Response::Error { .. } = dispatch(
            Request::Run {
                controller: "link".to_string(),
                script: "let x = ;".to_string(),
                address: None,
            },
            &mut node,
        ) else {
            panic!("a broken script must error");
        };
    }

    #[test]
    fn handle_http_routes_and_status_codes() {
        let mut node = None;
        // GET /v1/controllers → 200 with the controller list.
        let r = handle_http(&mut node, "GET", "/v1/controllers", "");
        assert_eq!(r.status, 200);
        assert!(r.body.contains("\"link\""), "{}", r.body);
        // POST /v1/run with a malformed body → 400.
        let r = handle_http(&mut node, "POST", "/v1/run", "not json");
        assert_eq!(r.status, 400);
        // POST /v1/run onto an unknown controller → 400 (the dispatch error).
        let r = handle_http(
            &mut node,
            "POST",
            "/v1/run",
            r#"{"controller":"bogus","script":"x"}"#,
        );
        assert_eq!(r.status, 400);
        assert!(r.body.contains("unknown controller"), "{}", r.body);
        // An unknown route → 404.
        let r = handle_http(&mut node, "DELETE", "/v1/nope", "");
        assert_eq!(r.status, 404);
    }

    #[test]
    fn handle_json_dispatches_and_survives_garbage() {
        let mut node = None;
        let out = handle_json(&mut node, r#"{"op":"list_controllers"}"#);
        assert!(out.contains("\"type\":\"controllers\""), "{out}");
        // Malformed JSON is an Error response, not a panic.
        let out = handle_json(&mut node, "}{");
        assert!(out.contains("\"type\":\"error\""), "{out}");
    }

    #[test]
    fn the_other_lists_are_present_and_routed() {
        // link is always a network; the local node owns it.
        let networks = list_networks();
        assert!(networks.iter().any(|n| n.name == "link" && n.deterministic));
        let nodes = list_nodes();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].name, "local");
        assert!(nodes[0].controllers.iter().any(|c| c == "link"));

        // All three GET routes answer 200 with their tagged body.
        let mut node = None;
        for (path, tag) in [
            ("/v1/networks", "networks"),
            ("/v1/devices", "devices"),
            ("/v1/nodes", "nodes"),
        ] {
            let r = handle_http(&mut node, "GET", path, "");
            assert_eq!(r.status, 200, "{path}");
            assert!(
                r.body.contains(&format!("\"type\":\"{tag}\"")),
                "{}",
                r.body
            );
        }
    }

    #[test]
    fn tick_needs_a_node_and_reports_the_clock() {
        // No node yet: tick errors rather than doing nothing silently.
        let mut node = None;
        let Response::Error { .. } = dispatch(Request::Tick { advance_us: 1000 }, &mut node) else {
            panic!("tick with no node must error");
        };
        // With a node, tick advances the node clock and answers Ticked.
        let mut node = Some(Node::new());
        let Response::Ticked {
            now_us,
            deadline_us,
        } = dispatch(Request::Tick { advance_us: 1500 }, &mut node)
        else {
            panic!("tick must answer Ticked");
        };
        assert_eq!(now_us, 1500);
        // tick returns the sans-io deadline — None until a device declares a wake.
        assert_eq!(deadline_us, None);
        // A malformed tick body is a 400, not a panic.
        let r = handle_http(&mut None, "POST", "/v1/tick", "not json");
        assert_eq!(r.status, 400);
    }

    #[test]
    fn get_clock_reports_the_next_deadline_or_none() {
        // No node yet: the clock reads 0 with no deadline — the host waits for a
        // packet or polls. The op and route exist so a host can write the
        // wait-until-deadline loop today; a real deadline lands once a device
        // declares a wake time with `wake_at`.
        let mut node = None;
        let Response::Clock {
            now_us,
            deadline_us,
        } = dispatch(Request::GetClock, &mut node)
        else {
            panic!("get_clock must answer Clock");
        };
        assert_eq!(now_us, 0);
        assert_eq!(deadline_us, None);

        let mut node = Some(Node::new());
        let Response::Clock {
            now_us,
            deadline_us,
        } = dispatch(Request::GetClock, &mut node)
        else {
            panic!("get_clock must answer Clock");
        };
        assert_eq!(now_us, 0);
        assert_eq!(deadline_us, None);

        let r = handle_http(&mut None, "GET", "/v1/clock", "");
        assert_eq!(r.status, 200);
        assert!(r.body.contains("\"type\":\"clock\""), "{}", r.body);
    }

    // The `link` controller runs the deterministic in-process scene, so the whole
    // v1 loop — run → tick → clock, with a real device-declared deadline — works
    // with no netsim or USB hardware.
    #[test]
    fn run_on_link_ticks_and_reports_a_real_deadline() {
        let script = r#"
            let server = android::BluetoothGattServer("waker");
            fn tick(server, t) { server.wake_at(t + 0.05); }
        "#;
        let mut node = None;
        let Response::Ran { controller, .. } = dispatch(
            Request::Run {
                controller: "link".to_string(),
                script: script.to_string(),
                address: None,
            },
            &mut node,
        ) else {
            panic!("run on link must succeed");
        };
        assert_eq!(controller, "link");

        // Advance 1 s (1_000_000 µs). The device asked to wake 50 ms later, so the
        // absolute deadline is the µs clock at 1.05 s.
        let Response::Ticked {
            now_us,
            deadline_us,
        } = dispatch(
            Request::Tick {
                advance_us: 1_000_000,
            },
            &mut node,
        )
        else {
            panic!("tick must answer Ticked");
        };
        assert_eq!(now_us, 1_000_000);
        assert_eq!(deadline_us, Some(1_050_000));

        // get_clock peeks the same clock and deadline without advancing.
        let Response::Clock {
            now_us,
            deadline_us,
        } = dispatch(Request::GetClock, &mut node)
        else {
            panic!("get_clock must answer Clock");
        };
        assert_eq!(now_us, 1_000_000);
        assert_eq!(deadline_us, Some(1_050_000));
    }

    // `stop` tears a device down: it leaves the device list, stops contributing a
    // deadline, and its index stays a stable handle — a later run takes a new one.
    #[test]
    fn stop_tears_down_a_device_and_keeps_indices_stable() {
        let waker = r#"
            let server = android::BluetoothGattServer("waker");
            fn tick(server, t) { server.wake_at(t + 0.05); }
        "#;
        let run = |node: &mut Option<Node>, script: &str| {
            dispatch(
                Request::Run {
                    controller: "link".to_string(),
                    script: script.to_string(),
                    address: None,
                },
                node,
            )
        };

        let mut node = None;
        assert!(matches!(
            run(&mut node, waker),
            Response::Ran { device: 0, .. }
        ));
        dispatch(
            Request::Tick {
                advance_us: 1_000_000,
            },
            &mut node,
        );
        // The waker is listed and drives the deadline.
        let Response::Devices { devices } = dispatch(Request::ListDevices, &mut node) else {
            panic!("list");
        };
        assert_eq!(devices.len(), 1);
        assert!(matches!(
            dispatch(Request::GetClock, &mut node),
            Response::Clock {
                deadline_us: Some(_),
                ..
            }
        ));

        // Stop it: gone from the list, and no device declares a deadline anymore.
        assert_eq!(
            dispatch(Request::Stop { device: 0 }, &mut node),
            Response::Stopped { device: 0 }
        );
        let Response::Devices { devices } = dispatch(Request::ListDevices, &mut node) else {
            panic!("list");
        };
        assert!(
            devices.is_empty(),
            "stopped device must not list: {devices:?}"
        );
        dispatch(
            Request::Tick {
                advance_us: 1_000_000,
            },
            &mut node,
        );
        assert!(matches!(
            dispatch(Request::GetClock, &mut node),
            Response::Clock {
                deadline_us: None,
                ..
            }
        ));

        // A new run takes index 1 — the stopped slot 0 is a tombstone, never reused.
        assert!(matches!(
            run(&mut node, waker),
            Response::Ran { device: 1, .. }
        ));

        // Stopping an out-of-range index is an error, not a panic.
        assert!(matches!(
            dispatch(Request::Stop { device: 99 }, &mut node),
            Response::Error { .. }
        ));
    }

    // `send` delivers an input event the script handles in `fn on_event`, driving
    // a real change a later `list_devices` observes.
    #[test]
    fn send_delivers_an_input_event_the_script_acts_on() {
        // A button device: on the "press" event it writes 0x0063 to its
        // characteristic; the value shows up in the device's status.
        let script = r#"
            let server = android::BluetoothGattServer("button");
            let svc = android::BluetoothGattService(uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY);
            let chr = android::BluetoothGattCharacteristic(
                uuid::HEART_RATE_MEASUREMENT,
                android::PROPERTY_READ | android::PROPERTY_NOTIFY,
                android::PERMISSION_READ,
            );
            chr.set_value([0x00, 0x00]);
            svc.add_characteristic(chr);
            server.add_service(svc);
            fn on_event(server, event) {
                if event.event == "press" {
                    server.update_value(uuid::HEART_RATE_MEASUREMENT, [0x00, 0x63]);
                }
            }
        "#;
        let mut node = None;
        assert!(matches!(
            dispatch(
                Request::Run {
                    controller: "link".to_string(),
                    script: script.to_string(),
                    address: None,
                },
                &mut node,
            ),
            Response::Ran { device: 0, .. }
        ));

        let value_of = |node: &mut Option<Node>| -> String {
            let Response::Devices { devices } = dispatch(Request::ListDevices, node) else {
                panic!("list");
            };
            devices[0].status.as_ref().unwrap()["services"][0]["characteristics"][0]["value"]
                .as_str()
                .unwrap()
                .to_string()
        };

        // Before the event the characteristic is still zero.
        dispatch(Request::Tick { advance_us: 1_000 }, &mut node);
        assert_eq!(value_of(&mut node), "0000");

        // Send the input; on the next tick the script handles it and the value changes.
        assert_eq!(
            dispatch(
                Request::Send {
                    device: 0,
                    event: "press".to_string(),
                    data: None,
                },
                &mut node,
            ),
            Response::Sent { device: 0 }
        );
        dispatch(Request::Tick { advance_us: 1_000 }, &mut node);
        assert_eq!(value_of(&mut node), "0063");

        // Sending to an unknown device is an error, not a panic.
        assert!(matches!(
            dispatch(
                Request::Send {
                    device: 99,
                    event: "press".to_string(),
                    data: None,
                },
                &mut node,
            ),
            Response::Error { .. }
        ));
    }

    // A node holds many controllers at once; `route` moves a device between them
    // keeping its handle, and `create` mints a private ether to route onto.
    #[test]
    fn route_and_create_move_a_device_between_controllers() {
        let mut node = None;
        // Two devices, one on `link`, one on a freshly-created private ether.
        assert_eq!(
            dispatch(
                Request::Create {
                    network: "arena".to_string()
                },
                &mut node
            ),
            Response::Created {
                network: "arena".to_string()
            }
        );
        assert!(matches!(
            dispatch(run_req("link", BARE_DEVICE), &mut node),
            Response::Ran { device: 0, .. }
        ));
        assert!(matches!(
            dispatch(run_req("arena", BARE_DEVICE), &mut node),
            Response::Ran { device: 1, .. }
        ));

        // Both list, each on its own controller — the node holds both at once.
        let Response::Devices { devices } = dispatch(Request::ListDevices, &mut node) else {
            panic!("list");
        };
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].controller, "link");
        assert_eq!(devices[1].controller, "arena");

        // Route device 0 from `link` onto `arena`: same handle, new controller.
        assert_eq!(
            dispatch(
                Request::Route {
                    device: 0,
                    controller: "arena".to_string(),
                },
                &mut node,
            ),
            Response::Routed {
                device: 0,
                controller: "arena".to_string(),
            }
        );
        let Response::Devices { devices } = dispatch(Request::ListDevices, &mut node) else {
            panic!("list");
        };
        let d0 = devices.iter().find(|d| d.index == 0).unwrap();
        assert_eq!(d0.controller, "arena", "device 0 moved to arena");

        // The created ether shows up as both a controller and a network.
        let Response::Controllers { controllers } = dispatch(Request::ListControllers, &mut node)
        else {
            panic!("controllers");
        };
        assert!(controllers.iter().any(|c| c.name == "arena"));
        let Response::Networks { networks } = dispatch(Request::ListNetworks, &mut node) else {
            panic!("networks");
        };
        assert!(networks.iter().any(|n| n.name == "arena"));

        // Routing an unknown device errors; creating a duplicate ether errors.
        assert!(matches!(
            dispatch(
                Request::Route {
                    device: 99,
                    controller: "link".to_string(),
                },
                &mut node,
            ),
            Response::Error { .. }
        ));
        assert!(matches!(
            dispatch(
                Request::Create {
                    network: "arena".to_string()
                },
                &mut node
            ),
            Response::Error { .. }
        ));
    }

    // `register` admits an external node into `list_nodes` (bookkeeping only).
    #[test]
    fn register_admits_an_external_node() {
        let mut node = None;
        let phone = NodeInfo {
            name: "pixel".to_string(),
            kind: "android".to_string(),
            controllers: vec!["android-0".to_string()],
        };
        assert_eq!(
            dispatch(
                Request::Register {
                    node: phone.clone()
                },
                &mut node
            ),
            Response::Registered {
                node: "pixel".to_string()
            }
        );
        let Response::Nodes { nodes } = dispatch(Request::ListNodes, &mut node) else {
            panic!("nodes");
        };
        // The local router is always first; the registered phone follows.
        assert_eq!(nodes[0].name, "local");
        assert!(
            nodes
                .iter()
                .any(|n| n.name == "pixel" && n.kind == "android")
        );
    }

    /// A `run` request for `controller`/`script` with an auto-allocated address.
    fn run_req(controller: &str, script: &str) -> Request {
        Request::Run {
            controller: controller.to_string(),
            script: script.to_string(),
            address: None,
        }
    }

    // The factory seam: an external backend injects its own controllers without
    // `simble-stack` knowing them — the pattern a `rootcanal-rs` backend uses.
    #[test]
    fn a_custom_controller_factory_injects_new_controllers() {
        // An external backend's factory offers one controller, "backend-0",
        // built from a LinkScene here (a rootcanal-rs actor in the real thing).
        struct BackendFactory;
        impl ControllerFactory for BackendFactory {
            fn create(&self, name: &str) -> Result<Box<dyn crate::transport::Scene>, String> {
                if name == "backend-0" {
                    Ok(Box::new(crate::transport::link_scene::LinkScene::new()))
                } else {
                    Err(format!("BackendFactory does not offer {name:?}"))
                }
            }
            fn available(&self) -> Vec<Controller> {
                vec![Controller {
                    name: "backend-0".to_string(),
                    kind: "rootcanal".to_string(),
                    api_class: "hci".to_string(),
                    network: "backend-net".to_string(),
                    real: false,
                    deterministic: false,
                    attachable: true,
                    product: Some("injected backend".to_string()),
                }]
            }
        }

        // The app composes the backend with the built-in controllers.
        let factory =
            CompositeFactory::new(vec![Box::new(BackendFactory), Box::new(BuiltinControllers)]);
        let mut node = Some(Node::with_factory(Box::new(factory)));

        // The list shows the injected controller alongside the built-in `link`.
        let Response::Controllers { controllers } = dispatch(Request::ListControllers, &mut node)
        else {
            panic!("controllers");
        };
        assert!(
            controllers
                .iter()
                .any(|c| c.name == "backend-0" && c.kind == "rootcanal"),
            "the injected controller must be listed"
        );
        assert!(controllers.iter().any(|c| c.name == "link"));

        // A device runs on the injected controller (its factory built the scene)…
        assert!(matches!(
            dispatch(run_req("backend-0", BARE_DEVICE), &mut node),
            Response::Ran { device: 0, .. }
        ));
        // …and on the built-in one too, through the same composed factory.
        assert!(matches!(
            dispatch(run_req("link", BARE_DEVICE), &mut node),
            Response::Ran { device: 1, .. }
        ));
        // A device can then route from the built-in world onto the injected one.
        assert_eq!(
            dispatch(
                Request::Route {
                    device: 1,
                    controller: "backend-0".to_string(),
                },
                &mut node,
            ),
            Response::Routed {
                device: 1,
                controller: "backend-0".to_string(),
            }
        );
    }
}
