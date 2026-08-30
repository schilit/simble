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
//! Only `list_controllers` is implemented so far — the foundational
//! observability op. `run` / `spawn` / `route` / the other verbs follow.

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

/// A v1 control request. Tagged by `op` on the wire: `{"op":"list_controllers"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Enumerate the controllers this node offers.
    ListControllers,
}

/// A v1 control response. Tagged by `type` on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    /// The answer to `list_controllers`.
    Controllers {
        /// The controllers this node offers, in a stable order.
        controllers: Vec<Controller>,
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

/// Handles one v1 request and produces its response.
pub fn dispatch(request: Request) -> Response {
    match request {
        Request::ListControllers => Response::Controllers {
            controllers: list_controllers(),
        },
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
        let Response::Controllers { controllers } = dispatch(Request::ListControllers) else {
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
}
