// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! `android::BluetoothGatt` — the **central** half of the scripting surface.
//!
//! The peripheral surface is deliberately Android-shaped
//! (`BluetoothGattServer` + a callback), so the client one mirrors Android's
//! `BluetoothGatt` + `BluetoothGattCallback`:
//!
//! ```rhai
//! let client = android::BluetoothGatt("Client");
//! client.connect("AA:BB:CC:00:00:01");
//!
//! fn on_services_discovered(client) {
//!     client.subscribe(uuid::HEART_RATE_MEASUREMENT);
//! }
//! fn on_characteristic_changed(client, uuid, value) {
//!     assert(value[1] < 200, "plausible heart rate");
//! }
//! ```
//!
//! Callbacks are free functions the script defines, dispatched by
//! [`ScriptedCentral`] — the same mechanism the peripheral's `fn tick(server,
//! t)` and `fn on_event(server, event)` already use, with `this` bound to a
//! persistent map so a client can remember things between calls. (Assigning
//! Rhai closures to a callback object, which is what Android would do, is
//! structurally impossible here for the reason given in the
//! [`crate::scripting`] module docs.) `fn on_event(client, event)` is
//! available too, for a script that would rather match on one event stream
//! than define six handlers.
//!
//! Everything below the bindings is [`LeCentral`], which owns the protocol
//! and no transport. [`ScriptedCentral`] is likewise transport-free: HCI
//! packets in, HCI packets out, so the same scripted client runs on the
//! in-process radio, on netsim, or against a foreign stack.
//!
//! **Not in scope, on purpose**: isochronous streams and the L2CAP-level
//! roles (CIS, RFCOMM, SCO, A2DP). Those stay in Rust and are reachable as
//! registrars, the way `add_pacs`/`add_ascs` are on the server side.

use std::cell::RefCell;
use std::rc::Rc;

use rhai::{AST, Array, Blob, CallFnOptions, Dynamic, Engine, EvalAltResult, Map, Module, Scope};

use crate::device::central::{CentralEvent, LeCentral};
use crate::scripting::bindings::{dynamic_to_bytes, runtime_error};

/// LE Set Scan Parameters (Vol 4, Part E, Section 7.8.10).
const LE_SET_SCAN_PARAMETERS: [u8; 2] = [0x0B, 0x20];
/// LE Set Scan Enable (Section 7.8.11).
const LE_SET_SCAN_ENABLE: [u8; 2] = [0x0C, 0x20];
use crate::types::{Address, Uuid};

/// Script-side handle to a GATT client. Rhai registered types must be
/// `Clone`, and a live state machine must not be — so this is an `Rc<RefCell>`
/// that clones shallowly, exactly as `ScriptGattServer` does, and every copy
/// a script holds drives the same client.
#[derive(Clone)]
pub struct ScriptGattClient {
    inner: Rc<RefCell<ClientInner>>,
}

struct ClientInner {
    name: String,
    central: LeCentral,
    /// H4 packets produced by a script call (`connect`, `disconnect`) that
    /// the host has not sent yet.
    outbox: Vec<Vec<u8>>,
    /// Messages the script emitted for the host, as JSON strings.
    emitted: Vec<String>,
}

impl ScriptGattClient {
    /// Crate-visible so a *profile* proxy that is a central underneath — the
    /// Broadcast Assistant — can borrow the whole central role rather than
    /// reimplementing a connection.
    pub(crate) fn create_for(name: &str) -> Self {
        Self::create(name)
    }

    fn create(name: &str) -> Self {
        Self {
            inner: Rc::new(RefCell::new(ClientInner {
                name: name.to_string(),
                central: LeCentral::new(),
                outbox: Vec::new(),
                emitted: Vec::new(),
            })),
        }
    }

    /// The client's name (what a page or a scene labels it with).
    pub fn name(&self) -> String {
        self.inner.borrow().name.clone()
    }

    /// Host-side access to the state machine underneath, for the runner and
    /// for Rust tests that want to drive a scripted client directly.
    pub fn with_central<R>(&self, f: impl FnOnce(&mut LeCentral) -> R) -> R {
        f(&mut self.inner.borrow_mut().central)
    }

    /// Points the client at `target`, queueing the controller bring-up — the
    /// same path `client.connect("AA:BB:…")` takes from a script.
    pub fn connect(&self, target: Address) {
        let mut inner = self.inner.borrow_mut();
        let packets = inner.central.connect(target);
        inner.outbox.extend(packets);
    }

    /// Tears the connection down, queueing the Disconnect — the same path
    /// `client.disconnect()` takes from a script. Public so a profile proxy
    /// built on this client offers the same call without reaching into it.
    pub fn disconnect(&self) {
        let mut inner = self.inner.borrow_mut();
        let packets = inner.central.disconnect();
        inner.outbox.extend(packets);
    }

    /// Drains the packets script calls have queued for the controller.
    pub fn take_outbox(&self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.inner.borrow_mut().outbox)
    }

    /// The return path to the host, as `client.emit(kind, payload)` uses it.
    /// Public so a proxy built on this client offers the same `emit`.
    pub fn emit(&self, kind: &str, payload: Dynamic) -> Result<(), Box<EvalAltResult>> {
        let payload = bytes_as_numbers(payload);
        let value: serde_json::Value = rhai::serde::from_dynamic(&payload)
            .map_err(|e| runtime_error(format!("emit payload is not serializable: {e}")))?;
        let message = serde_json::json!({ "event": kind, "payload": value });
        self.push_emitted(message.to_string());
        Ok(())
    }

    /// Drains what the script emitted for the host, oldest first.
    pub fn take_emitted(&self) -> Vec<String> {
        std::mem::take(&mut self.inner.borrow_mut().emitted)
    }

    fn push_emitted(&self, message: String) {
        const MAX_EMITTED: usize = 512;
        let mut inner = self.inner.borrow_mut();
        if inner.emitted.len() >= MAX_EMITTED {
            inner.emitted.remove(0);
        }
        inner.emitted.push(message);
    }
}

/// A scan filter, in Android's shape: `ScanFilter.Builder().setServiceUuid()`
/// flattened to a constructor, because a script has no use for a builder it
/// would call once.
#[derive(Debug, Clone)]
pub struct ScriptScanFilter {
    /// The service an advertisement must carry to be reported.
    pub service_uuid: Option<Uuid>,
}

/// Android's `BluetoothLeScanner`: find peers by what they advertise, rather
/// than by an address the script was told in advance.
///
/// This is the primitive that makes a script able to meet a peer it does not
/// already know. A phone advertises from a resolvable private address that
/// rotates and that Android will not disclose even to its own app, so there
/// is no address to write into a script — only a service to look for.
#[derive(Debug, Clone, Default)]
pub struct ScriptScanner {
    inner: Rc<RefCell<ScannerInner>>,
}

#[derive(Debug, Default)]
struct ScannerInner {
    scanning: bool,
    filter: Option<ScriptScanFilter>,
    /// Set once bring-up has been queued, so it is queued exactly once.
    started: bool,
}

impl ScriptScanner {
    /// Whether the script asked for a scan that has not been queued yet.
    pub fn needs_start(&self) -> bool {
        let inner = self.inner.borrow();
        inner.scanning && !inner.started
    }

    /// Marks the bring-up as queued.
    pub fn mark_started(&self) {
        self.inner.borrow_mut().started = true;
    }

    /// Whether the scan is still running.
    pub fn scanning(&self) -> bool {
        self.inner.borrow().scanning
    }

    /// Whether `uuids` satisfies the filter the script set.
    pub fn matches(&self, uuids: &[String]) -> bool {
        match self.inner.borrow().filter.as_ref().and_then(|f| f.service_uuid) {
            None => true,
            Some(wanted) => {
                let wanted = wanted.to_string();
                uuids.iter().any(|u| u.eq_ignore_ascii_case(&wanted))
            }
        }
    }
}

/// Registers the client type, its methods and the `android::BluetoothGatt`
/// constructor. Called from [`crate::scripting::new_engine`], so every
/// surface — the playground, `run_test`, MCP, the pages — sees the same
/// client API.
pub fn register(engine: &mut Engine, android: &mut Module) {
    engine
        .register_type_with_name::<ScriptGattClient>("BluetoothGatt")
        .register_get("name", |client: &mut ScriptGattClient| client.name())
        .register_get("peer", |client: &mut ScriptGattClient| {
            client.with_central(|c| c.target().to_string())
        })
        .register_get("state", |client: &mut ScriptGattClient| {
            client.with_central(|c| c.phase().label().to_string())
        })
        .register_get("connected", |client: &mut ScriptGattClient| {
            client.with_central(|c| c.connection_handle() != 0)
        })
        // True once discovery has finished: the moment at which naming a
        // characteristic by UUID starts working.
        .register_get("discovered", |client: &mut ScriptGattClient| {
            client.with_central(|c| c.is_ready())
        })
        // True when every queued operation has been sent *and* answered.
        .register_get("idle", |client: &mut ScriptGattClient| {
            client.with_central(|c| c.is_idle())
        })
        .register_get("mtu", |client: &mut ScriptGattClient| {
            client.with_central(|c| c.mtu() as i64)
        })
        .register_fn(
            "connect",
            |client: &mut ScriptGattClient, address: &str| -> Result<(), Box<EvalAltResult>> {
                let address = address.parse::<Address>().map_err(|e| {
                    runtime_error(format!("connect: {address:?} is not an address: {e}"))
                })?;
                client.connect(address);
                Ok(())
            },
        )
        .register_fn("disconnect", ScriptGattClient::disconnect)
        .register_type_with_name::<ScriptScanFilter>("ScanFilter")
        .register_type_with_name::<ScriptScanner>("BluetoothLeScanner")
        .register_get("scanning", |s: &mut ScriptScanner| s.scanning())
        .register_fn(
            "start_scan",
            |scanner: &mut ScriptScanner, filter: ScriptScanFilter| {
                let mut inner = scanner.inner.borrow_mut();
                inner.filter = Some(filter);
                inner.scanning = true;
            },
        )
        // Android allows a scan with no filters at all; so does this.
        .register_fn("start_scan", |scanner: &mut ScriptScanner| {
            let mut inner = scanner.inner.borrow_mut();
            inner.filter = None;
            inner.scanning = true;
        })
        .register_fn("stop_scan", |scanner: &mut ScriptScanner| {
            scanner.inner.borrow_mut().scanning = false;
        })
        .register_fn("read", |client: &mut ScriptGattClient, uuid: Uuid| {
            client.with_central(|c| c.queue_read(uuid));
        })
        .register_fn(
            // Write Request: acknowledged, answered by `on_characteristic_write`.
            "write",
            |client: &mut ScriptGattClient,
             uuid: Uuid,
             value: Dynamic|
             -> Result<(), Box<EvalAltResult>> {
                let bytes = dynamic_to_bytes(value)?;
                client.with_central(|c| c.queue_write(uuid, bytes, true));
                Ok(())
            },
        )
        .register_fn(
            // Write Command: unacknowledged (Vol 3, Part F, 3.4.5.3). The
            // peer never answers one, so `on_characteristic_write` fires as
            // soon as it goes out.
            "write_command",
            |client: &mut ScriptGattClient,
             uuid: Uuid,
             value: Dynamic|
             -> Result<(), Box<EvalAltResult>> {
                let bytes = dynamic_to_bytes(value)?;
                client.with_central(|c| c.queue_write(uuid, bytes, false));
                Ok(())
            },
        )
        .register_fn("subscribe", |client: &mut ScriptGattClient, uuid: Uuid| {
            client.with_central(|c| c.queue_subscribe(uuid, true));
        })
        .register_fn(
            "unsubscribe",
            |client: &mut ScriptGattClient, uuid: Uuid| {
                client.with_central(|c| c.queue_subscribe(uuid, false));
            },
        )
        .register_fn(
            // The last bytes seen for a characteristic, read or notified.
            // Empty when nothing has arrived on it — a script asserting on a
            // value it never received sees an empty blob, not a stale one.
            "value",
            |client: &mut ScriptGattClient, uuid: Uuid| -> Blob {
                client.with_central(|c| c.value(uuid).map(<[u8]>::to_vec).unwrap_or_default())
            },
        )
        .register_fn(
            "is_subscribed",
            |client: &mut ScriptGattClient, uuid: Uuid| {
                client.with_central(|c| c.is_subscribed(uuid))
            },
        )
        .register_fn(
            "has_characteristic",
            |client: &mut ScriptGattClient, uuid: Uuid| {
                client.with_central(|c| c.value_handle(uuid).is_some())
            },
        )
        .register_fn("services", |client: &mut ScriptGattClient| -> Array {
            client.with_central(|c| c.services().iter().map(|s| Dynamic::from(s.uuid)).collect())
        })
        .register_fn(
            "characteristics",
            |client: &mut ScriptGattClient, service: Uuid| -> Array {
                client.with_central(|c| {
                    c.services()
                        .iter()
                        .filter(|s| s.uuid == service)
                        .flat_map(|s| s.characteristics.iter())
                        .map(|ch| Dynamic::from(ch.uuid))
                        .collect()
                })
            },
        )
        .register_fn(
            // The return path to the host, mirroring `server.emit`: a client
            // tells a page or a test something that isn't GATT state.
            "emit",
            |client: &mut ScriptGattClient,
             kind: &str,
             payload: Dynamic|
             -> Result<(), Box<EvalAltResult>> { client.emit(kind, payload) },
        );

    // `android::BluetoothGatt("name")` — the type name as a module function,
    // the same constructor idiom the server bindings use (`new` is a reserved
    // word in Rhai). A central has no address of its own to allocate: the
    // controller it runs on supplies one.
    android.set_native_fn(
        "BluetoothGatt",
        |name: &str| -> Result<ScriptGattClient, Box<EvalAltResult>> {
            Ok(ScriptGattClient::create(name))
        },
    );
    android.set_native_fn(
        "BluetoothLeScanner",
        || -> Result<ScriptScanner, Box<EvalAltResult>> { Ok(ScriptScanner::default()) },
    );
    // Android's builder collapsed to a constructor: a script would call
    // `setServiceUuid` once and `build` immediately.
    android.set_native_fn(
        "ScanFilter",
        |uuid: Uuid| -> Result<ScriptScanFilter, Box<EvalAltResult>> {
            Ok(ScriptScanFilter {
                service_uuid: Some(uuid),
            })
        },
    );
}

/// The script functions a central script may define. Looked up once, at
/// compile time, so a tick does not pay for handlers that aren't there.
#[derive(Default, Debug, Clone, Copy)]
struct Handlers {
    tick: bool,
    connection_state_change: bool,
    services_discovered: bool,
    characteristic_read: bool,
    characteristic_write: bool,
    characteristic_changed: bool,
    subscribed: bool,
    mtu_changed: bool,
    scan_result: bool,
    error: bool,
    event: bool,
}

impl Handlers {
    fn detect(ast: &AST) -> Self {
        let has = |name: &str, arity: usize| {
            ast.iter_functions()
                .any(|f| f.name == name && f.params.len() == arity)
        };
        Self {
            tick: has("tick", 2),
            connection_state_change: has("on_connection_state_change", 2),
            services_discovered: has("on_services_discovered", 1),
            characteristic_read: has("on_characteristic_read", 3),
            characteristic_write: has("on_characteristic_write", 3),
            characteristic_changed: has("on_characteristic_changed", 3),
            subscribed: has("on_subscribed", 2),
            mtu_changed: has("on_mtu_changed", 2),
            scan_result: has("on_scan_result", 2),
            error: has("on_error", 2),
            event: has("on_event", 2),
        }
    }
}

/// A Rhai script hosted as a GATT client: the engine, the client it built,
/// and the callback dispatch that turns [`CentralEvent`]s into script calls.
///
/// Transport-free, like [`LeCentral`] and
/// [`LeHost`](crate::device::LeHost): [`Self::take_outbox`] hands out H4
/// packets to send, [`Self::on_packet`] takes one in. A caller supplies the
/// transport and the clock.
pub struct ScriptedCentral {
    engine: Engine,
    ast: AST,
    scope: Scope<'static>,
    client: ScriptGattClient,
    /// The profile proxy this script actually built, when it built one rather
    /// than a bare `BluetoothGatt`. A Broadcast Assistant *is* a central, so
    /// it is hosted here; the BASS-shaped callbacks it owes the script are
    /// derived from the same [`CentralEvent`] stream, by the proxy itself.
    assistant: Option<crate::scripting::broadcast::ScriptBroadcastAssistant>,
    /// The HID host this script built, if it built one. Hosted here for the
    /// same reason the Assistant is: a HID host *is* a central, and the
    /// HOGP-shaped callbacks it owes the script are derived from the same
    /// [`CentralEvent`] stream by the proxy itself.
    hid: Option<crate::scripting::hid::ScriptHidHost>,
    /// The scanner the script built, if it built one. A script that scans is
    /// looking for a peer it cannot name, so this replaces the address a host
    /// would otherwise have to supply.
    scanner: Option<ScriptScanner>,
    /// Per-client state bound as `this` for every handler — script functions
    /// are pure and cannot see the calling scope, so this map is the only
    /// thing a client can remember between calls.
    state: Dynamic,
    handlers: Handlers,
    /// The most recent handler error, cleared by the next clean dispatch.
    last_error: Option<String>,
    /// The *first* handler error, kept forever. A failed `assert` inside a
    /// callback is the whole point of a test script, so it must not be
    /// overwritten by whatever the client did next.
    failure: Option<String>,
}

impl ScriptedCentral {
    /// Compiles and runs `script`, which must leave an
    /// `android::BluetoothGatt` in a top-level variable.
    pub fn run_script(script: &str) -> Result<Self, String> {
        let engine = crate::scripting::new_engine();
        let ast = engine.compile(script).map_err(|e| e.to_string())?;
        let mut scope = Scope::new();
        engine
            .run_ast_with_scope(&mut scope, &ast)
            .map_err(|e| e.to_string())?;

        // A profile proxy that is a central underneath counts as a client: the
        // Broadcast Assistant and the HID host are hosted exactly like a
        // `BluetoothGatt`, and only their extra callbacks differ.
        let assistant = scope.iter().find_map(|(_, _, value)| {
            value.try_cast::<crate::scripting::broadcast::ScriptBroadcastAssistant>()
        });
        let hid = scope
            .iter()
            .find_map(|(_, _, value)| value.try_cast::<crate::scripting::hid::ScriptHidHost>());
        let client = assistant
            .as_ref()
            .map(|a| a.client())
            .or_else(|| hid.as_ref().map(|h| h.client()))
            .or_else(|| {
                scope
                    .iter()
                    .find_map(|(_, _, value)| value.try_cast::<ScriptGattClient>())
            });
        let client = client.ok_or_else(|| {
            "script must create an android::BluetoothGatt (or an \
             android::BluetoothLeBroadcastAssistant or android::BluetoothHidHost) \
             and keep it in a top-level variable"
                .to_string()
        })?;

        let scanner = scope
            .iter()
            .find_map(|(_, _, value)| value.try_cast::<ScriptScanner>());

        let handlers = Handlers::detect(&ast);
        Ok(Self {
            engine,
            ast,
            scope,
            client,
            assistant,
            hid,
            scanner,
            state: Dynamic::from_map(Map::new()),
            handlers,
            last_error: None,
            failure: None,
        })
    }

    /// The script's client handle, for a host that wants to drive it
    /// directly (a page button, a Rust test).
    pub fn client(&self) -> &ScriptGattClient {
        &self.client
    }

    /// The client's name.
    pub fn name(&self) -> String {
        self.client.name()
    }

    /// Re-points the client at `target`, discarding whatever address its
    /// script named.
    ///
    /// A script naming its own peer is right in the playground and in a scene
    /// file, where the topology is written down. It is wrong wherever the
    /// host allocates addresses — MCP, a page that spawns devices — because
    /// the script cannot know them. Topology beats script, so this overrides.
    pub fn set_target(&mut self, target: Address) {
        // Drop the bring-up the script's own `connect` queued: re-issuing it
        // would send Reset twice and confuse the phase gate.
        let _ = self.client.take_outbox();
        self.client.connect(target);
    }

    /// The Broadcast Assistant this script built, if it built one — the
    /// handle a host needs to drive or inspect the profile proxy directly.
    pub fn assistant(&self) -> Option<&crate::scripting::broadcast::ScriptBroadcastAssistant> {
        self.assistant.as_ref()
    }

    /// The HID host this script built, if it built one — the handle a host
    /// needs to show what the peer was identified as, or the raw bytes of the
    /// last report beside what they decoded to.
    pub fn hid(&self) -> Option<&crate::scripting::hid::ScriptHidHost> {
        self.hid.as_ref()
    }

    /// The first handler error, if any — a failed `assert` in a callback.
    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    /// The most recent handler error, if the last dispatch failed.
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Drains what the script emitted with `client.emit(...)`.
    pub fn take_emitted(&mut self) -> Vec<String> {
        self.client.take_emitted()
    }

    /// Queues a read from outside the script — a page button on the
    /// discovered tree, a test. It joins the same queue the script's own
    /// `client.read(uuid)` uses, so ordering is one story, not two.
    pub fn read(&mut self, uuid: Uuid) {
        self.client.with_central(|c| c.queue_read(uuid));
    }

    /// Queues a write from outside the script.
    pub fn write(&mut self, uuid: Uuid, value: Vec<u8>, with_response: bool) {
        self.client
            .with_central(|c| c.queue_write(uuid, value, with_response));
    }

    /// Queues a subscribe (or unsubscribe) from outside the script.
    pub fn subscribe(&mut self, uuid: Uuid, enable: bool) {
        self.client
            .with_central(|c| c.queue_subscribe(uuid, enable));
    }

    /// Drains the H4 packets the client has queued for the controller.
    pub fn take_outbox(&mut self) -> Vec<Vec<u8>> {
        let mut out = self.scan_bring_up();
        out.extend(self.client.take_outbox());
        out.extend(self.client.with_central(LeCentral::pump));
        out
    }

    /// Feeds one controller→host packet in and returns what to send back,
    /// dispatching any callbacks it triggered first — so a script that
    /// subscribes from `on_services_discovered` has its subscription on the
    /// wire in the same pass, rather than a tick later.
    pub fn on_packet(&mut self, packet: &[u8]) -> Vec<Vec<u8>> {
        self.dispatch_scan_reports(packet);
        let mut out = self.client.with_central(|c| c.on_packet(packet));
        self.dispatch_events();
        out.extend(self.take_outbox());
        out
    }

    /// Turns advertising reports into `on_scan_result(client, result)`.
    ///
    /// Read before [`LeCentral`] sees the packet, and never instead of it: a
    /// central waiting to hear its target needs the same reports.
    fn dispatch_scan_reports(&mut self, packet: &[u8]) {
        let Some(scanner) = self.scanner.clone() else {
            return;
        };
        if !scanner.scanning() || !self.handlers.scan_result {
            return;
        }
        for report in crate::transport::wasm_ws::parse_scan_reports(packet) {
            if !scanner.matches(&report.service_uuids) {
                continue;
            }
            let mut map = Map::new();
            map.insert("address".into(), Dynamic::from(report.address.clone()));
            map.insert(
                "address_type".into(),
                Dynamic::from(report.address_type.to_string()),
            );
            map.insert("rssi".into(), Dynamic::from(i64::from(report.rssi)));
            map.insert("connectable".into(), Dynamic::from(report.connectable));
            map.insert(
                "name".into(),
                report.name.clone().map_or(Dynamic::UNIT, Dynamic::from),
            );
            map.insert(
                "service_uuids".into(),
                Dynamic::from(
                    report
                        .service_uuids
                        .iter()
                        .map(|u| Dynamic::from(u.clone()))
                        .collect::<Array>(),
                ),
            );
            self.call("on_scan_result", (Dynamic::from_map(map),));
        }
    }

    /// The HCI a scanning script needs before any report can arrive.
    ///
    /// Queued once, and only for a script that actually asked to scan, so a
    /// client script that names its peer is unaffected.
    fn scan_bring_up(&mut self) -> Vec<Vec<u8>> {
        let Some(scanner) = self.scanner.clone() else {
            return Vec::new();
        };
        if !scanner.needs_start() {
            return Vec::new();
        }
        scanner.mark_started();
        let mut out = crate::device::host::init_commands();
        // Active scanning, so a peer's scan-response name is collected too;
        // a passive scan never solicits it.
        out.push(crate::device::host::command(
            LE_SET_SCAN_PARAMETERS,
            &[0x01, 0x10, 0x00, 0x10, 0x00, 0x00, 0x00],
        ));
        out.push(crate::device::host::command(LE_SET_SCAN_ENABLE, &[0x01, 0x00]));
        out
    }

    /// One host tick at `t_seconds` since start: dispatch pending callbacks,
    /// run `fn tick(client, t)` if defined, then send whatever that queued.
    pub fn tick(&mut self, t_seconds: f64) -> Vec<Vec<u8>> {
        self.dispatch_events();
        if self.handlers.tick {
            let args = (self.receiver(), t_seconds);
            // eval_ast(false): the script body already ran in `run_script`;
            // re-evaluating it would rebuild the client every tick.
            let options = CallFnOptions::new()
                .eval_ast(false)
                .bind_this_ptr(&mut self.state);
            let result = self.engine.call_fn_with_options::<Dynamic>(
                options,
                &mut self.scope,
                &self.ast,
                "tick",
                args,
            );
            record(&mut self.last_error, &mut self.failure, result);
        }
        self.take_outbox()
    }

    /// Turns queued [`CentralEvent`]s into script calls.
    fn dispatch_events(&mut self) {
        loop {
            let events = self.client.with_central(LeCentral::take_events);
            if events.is_empty() {
                return;
            }
            for event in events {
                self.dispatch_one(event);
            }
            // A handler may have produced more events synchronously (a write
            // command completes on the spot); loop until the queue is dry.
        }
    }

    fn dispatch_one(&mut self, event: CentralEvent) {
        // A profile proxy's callbacks come first: they are what the script
        // asked for, and the raw GATT ones are the layer underneath.
        if let Some(assistant) = self.assistant.clone() {
            let receiver = Dynamic::from(assistant.clone());
            for (name, args) in assistant.observe(&event) {
                if !crate::scripting::broadcast::defines(&self.ast, name, args.len() + 1) {
                    continue;
                }
                let mut all = vec![receiver.clone()];
                all.extend(args);
                self.invoke(name, all);
            }
        }
        if let Some(hid) = self.hid.clone() {
            let receiver = Dynamic::from(hid.clone());
            for (name, args) in hid.observe(&event) {
                if !crate::scripting::broadcast::defines(&self.ast, name, args.len() + 1) {
                    continue;
                }
                let mut all = vec![receiver.clone()];
                all.extend(args);
                self.invoke(name, all);
            }
        }
        if self.handlers.event {
            let map = event_map(&event);
            self.call("on_event", (Dynamic::from_map(map),));
        }
        match event {
            CentralEvent::ConnectionStateChange { connected, .. } => {
                if self.handlers.connection_state_change {
                    self.call("on_connection_state_change", (Dynamic::from(connected),));
                }
            }
            CentralEvent::MtuChanged { mtu } => {
                if self.handlers.mtu_changed {
                    self.call("on_mtu_changed", (Dynamic::from(mtu as i64),));
                }
            }
            CentralEvent::ServicesDiscovered { .. } => {
                if self.handlers.services_discovered {
                    self.call_0("on_services_discovered");
                }
            }
            CentralEvent::CharacteristicRead {
                uuid,
                value,
                status,
                ..
            } => {
                if self.handlers.characteristic_read {
                    self.call(
                        "on_characteristic_read",
                        (Dynamic::from(uuid), Dynamic::from_blob(value)),
                    );
                }
                if status != 0 && self.handlers.error {
                    self.call(
                        "on_error",
                        (Dynamic::from(format!(
                            "read {uuid}: ATT error {status:#04X}"
                        )),),
                    );
                }
            }
            CentralEvent::CharacteristicWrite { uuid, status, .. } => {
                if self.handlers.characteristic_write {
                    self.call(
                        "on_characteristic_write",
                        (Dynamic::from(uuid), Dynamic::from(status as i64)),
                    );
                }
            }
            CentralEvent::CharacteristicChanged { uuid, value, .. } => {
                if self.handlers.characteristic_changed {
                    self.call(
                        "on_characteristic_changed",
                        (Dynamic::from(uuid), Dynamic::from_blob(value)),
                    );
                }
            }
            CentralEvent::SubscriptionChanged {
                uuid,
                enabled,
                status,
                ..
            } => {
                if enabled && self.handlers.subscribed {
                    self.call("on_subscribed", (Dynamic::from(uuid),));
                }
                if status != 0 && self.handlers.error {
                    self.call(
                        "on_error",
                        (Dynamic::from(format!(
                            "subscribe {uuid}: ATT error {status:#04X}"
                        )),),
                    );
                }
            }
            CentralEvent::OperationFailed {
                uuid,
                operation,
                reason,
            } => {
                let message = format!("{operation} {uuid}: {reason}");
                if self.handlers.error {
                    self.call("on_error", (Dynamic::from(message.clone()),));
                }
                // An operation that cannot even start is a script bug, not a
                // peer's answer: record it so a test fails rather than
                // waiting for a callback that will never come.
                self.last_error = Some(message.clone());
                self.failure.get_or_insert(message);
            }
        }
    }

    /// The object a callback's first argument is bound to.
    ///
    /// When the script built a profile proxy, that proxy is what the script
    /// holds and what its handlers name — so the *central's own* callbacks
    /// (`tick`, `on_services_discovered`, `on_error`) must receive it too.
    /// Passing the bare client there instead is a trap: `fn tick(host, t)`
    /// would be handed a `BluetoothGatt`, and the first property access on it
    /// fails with "a getter is not registered", naming a type the script never
    /// mentioned.
    fn receiver(&self) -> Dynamic {
        if let Some(hid) = &self.hid {
            return Dynamic::from(hid.clone());
        }
        if let Some(assistant) = &self.assistant {
            return Dynamic::from(assistant.clone());
        }
        Dynamic::from(self.client.clone())
    }

    fn call_0(&mut self, name: &str) {
        self.invoke(name, vec![self.receiver()]);
    }

    fn call<A: IntoArgs>(&mut self, name: &str, args: A) {
        let mut all = vec![self.receiver()];
        args.push_into(&mut all);
        self.invoke(name, all);
    }

    fn invoke(&mut self, name: &str, args: Vec<Dynamic>) {
        let options = CallFnOptions::new()
            .eval_ast(false)
            .bind_this_ptr(&mut self.state);
        let result = self.engine.call_fn_with_options::<Dynamic>(
            options,
            &mut self.scope,
            &self.ast,
            name,
            args,
        );
        record(&mut self.last_error, &mut self.failure, result);
    }

    /// The client's view of the peer, as JSON: `{name, peer, connected,
    /// phase, mtu, services:[{uuid, characteristics:[…]}], last_error}` —
    /// the same shape the scene central reports, so pages and the MCP
    /// annotator read either one.
    pub fn status_json(&self) -> String {
        #[derive(serde::Serialize)]
        struct View {
            name: String,
            connected: bool,
            peer: String,
            phase: &'static str,
            mtu: u16,
            services: Vec<Svc>,
            #[serde(skip_serializing_if = "Option::is_none")]
            last_error: Option<String>,
            /// The first callback error — a failed `assert`. Separate from
            /// `last_error` because it is the *verdict*: a caller deciding
            /// whether a run passed reads this, and it never clears.
            #[serde(skip_serializing_if = "Option::is_none")]
            failure: Option<String>,
        }
        #[derive(serde::Serialize)]
        struct Svc {
            uuid: String,
            characteristics: Vec<Chr>,
        }
        #[derive(serde::Serialize)]
        struct Chr {
            uuid: String,
            value_handle: u16,
            properties: u8,
            #[serde(skip_serializing_if = "Option::is_none")]
            value: Option<String>,
            subscribed: bool,
        }
        let view = self.client.with_central(|c| View {
            name: String::new(),
            connected: c.connection_handle() != 0,
            peer: c.target().to_string(),
            phase: c.phase().label(),
            mtu: c.mtu(),
            services: c
                .services()
                .iter()
                .map(|s| Svc {
                    uuid: s.uuid.to_string(),
                    characteristics: s
                        .characteristics
                        .iter()
                        .map(|ch| Chr {
                            uuid: ch.uuid.to_string(),
                            value_handle: ch.value_handle,
                            properties: ch.properties,
                            value: c
                                .value_at(ch.value_handle)
                                .map(|v| v.iter().map(|b| format!("{b:02X}")).collect()),
                            subscribed: c.is_subscribed_at(ch.value_handle),
                        })
                        .collect(),
                })
                .collect(),
            last_error: self.last_error.clone(),
            failure: self.failure.clone(),
        });
        let view = View {
            name: self.client.name(),
            ..view
        };
        serde_json::to_string(&view).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
    }
}

/// Rewrites Rhai blobs into arrays of byte values, recursively.
///
/// `rhai::serde::from_dynamic` refuses a blob outright — "invalid type: byte
/// array, expected any valid JSON value" — so without this, `emit`ting the
/// bytes a script just received fails at runtime. That is the most natural
/// thing a *protocol* simulator can report, and every value that crosses the
/// wire here is bytes: a notified characteristic, a Report Map, an input
/// report. The error also named serialization rather than the offending
/// value, which made it read like a bug in `emit` itself.
///
/// Arrays and maps are walked because a blob is usually nested inside one:
/// `emit("input", #{ event: event, report: host.report })`.
fn bytes_as_numbers(value: Dynamic) -> Dynamic {
    if value.is_blob() {
        let bytes = value.cast::<Blob>();
        return Dynamic::from_array(bytes.into_iter().map(|b| Dynamic::from(b as i64)).collect());
    }
    if value.is_array() {
        let array = value.cast::<Array>();
        return Dynamic::from_array(array.into_iter().map(bytes_as_numbers).collect());
    }
    if value.is_map() {
        let map = value.cast::<Map>();
        return Dynamic::from_map(
            map.into_iter()
                .map(|(key, value)| (key, bytes_as_numbers(value)))
                .collect(),
        );
    }
    value
}

/// Keeps the newest error for display and the first one forever: a failed
/// assertion is the result of a test, and must survive later activity.
fn record(
    last_error: &mut Option<String>,
    failure: &mut Option<String>,
    result: Result<Dynamic, Box<EvalAltResult>>,
) {
    match result {
        Ok(_) => *last_error = None,
        Err(e) => {
            let message = e.to_string();
            *last_error = Some(message.clone());
            failure.get_or_insert(message);
        }
    }
}

/// The object-map form of an event, for `fn on_event(client, event)`. Same
/// vocabulary as the peripheral's event maps (`event.event`, `event.uuid`,
/// `event.value`, `event.status`), so a script that already reads one reads
/// the other.
fn event_map(event: &CentralEvent) -> Map {
    let mut map = Map::new();
    let mut set = |key: &str, value: Dynamic| {
        map.insert(key.into(), value);
    };
    match event {
        CentralEvent::ConnectionStateChange {
            peer,
            connected,
            status,
        } => {
            set(
                "event",
                if *connected {
                    "connected"
                } else {
                    "disconnected"
                }
                .into(),
            );
            set("peer", peer.to_string().into());
            set("status", (*status as i64).into());
        }
        CentralEvent::MtuChanged { mtu } => {
            set("event", "mtu_changed".into());
            set("mtu", (*mtu as i64).into());
        }
        CentralEvent::ServicesDiscovered { services } => {
            set("event", "services_discovered".into());
            set("services", (*services as i64).into());
        }
        CentralEvent::CharacteristicRead {
            uuid,
            handle,
            value,
            status,
        } => {
            set("event", "characteristic_read".into());
            set("uuid", Dynamic::from(*uuid));
            set("handle", (*handle as i64).into());
            set("value", Dynamic::from_blob(value.clone()));
            set("status", (*status as i64).into());
        }
        CentralEvent::CharacteristicWrite {
            uuid,
            handle,
            status,
        } => {
            set("event", "characteristic_write".into());
            set("uuid", Dynamic::from(*uuid));
            set("handle", (*handle as i64).into());
            set("status", (*status as i64).into());
        }
        CentralEvent::CharacteristicChanged {
            uuid,
            handle,
            value,
        } => {
            set("event", "characteristic_changed".into());
            set("uuid", Dynamic::from(*uuid));
            set("handle", (*handle as i64).into());
            set("value", Dynamic::from_blob(value.clone()));
        }
        CentralEvent::SubscriptionChanged {
            uuid,
            handle,
            enabled,
            status,
        } => {
            set("event", "subscription_changed".into());
            set("uuid", Dynamic::from(*uuid));
            set("handle", (*handle as i64).into());
            set("enabled", (*enabled).into());
            set("status", (*status as i64).into());
        }
        CentralEvent::OperationFailed {
            uuid,
            operation,
            reason,
        } => {
            set("event", "operation_failed".into());
            set("uuid", Dynamic::from(*uuid));
            set("operation", (*operation).into());
            set("reason", reason.clone().into());
        }
    }
    map
}

/// Lets [`ScriptedCentral::call`] take 1- and 2-argument tuples without a
/// macro or a `Vec` at every call site.
trait IntoArgs {
    fn push_into(self, args: &mut Vec<Dynamic>);
}

impl IntoArgs for (Dynamic,) {
    fn push_into(self, args: &mut Vec<Dynamic>) {
        args.push(self.0);
    }
}

impl IntoArgs for (Dynamic, Dynamic) {
    fn push_into(self, args: &mut Vec<Dynamic>) {
        args.push(self.0);
        args.push(self.1);
    }
}
