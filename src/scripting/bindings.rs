// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! `android::*` host bindings: the Rhai-visible surface over
//! `crate::android` (spec §05 — same vocabulary as the native Rust layer,
//! callable from scripts). Event delivery uses a queue rather than the
//! spec's sketched closure assignment; see the module doc in
//! `crate::scripting` for why closure dispatch is impossible in this
//! (non-`sync`) rhai build.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use rhai::{Array, Blob, Dynamic, Engine, EvalAltResult, Map, Module, Position};

use crate::android::device::BluetoothDevice;
use crate::android::gatt_server::{BluetoothGattServer, BluetoothGattServerCallback};
use crate::android::gatt_service::{
    BluetoothGattCharacteristic, BluetoothGattDescriptor, BluetoothGattService,
};
use crate::device::VirtualDevice;
use crate::gap::advertising::AdvertisingData;
use crate::types::{Address, AddressType, Uuid};

/// One recorded callback event, as plain owned data — every field is `Send`
/// so the recording callback satisfies `BluetoothGattServerCallback: Send`
/// even though Rhai values themselves could not.
#[derive(Debug, Clone)]
pub struct ScriptEvent {
    /// Event name, matched by `wait_for` (e.g. "connected",
    /// "characteristic_read").
    pub kind: String,
    /// Name of the server (its `VirtualDevice`) that produced the event, so
    /// one session queue can serve several scripted servers.
    pub server: String,
    /// The peer device involved in the event, if any.
    pub peer: Option<BluetoothDevice>,
    /// The characteristic/descriptor UUID the event concerns, if any.
    pub uuid: Option<Uuid>,
    /// The attribute value carried by the event, if any.
    pub value: Option<Vec<u8>>,
    /// ATT request id for read/write events, if any.
    pub request_id: Option<i32>,
    /// Byte offset for a read/write event, if any.
    pub offset: Option<i32>,
    /// Status/error code associated with the event, if any.
    pub status: Option<i32>,
    /// Negotiated ATT MTU for an MTU-change event, if any.
    pub mtu: Option<i32>,
    /// Whether a write event expects a response, if applicable.
    pub response_needed: Option<bool>,
    /// Free-form payload for host-pushed events (a UI action, a simulated
    /// condition), as a JSON object. Kept as a string rather than a
    /// `rhai::Map` because `ScriptEvent` must stay `Send` (it is produced
    /// inside the `Send` callback); it becomes real Dynamic values in
    /// `into_map`, and its keys merge in so `event.action` reads naturally.
    pub extra: Option<String>,
}

impl ScriptEvent {
    fn new(kind: &str, server: &str) -> Self {
        Self {
            kind: kind.to_string(),
            server: server.to_string(),
            peer: None,
            uuid: None,
            value: None,
            request_id: None,
            offset: None,
            status: None,
            mtu: None,
            response_needed: None,
            extra: None,
        }
    }

    /// An event pushed in from outside the stack — a UI control, a test, a
    /// host simulating a condition. `kind` is what the script matches on
    /// (`event.event == "ui"`), `payload` is merged in alongside.
    pub fn custom(kind: &str, server: &str, payload_json: String) -> Self {
        let mut event = Self::new(kind, server);
        event.extra = Some(payload_json);
        event
    }

    /// Converts to the object-map shape scripts see (`event.uuid`,
    /// `event.device`, ...). Absent fields are omitted rather than set to
    /// `()` so scripts can use `"uuid" in event` naturally.
    fn into_map(self) -> Map {
        let mut map = Map::new();
        map.insert("event".into(), self.kind.into());
        map.insert("server".into(), self.server.into());
        if let Some(peer) = self.peer {
            map.insert("device".into(), Dynamic::from(peer));
        }
        if let Some(uuid) = self.uuid {
            map.insert("uuid".into(), Dynamic::from(uuid));
        }
        if let Some(value) = self.value {
            map.insert("value".into(), Dynamic::from_blob(value));
        }
        if let Some(request_id) = self.request_id {
            map.insert("request_id".into(), Dynamic::from(request_id as i64));
        }
        if let Some(offset) = self.offset {
            map.insert("offset".into(), Dynamic::from(offset as i64));
        }
        if let Some(status) = self.status {
            map.insert("status".into(), Dynamic::from(status as i64));
        }
        if let Some(mtu) = self.mtu {
            map.insert("mtu".into(), Dynamic::from(mtu as i64));
        }
        if let Some(response_needed) = self.response_needed {
            map.insert("response_needed".into(), Dynamic::from(response_needed));
        }
        // Payload keys last: a host-pushed event owns its own vocabulary.
        // A payload that isn't a JSON object (or doesn't parse) is dropped
        // rather than corrupting the event's shape.
        if let Some(extra) = self.extra
            && let Ok(value) = serde_json::from_str::<serde_json::Value>(&extra)
            && let Ok(dynamic) = rhai::serde::to_dynamic(value)
            && let Some(payload) = dynamic.try_cast::<Map>()
        {
            map.extend(payload);
        }
        map
    }
}

/// The per-engine session event queue. `Arc<Mutex<_>>` (not `Rc<RefCell<_>>`)
/// because the producing side lives inside the `Send`-required
/// `BluetoothGattServerCallback`; the consuming side is the (single-threaded)
/// Rhai engine.
pub type SessionEvents = Arc<Mutex<VecDeque<ScriptEvent>>>;

/// `BluetoothGattServerCallback` impl that records events into the session
/// queue instead of dispatching to closures (see module doc for why).
struct QueueCallback {
    server_name: String,
    events: SessionEvents,
}

impl QueueCallback {
    fn push(&self, event: ScriptEvent) {
        self.events.lock().unwrap().push_back(event);
    }
}

impl BluetoothGattServerCallback for QueueCallback {
    fn on_connection_state_change(&mut self, device: &BluetoothDevice, status: i32, state: i32) {
        // Scripts match connection edges by name ("connected"/"disconnected",
        // per the spec's `wait_for "connected"` example) rather than by
        // Android's numeric STATE_* pair.
        let kind = if state == crate::android::gatt_server::BluetoothProfile::STATE_CONNECTED {
            "connected"
        } else {
            "disconnected"
        };
        let mut event = ScriptEvent::new(kind, &self.server_name);
        event.peer = Some(device.clone());
        event.status = Some(status);
        self.push(event);
    }

    fn on_service_added(&mut self, status: i32, service: &BluetoothGattService) {
        let mut event = ScriptEvent::new("service_added", &self.server_name);
        event.uuid = Some(service.uuid);
        event.status = Some(status);
        self.push(event);
    }

    fn on_characteristic_read_request(
        &mut self,
        device: &BluetoothDevice,
        request_id: i32,
        offset: i32,
        characteristic: &BluetoothGattCharacteristic,
    ) {
        let mut event = ScriptEvent::new("characteristic_read", &self.server_name);
        event.peer = Some(device.clone());
        event.request_id = Some(request_id);
        event.offset = Some(offset);
        event.uuid = Some(characteristic.uuid);
        self.push(event);
    }

    fn on_characteristic_write_request(
        &mut self,
        device: &BluetoothDevice,
        request_id: i32,
        characteristic: &BluetoothGattCharacteristic,
        _prepared_write: bool,
        response_needed: bool,
        offset: i32,
        value: &[u8],
    ) {
        let mut event = ScriptEvent::new("characteristic_write", &self.server_name);
        event.peer = Some(device.clone());
        event.request_id = Some(request_id);
        event.offset = Some(offset);
        event.uuid = Some(characteristic.uuid);
        event.value = Some(value.to_vec());
        event.response_needed = Some(response_needed);
        self.push(event);
    }

    fn on_descriptor_read_request(
        &mut self,
        device: &BluetoothDevice,
        request_id: i32,
        offset: i32,
        descriptor: &BluetoothGattDescriptor,
    ) {
        let mut event = ScriptEvent::new("descriptor_read", &self.server_name);
        event.peer = Some(device.clone());
        event.request_id = Some(request_id);
        event.offset = Some(offset);
        event.uuid = Some(descriptor.uuid);
        self.push(event);
    }

    fn on_descriptor_write_request(
        &mut self,
        device: &BluetoothDevice,
        request_id: i32,
        descriptor: &BluetoothGattDescriptor,
        _prepared_write: bool,
        response_needed: bool,
        offset: i32,
        value: &[u8],
    ) {
        let mut event = ScriptEvent::new("descriptor_write", &self.server_name);
        event.peer = Some(device.clone());
        event.request_id = Some(request_id);
        event.offset = Some(offset);
        event.uuid = Some(descriptor.uuid);
        event.value = Some(value.to_vec());
        event.response_needed = Some(response_needed);
        self.push(event);
    }

    fn on_notification_sent(&mut self, device: &BluetoothDevice, status: i32) {
        let mut event = ScriptEvent::new("notification_sent", &self.server_name);
        event.peer = Some(device.clone());
        event.status = Some(status);
        self.push(event);
    }

    fn on_mtu_changed(&mut self, device: &BluetoothDevice, mtu: i32) {
        let mut event = ScriptEvent::new("mtu_changed", &self.server_name);
        event.peer = Some(device.clone());
        event.mtu = Some(mtu);
        self.push(event);
    }
}

/// Script-side handle to a `BluetoothGattServer`. Rhai requires registered
/// types to be `Clone`, and `BluetoothGattServer` isn't — so the handle is
/// an `Rc<RefCell<_>>` (plain `Rc`: this rhai build is single-threaded, no
/// `sync` feature) that clones shallowly, so every copy a script (or the
/// host test) holds is the same live server.
#[derive(Clone)]
pub struct ScriptGattServer {
    inner: Rc<RefCell<BluetoothGattServer>>,
    events: SessionEvents,
    /// Messages the script has emitted for the host/page, as JSON strings.
    /// The return path of the event channel: `server.emit(kind, payload)`.
    emitted: Rc<RefCell<Vec<String>>>,
}

impl ScriptGattServer {
    fn create(name: &str, address: Address, events: SessionEvents) -> Self {
        let device = VirtualDevice::new(name, address, AddressType::Random);
        let callback = QueueCallback {
            server_name: name.to_string(),
            events: events.clone(),
        };
        Self {
            inner: Rc::new(RefCell::new(BluetoothGattServer::new(
                device,
                Box::new(callback),
            ))),
            events,
            emitted: Rc::new(RefCell::new(Vec::new())),
        }
    }

    /// Host-side access to the wrapped server (and through it the real
    /// `VirtualDevice`), so Rust tests can drive real ATT traffic at a
    /// server a script built — the end-to-end loop the spec's testing
    /// section (§06) is about.
    pub fn with_server<R>(&self, f: impl FnOnce(&mut BluetoothGattServer) -> R) -> R {
        f(&mut self.inner.borrow_mut())
    }

    fn name(&self) -> String {
        self.inner.borrow().device.name.clone()
    }

    /// Queues an event for this server's script, as if the stack had raised
    /// it. Delivered on the next dispatch (see `ScriptedPeripheral`).
    pub fn push_event(&self, kind: &str, payload_json: String) {
        let event = ScriptEvent::custom(kind, &self.name(), payload_json);
        self.events.lock().unwrap().push_back(event);
    }

    /// Records a message the script emitted for the host.
    pub fn push_emitted(&self, message: String) {
        const MAX_EMITTED: usize = 512;
        let mut queue = self.emitted.borrow_mut();
        if queue.len() >= MAX_EMITTED {
            queue.remove(0);
        }
        queue.push(message);
    }

    /// Drains what the script has emitted for the host, oldest first.
    pub fn take_emitted(&self) -> Vec<String> {
        std::mem::take(&mut *self.emitted.borrow_mut())
    }

    /// Drains only this server's events, leaving other servers' events
    /// queued (one session queue serves all servers; see `SessionEvents`).
    pub fn take_own_events(&self) -> Array {
        let name = self.name();
        let mut queue = self.events.lock().unwrap();
        let mut own = Array::new();
        queue.retain(|event| {
            if event.server == name {
                own.push(Dynamic::from_map(event.clone().into_map()));
                false
            } else {
                true
            }
        });
        own
    }
}

/// Crate-visible so host-side extension registrars (the web runtime's
/// `update_value` in `transport::wasm_ws`) raise script errors the same way.
pub(crate) fn runtime_error(message: impl Into<String>) -> Box<EvalAltResult> {
    Box::new(EvalAltResult::ErrorRuntime(
        Dynamic::from(message.into()),
        Position::NONE,
    ))
}

/// Coerces a script-supplied value (blob, int array, or `()` — the spec's
/// `send_response(..., ())` example passes unit for "no payload") into
/// bytes. Crate-visible for the same reason as [`runtime_error`].
pub(crate) fn dynamic_to_bytes(value: Dynamic) -> Result<Vec<u8>, Box<EvalAltResult>> {
    if value.is_unit() {
        return Ok(Vec::new());
    }
    if value.is_blob() {
        return Ok(value.cast::<Blob>());
    }
    if value.is_array() {
        return value
            .cast::<Array>()
            .into_iter()
            .map(|item| {
                let n = item
                    .as_int()
                    .map_err(|t| runtime_error(format!("expected byte (int), got {t}")))?;
                u8::try_from(n).map_err(|_| runtime_error(format!("byte out of range: {n}")))
            })
            .collect();
    }
    Err(runtime_error(format!(
        "expected blob, array of bytes, or (), got {}",
        value.type_name()
    )))
}

/// Registers the `android::*` module, the GATT builder types and their
/// methods, `assert`, `take_events`, and the `wait_for` custom syntax.
pub fn register(engine: &mut Engine, events: SessionEvents) {
    register_types(engine);
    register_android_module(engine, events.clone());
    register_assert(engine);
    register_take_events(engine, events.clone());
    register_wait_for(engine, events);
}

fn register_types(engine: &mut Engine) {
    engine
        .register_type_with_name::<Uuid>("Uuid")
        .register_fn("to_string", |uuid: &mut Uuid| uuid.to_string())
        .register_fn("to_debug", |uuid: &mut Uuid| format!("{uuid:?}"))
        .register_fn("==", |a: Uuid, b: Uuid| a == b)
        .register_fn("!=", |a: Uuid, b: Uuid| a != b);

    engine
        .register_type_with_name::<BluetoothDevice>("BluetoothDevice")
        .register_get("address", |device: &mut BluetoothDevice| {
            device.get_address()
        })
        .register_fn("to_string", |device: &mut BluetoothDevice| {
            device.to_string()
        })
        .register_fn("==", |a: BluetoothDevice, b: BluetoothDevice| a == b);

    engine
        .register_type_with_name::<BluetoothGattDescriptor>("BluetoothGattDescriptor")
        .register_get("uuid", |descriptor: &mut BluetoothGattDescriptor| {
            descriptor.uuid
        })
        .register_get(
            "value",
            |descriptor: &mut BluetoothGattDescriptor| -> Blob { descriptor.value.clone() },
        )
        .register_fn(
            "set_value",
            |descriptor: &mut BluetoothGattDescriptor,
             value: Dynamic|
             -> Result<bool, Box<EvalAltResult>> {
                Ok(descriptor.set_value(dynamic_to_bytes(value)?))
            },
        )
        .register_fn(
            "get_value",
            |descriptor: &mut BluetoothGattDescriptor| -> Blob { descriptor.value.clone() },
        );

    engine
        .register_type_with_name::<BluetoothGattCharacteristic>("BluetoothGattCharacteristic")
        .register_get(
            "uuid",
            |characteristic: &mut BluetoothGattCharacteristic| characteristic.uuid,
        )
        .register_get(
            "properties",
            |characteristic: &mut BluetoothGattCharacteristic| characteristic.properties as i64,
        )
        .register_get(
            "permissions",
            |characteristic: &mut BluetoothGattCharacteristic| characteristic.permissions as i64,
        )
        .register_get(
            "value",
            |characteristic: &mut BluetoothGattCharacteristic| -> Blob {
                characteristic.value.clone()
            },
        )
        .register_fn(
            "set_value",
            |characteristic: &mut BluetoothGattCharacteristic,
             value: Dynamic|
             -> Result<bool, Box<EvalAltResult>> {
                Ok(characteristic.set_value(dynamic_to_bytes(value)?))
            },
        )
        .register_fn(
            "get_value",
            |characteristic: &mut BluetoothGattCharacteristic| -> Blob {
                characteristic.value.clone()
            },
        )
        .register_fn(
            "add_descriptor",
            |characteristic: &mut BluetoothGattCharacteristic,
             descriptor: BluetoothGattDescriptor| {
                characteristic.add_descriptor(descriptor)
            },
        );

    engine
        .register_type_with_name::<BluetoothGattService>("BluetoothGattService")
        .register_get("uuid", |service: &mut BluetoothGattService| service.uuid)
        .register_get("service_type", |service: &mut BluetoothGattService| {
            service.service_type as i64
        })
        .register_get("characteristics", |service: &mut BluetoothGattService| {
            service
                .characteristics
                .iter()
                .cloned()
                .map(Dynamic::from)
                .collect::<Array>()
        })
        .register_fn(
            "add_characteristic",
            |service: &mut BluetoothGattService, characteristic: BluetoothGattCharacteristic| {
                service.add_characteristic(characteristic)
            },
        )
        .register_fn(
            "get_characteristic",
            |service: &mut BluetoothGattService, uuid: Uuid| -> Dynamic {
                service
                    .characteristics
                    .iter()
                    .find(|c| c.uuid == uuid)
                    .cloned()
                    .map_or(Dynamic::UNIT, Dynamic::from)
            },
        );

    engine
        .register_type_with_name::<ScriptGattServer>("BluetoothGattServer")
        .register_get("name", |server: &mut ScriptGattServer| server.name())
        .register_fn(
            "add_service",
            |server: &mut ScriptGattServer, service: BluetoothGattService| {
                server.inner.borrow_mut().add_service(service)
            },
        )
        .register_fn(
            "get_service",
            |server: &mut ScriptGattServer, uuid: Uuid| -> Dynamic {
                server
                    .inner
                    .borrow()
                    .get_service(uuid)
                    .map_or(Dynamic::UNIT, Dynamic::from)
            },
        )
        .register_fn(
            "notify_characteristic_changed",
            |server: &mut ScriptGattServer,
             device: BluetoothDevice,
             characteristic: BluetoothGattCharacteristic,
             confirm: bool| {
                server.inner.borrow_mut().notify_characteristic_changed(
                    &device,
                    &characteristic,
                    confirm,
                ) as i64
            },
        )
        .register_fn(
            // 5-arg convenience matching the spec's example shape
            // (`notify_characteristic_changed(peer, chr, false, [bpm])`):
            // sets the value, then notifies.
            "notify_characteristic_changed",
            |server: &mut ScriptGattServer,
             device: BluetoothDevice,
             mut characteristic: BluetoothGattCharacteristic,
             confirm: bool,
             value: Dynamic|
             -> Result<i64, Box<EvalAltResult>> {
                characteristic.set_value(dynamic_to_bytes(value)?);
                Ok(server.inner.borrow_mut().notify_characteristic_changed(
                    &device,
                    &characteristic,
                    confirm,
                ) as i64)
            },
        )
        .register_fn(
            "send_response",
            |server: &mut ScriptGattServer,
             device: BluetoothDevice,
             request_id: i64,
             status: i64,
             offset: i64,
             value: Dynamic|
             -> Result<(), Box<EvalAltResult>> {
                server.inner.borrow_mut().send_response(
                    &device,
                    request_id as i32,
                    status as i32,
                    offset as i32,
                    &dynamic_to_bytes(value)?,
                );
                Ok(())
            },
        )
        .register_fn(
            // Stages 16-bit Service Data for the device's advertisement (the
            // beacon idiom: Fast Pair, Eddystone, Quick Share nudges). Folded
            // into the on-air payload at bring-up.
            "advertise_service_data",
            |server: &mut ScriptGattServer,
             uuid16: i64,
             data: Dynamic|
             -> Result<(), Box<EvalAltResult>> {
                let uuid16 = u16::try_from(uuid16).map_err(|_| {
                    runtime_error(format!("advertise_service_data: not a 16-bit uuid: {uuid16}"))
                })?;
                let bytes = dynamic_to_bytes(data)?;
                server.with_server(|s| {
                    s.device
                        .advertising_data
                        .get_or_insert_with(AdvertisingData::new)
                        .service_data_16
                        .push((uuid16, bytes.clone()));
                });
                Ok(())
            },
        )
        .register_fn(
            // Stages manufacturer-specific data (company ID + payload) for
            // the device's advertisement.
            "advertise_manufacturer_data",
            |server: &mut ScriptGattServer,
             company_id: i64,
             data: Dynamic|
             -> Result<(), Box<EvalAltResult>> {
                let company_id = u16::try_from(company_id).map_err(|_| {
                    runtime_error(format!(
                        "advertise_manufacturer_data: not a 16-bit company id: {company_id}"
                    ))
                })?;
                let bytes = dynamic_to_bytes(data)?;
                server.with_server(|s| {
                    let staged = s
                        .device
                        .advertising_data
                        .get_or_insert_with(AdvertisingData::new);
                    *staged = staged.clone().with_manufacturer_data(company_id, &bytes);
                });
                Ok(())
            },
        )
        .register_fn(
            // Marks the device a pure beacon: it advertises non-connectable
            // (ADV_NONCONN_IND), so scanners show it as a broadcast-only
            // beacon and never offer a connect — what real iBeacon /
            // Eddystone / Quick Share nudge advertisers do.
            "advertise_connectable",
            |server: &mut ScriptGattServer, connectable: bool| {
                server.with_server(|s| s.device.connectable = connectable);
            },
        )
        .register_fn("take_events", |server: &mut ScriptGattServer| {
            server.take_own_events()
        })
        .register_fn("close", |server: &mut ScriptGattServer| {
            server.inner.borrow_mut().close();
        });
}

fn register_android_module(engine: &mut Engine, events: SessionEvents) {
    let mut android = Module::new();
    super::constants::set_android_constants(&mut android);

    // Constructors are the type name as a module function —
    // `android::BluetoothGattServer("HRM")`. The spec's spelling
    // (`android::BluetoothGattServer::new(...)`) is unavailable because
    // `new` is a reserved keyword in Rhai; the type-name-as-constructor
    // idiom is Rhai's own convention for exactly this case.
    //
    // Addresses are allocated deterministically per engine session (base +
    // counter) so identical scripts produce identical devices — determinism
    // is what makes generate-check-repair loops converge (spec §06).
    let next_address = Cell::new(1u16);
    android.set_native_fn(
        "BluetoothGattServer",
        move |name: &str| -> Result<ScriptGattServer, Box<EvalAltResult>> {
            let n = next_address.get();
            next_address.set(n.wrapping_add(1));
            let [hi, lo] = n.to_be_bytes();
            let address = Address::from_be_bytes([0xF0, 0xDE, 0xC0, 0x00, hi, lo]);
            Ok(ScriptGattServer::create(name, address, events.clone()))
        },
    );

    android.set_native_fn(
        "BluetoothGattService",
        |uuid: Uuid, service_type: i64| -> Result<BluetoothGattService, Box<EvalAltResult>> {
            Ok(BluetoothGattService::new(uuid, service_type as i32))
        },
    );

    android.set_native_fn(
        "BluetoothGattCharacteristic",
        |uuid: Uuid,
         properties: i64,
         permissions: i64|
         -> Result<BluetoothGattCharacteristic, Box<EvalAltResult>> {
            Ok(BluetoothGattCharacteristic::new(
                uuid,
                properties as i32,
                permissions as i32,
            ))
        },
    );

    android.set_native_fn(
        "BluetoothGattDescriptor",
        |uuid: Uuid, permissions: i64| -> Result<BluetoothGattDescriptor, Box<EvalAltResult>> {
            Ok(BluetoothGattDescriptor::new(uuid, permissions as i32))
        },
    );

    engine.register_static_module("android", android.into());
}

fn register_assert(engine: &mut Engine) {
    // Spec §06: a host function that throws a Rhai runtime error on false —
    // a test fails loudly and specifically, not silently.
    engine.register_fn(
        "assert",
        |condition: bool, message: &str| -> Result<(), Box<EvalAltResult>> {
            if condition {
                Ok(())
            } else {
                Err(runtime_error(format!("assertion failed: {message}")))
            }
        },
    );
}

fn register_take_events(engine: &mut Engine, events: SessionEvents) {
    // Session-wide drain (all servers' events, in arrival order); the
    // per-server filtered variant is `server.take_events()`.
    engine.register_fn("take_events", move || -> Array {
        events
            .lock()
            .unwrap()
            .drain(..)
            .map(|event| Dynamic::from_map(event.into_map()))
            .collect()
    });
}

fn register_wait_for(engine: &mut Engine, events: SessionEvents) {
    // Spec §05: `wait_for` is custom syntax, not a function, so the block is
    // captured at parse time. With the event-queue model its semantics are
    // synchronous: drain already-queued events until one matches, bind it as
    // `event`, run the block. No match left in the queue is an error — under
    // synchronous ATT dispatch an event that hasn't arrived by now never
    // will, so failing beats silently skipping the block.
    engine
        .register_custom_syntax(
            ["wait_for", "$expr$", "$block$"],
            true,
            move |context, inputs| {
                let name = context
                    .eval_expression_tree(&inputs[0])?
                    .into_immutable_string()
                    .map_err(|t| {
                        runtime_error(format!("wait_for: expected event name string, got {t}"))
                    })?;

                let matched = {
                    let mut queue = events.lock().unwrap();
                    loop {
                        match queue.pop_front() {
                            Some(event) if event.kind == *name => break Some(event),
                            // Earlier non-matching events are consumed:
                            // wait_for models "everything up to this event
                            // has happened", mirroring how a blocking wait
                            // would observe the stream.
                            Some(_) => continue,
                            None => break None,
                        }
                    }
                };

                let Some(event) = matched else {
                    return Err(runtime_error(format!(
                        "wait_for: no '{name}' event pending"
                    )));
                };

                let scope_len = context.scope().len();
                context.scope_mut().push("event", event.into_map());
                let result = context.eval_expression_tree(&inputs[1]);
                context.scope_mut().rewind(scope_len);
                result
            },
        )
        .expect("wait_for custom syntax is valid");
}
