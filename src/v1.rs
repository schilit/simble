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
//! Implemented so far: the four observability lists (`list_controllers` /
//! `list_networks` / `list_devices` / `list_nodes`), `run`, `stop`, `send`,
//! `tick`, and `get_clock` (over a `Node` execution core, runnable on the
//! deterministic `link` controller with no hardware). `spawn` / `attach` /
//! `route` and `create` / `register` follow.

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

/// A v1 node's execution state: the controller it runs on and the devices on
/// it. One controller per node for now (many controllers is the router). MCP
/// keeps its own `Server`; this is the v1 interface's node, and both sit on the
/// same `transport::Scene` trait underneath.
#[cfg(not(target_arch = "wasm32"))]
pub struct Node {
    scene: Box<dyn crate::transport::Scene>,
    next_addr: u16,
}

#[cfg(not(target_arch = "wasm32"))]
impl Node {
    /// A node running on `scene`.
    pub fn new(scene: Box<dyn crate::transport::Scene>) -> Self {
        Self {
            scene,
            next_addr: 1,
        }
    }

    /// The controller this node runs on.
    pub fn controller(&self) -> &'static str {
        self.scene.name()
    }

    /// Runs `script` as a device on this node's controller; returns its index.
    /// A deterministic address is allocated when `address` is `None`.
    pub fn run(
        &mut self,
        script: &str,
        address: Option<crate::types::Address>,
    ) -> Result<usize, String> {
        let address = address.unwrap_or_else(|| self.next_address());
        self.scene.add_peripheral(address, script)
    }

    fn next_address(&mut self) -> crate::types::Address {
        let n = self.next_addr;
        self.next_addr = self.next_addr.wrapping_add(1);
        // Stable and obviously simulated, in the CC:1E:57 space simble uses.
        crate::types::Address::new([(n & 0xff) as u8, (n >> 8) as u8, 0x00, 0x57, 0x1e, 0xcc])
    }

    /// Moves packets both ways for every device on this node.
    pub fn pump(&mut self) {
        self.scene.pump();
    }

    /// Advances this node's simulated clock by `advance_us` microseconds and
    /// returns the absolute clock (µs) of the next scheduled event, `None` if
    /// nothing is scheduled — so the caller waits *until* the deadline rather than
    /// spinning.
    pub fn tick(&mut self, advance_us: u64) -> Option<u64> {
        self.scene.tick(advance_us)
    }

    /// This node's current simulated clock, in microseconds.
    pub fn now_us(&self) -> u64 {
        self.scene.now_us()
    }

    /// The absolute clock (µs) of this node's next scheduled event, so a host can
    /// wait until it instead of spinning. `None` means nothing is scheduled — wait
    /// for a packet or poll. A real value appears once a device declares a wake
    /// time with the `wake_at` script binding.
    pub fn next_deadline_us(&self) -> Option<u64> {
        self.scene.next_deadline_us()
    }

    /// The devices running on this node — stopped tombstones are skipped, so a
    /// device's index stays a stable handle across other devices' teardowns.
    pub fn list_devices(&self) -> Vec<Device> {
        (0..self.scene.device_count())
            .filter(|&index| !self.scene.device_stopped(index))
            .map(|index| Device {
                index,
                controller: self.scene.name().to_string(),
                status: self
                    .scene
                    .peripheral_status_json(index)
                    .and_then(|s| serde_json::from_str(&s).ok()),
            })
            .collect()
    }

    /// Stops (tears down) the device at `index` on this node.
    pub fn stop(&mut self, index: usize) -> Result<(), String> {
        self.scene.stop(index)
    }

    /// Delivers an input `event` (with an optional JSON `data` payload) to the
    /// device at `index` — its script sees it in `fn on_event` on the next tick.
    pub fn send(
        &mut self,
        index: usize,
        event: &str,
        data: Option<&serde_json::Value>,
    ) -> Result<(), String> {
        let data_json = data
            .map(|v| v.to_string())
            .unwrap_or_else(|| "{}".to_string());
        self.scene.send(index, event, &data_json)
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
    match request {
        Request::ListControllers => Response::Controllers {
            controllers: list_controllers(),
        },
        Request::ListNetworks => Response::Networks {
            networks: list_networks(),
        },
        Request::ListDevices => Response::Devices {
            devices: node.as_ref().map(Node::list_devices).unwrap_or_default(),
        },
        Request::ListNodes => Response::Nodes {
            nodes: list_nodes(),
        },
        Request::Run {
            controller,
            script,
            address,
        } => {
            // Select the controller (build its scene) if not already on it.
            if node.as_ref().map(Node::controller) != Some(controller.as_str()) {
                match scene_for_controller(&controller) {
                    Ok(scene) => *node = Some(Node::new(scene)),
                    Err(message) => return Response::Error { message },
                }
            }
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
            let node = node.as_mut().expect("just selected");
            match node.run(&script, address) {
                Ok(device) => Response::Ran { device, controller },
                Err(message) => Response::Error { message },
            }
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
            None => Response::Error {
                message: "no device running — run something first".to_string(),
            },
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
            None => Response::Error {
                message: "no device running — run something first".to_string(),
            },
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
            None => Response::Error {
                message: "no device running — run something first".to_string(),
            },
        },
    }
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

    /// A `Scene` that records what it is told to run — enough to exercise `Node`
    /// and the `run` op without netsim or a dongle in the loop.
    struct MockScene {
        added: Vec<(crate::types::Address, String)>,
    }

    impl crate::transport::Scene for MockScene {
        fn name(&self) -> &'static str {
            "mock"
        }
        fn add_peripheral(
            &mut self,
            address: crate::types::Address,
            script: &str,
        ) -> Result<usize, String> {
            self.added.push((address, script.to_string()));
            Ok(self.added.len() - 1)
        }
        fn pump(&mut self) {}
        fn tick(&mut self, _advance_us: u64) -> Option<u64> {
            None
        }
        fn now_us(&self) -> u64 {
            0
        }
        fn device_count(&self) -> usize {
            self.added.len()
        }
        fn peripheral_status_json(&self, index: usize) -> Option<String> {
            (index < self.added.len()).then(|| format!(r#"{{"index":{index}}}"#))
        }
    }

    #[test]
    fn node_run_adds_a_device_and_lists_it() {
        let mut node = Node::new(Box::new(MockScene { added: Vec::new() }));
        assert_eq!(node.controller(), "mock");
        let index = node.run("peripheral(\"x\") {}", None).unwrap();
        assert_eq!(index, 0);

        let devices = node.list_devices();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].index, 0);
        assert_eq!(devices[0].controller, "mock");
        assert_eq!(devices[0].status, Some(serde_json::json!({"index": 0})));
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
        // With a node, tick answers Ticked with the clock (fixed at 0 in the mock).
        let mut node = Some(Node::new(Box::new(MockScene { added: Vec::new() })));
        let Response::Ticked {
            now_us,
            deadline_us,
        } = dispatch(Request::Tick { advance_us: 1500 }, &mut node)
        else {
            panic!("tick must answer Ticked");
        };
        assert_eq!(now_us, 0);
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

        let mut node = Some(Node::new(Box::new(MockScene { added: Vec::new() })));
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
}
