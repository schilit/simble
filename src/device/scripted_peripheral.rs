// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! The scripted GATT peripheral: a Rhai script hosted as a live LE device.
//!
//! `ScriptedPeripheral` compiles a device script, stands up its GATT server
//! over an in-process [`LeHost`], and serves it on an [`HciChannel`] — the
//! natively-testable engine the browser bindings in
//! [`crate::transport::wasm_ws`] wrap. It carries the default device script and
//! the REPL/status plumbing the pages and the scene engine drive.

use std::collections::HashMap;

use crate::android::gatt_service::BluetoothGattCharacteristic;
use crate::device::LeHost;
use crate::scripting::test_script::{find_value_handle, register_web_extensions};
use crate::scripting::{ScriptBroadcastSource, ScriptGattServer, new_engine};
use crate::transport::hci_adapter::HciChannel;
use crate::transport::scan_report::{hex, send_acl};
use crate::types::{Address, AddressType, SimbleError, Uuid};
use rhai::{AST, Array, CallFnOptions, Dynamic, Engine, Map, Scope};
use serde::Serialize;

/// The default script served by the scripted-device page — a single source of
/// truth shared by the page (via `default_heart_rate_script`) and the native
/// unit tests below, so what ships is what's tested. (The file keeps its
/// legacy `heart_rate.rhai` name but now builds a thermometer; the export name
/// is likewise kept for the page's stable wasm import.)
pub const DEFAULT_HEART_RATE_SCRIPT: &str = include_str!("../../catalog/devices/heart_rate.rhai");

/// What a client subscribed to on a characteristic's CCCD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CccdSubscription {
    /// Nothing enabled.
    None,
    /// Notifications (unacknowledged).
    Notify,
    /// Indications (confirmed by the peer).
    Indicate,
}

/// A notify-capable characteristic the host glue watches for value changes.
#[derive(Debug, Clone)]
pub(crate) struct WatchedCharacteristic {
    server_index: usize,
    value_handle: u16,
    pub(crate) cccd_handle: Option<u16>,
}

/// Everything the peripheral page reports back to its JS, per tick.
#[derive(Serialize)]
struct PeripheralStatus {
    name: String,
    address: String,
    connected: bool,
    peer: Option<String>,
    tick_defined: bool,
    last_error: Option<String>,
    services: Vec<ServiceStatus>,
}

#[derive(Serialize)]
struct ServiceStatus {
    uuid: String,
    characteristics: Vec<CharacteristicStatus>,
}

#[derive(Serialize)]
struct CharacteristicStatus {
    uuid: String,
    /// The GATT property bitmask (READ/WRITE/NOTIFY/INDICATE/…), so the page
    /// can render the generic R/W/N/I chips for any script-built device, not
    /// just the ones it recognizes.
    properties: i64,
    value: String,
    subscribed: bool,
}

/// A Simble peripheral whose entire behavior comes from a Rhai script — the
/// web demos' scripted-device engine, host-side. The script builds real
/// `android::BluetoothGattServer`s over real `VirtualDevice`s; this glue
/// connects the first one to an [`HciChannel`]:
///
/// - **Advertising is host glue for now**: the `android::*` bindings have no
///   `BluetoothLeAdvertiser` yet, so [`Self::queue_start`] issues the HCI
///   advertising sequence itself, carrying the script device's name and
///   16-bit service UUIDs (re-derived on every re-Run, so a renamed device
///   advertises its new name).
/// - **Ticking**: if the script defines `fn tick(server, t)`, it's called on
///   every host tick with seconds-since-start — behavior lives in the
///   script, not the page.
/// - **Notifications**: any notify-capable characteristic whose database
///   value changes (script `update_value`, or a peer write) is notified to
///   a subscribed central automatically.
pub struct ScriptedPeripheral {
    engine: Engine,
    ast: AST,
    scope: Scope<'static>,
    servers: Vec<ScriptGattServer>,
    /// Auracast broadcast sources the script built
    /// (`android::BluetoothLeBroadcast`).
    ///
    /// A source is not a GATT server and shares nothing with one: it drives an
    /// extended advertising set, a periodic train and a BIG, all of which the
    /// controller tracks separately from the legacy advertising this
    /// peripheral's own bring-up uses. So the two coexist on one device — as
    /// they do on a real Auracast TV, which is a connectable GATT peripheral
    /// *and* a broadcast source.
    sources: Vec<ScriptBroadcastSource>,
    /// The LE host layer: HCI event dispatch, ATT/SMP replies, ACL framing.
    host: LeHost,
    connection: Option<(u16, Address)>,
    tick_defined: bool,
    /// Whether the script defines `fn on_event(server, event)` — the
    /// handler that receives ATT events and host-pushed UI events.
    on_event_defined: bool,
    /// Per-device state bound as `this` for `tick`/`on_event`.
    state: Dynamic,
    pub(crate) watched: Vec<WatchedCharacteristic>,
    last_values: HashMap<u16, Vec<u8>>,
    last_error: Option<String>,
}

/// The result of evaluating one REPL line in a `ScriptedPeripheral` session
/// (the API Explorer emits exactly one Rhai statement per Execute): the
/// statement's return value rendered for display, and any queue events it
/// produced, already formatted for the Explorer's log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplOutcome {
    /// The statement's return value, rendered for display.
    pub value: String,
    /// Queue events produced by the statement, formatted for the log.
    pub events: Vec<String>,
}

/// The JSON shape returned to the Explorer page per Execute — success carries
/// the rendered return value and events; failure carries the Rhai error.
#[derive(Serialize)]
struct ReplResult {
    ok: bool,
    value: String,
    error: Option<String>,
    events: Vec<String>,
}

/// Renders a REPL line's return value for the Explorer log. Unit (the result
/// of a `let` binding or a void call) shows as `()`; a `Uuid` shows in its
/// canonical string form; everything else uses Rhai's own `Display`.
fn display_value(value: &Dynamic) -> String {
    if value.is_unit() {
        return "()".to_string();
    }
    if let Some(uuid) = value.clone().try_cast::<Uuid>() {
        return uuid.to_string();
    }
    let rendered = value.to_string();
    if rendered.is_empty() {
        "()".to_string()
    } else {
        rendered
    }
}

/// Formats one queued `ScriptEvent` map (as seen by scripts) into a short log
/// line for the Explorer, e.g. `service_added uuid=180D status=0`.
fn format_event(map: &Map) -> String {
    let mut parts = vec![
        map.get("event")
            .map(|v| v.clone().into_string().unwrap_or_default())
            .unwrap_or_default(),
    ];
    if let Some(uuid) = map.get("uuid").and_then(|v| v.clone().try_cast::<Uuid>()) {
        parts.push(format!("uuid={uuid}"));
    }
    if let Some(status) = map.get("status").and_then(|v| v.as_int().ok()) {
        parts.push(format!("status={status}"));
    }
    parts.join(" ")
}

impl ScriptedPeripheral {
    /// Compiles and runs `script` on a fresh engine, collecting every
    /// `android::BluetoothGattServer` the script left in a top-level
    /// variable. Compile and runtime errors come back as display strings
    /// ready for the page's error pane.
    pub fn run_script(script: &str) -> Result<Self, String> {
        let mut engine = new_engine();
        register_web_extensions(&mut engine);
        let ast = engine.compile(script).map_err(|e| e.to_string())?;
        let mut scope = Scope::new();
        engine
            .run_ast_with_scope(&mut scope, &ast)
            .map_err(|e| e.to_string())?;

        let servers: Vec<ScriptGattServer> = scope
            .iter()
            .filter_map(|(_, _, value)| value.try_cast::<ScriptGattServer>())
            .collect();
        if servers.is_empty() {
            return Err("script must create an android::BluetoothGattServer \
                 and keep it in a top-level variable"
                .to_string());
        }

        let sources: Vec<ScriptBroadcastSource> = scope
            .iter()
            .filter_map(|(_, _, value)| value.try_cast::<ScriptBroadcastSource>())
            .collect();

        let tick_defined = ast
            .iter_functions()
            .any(|f| f.name == "tick" && f.params.len() == 2);
        let on_event_defined = ast
            .iter_functions()
            .any(|f| f.name == "on_event" && f.params.len() == 2);

        let mut peripheral = Self {
            engine,
            ast,
            scope,
            servers,
            sources,
            host: LeHost::new(),
            connection: None,
            tick_defined,
            on_event_defined,
            // Persistent per-device state, bound as `this` for `tick` and
            // `on_event` (Rhai's documented event-handler pattern): script
            // functions are pure and cannot see the calling scope, so this
            // map is how a device remembers anything between calls.
            state: Dynamic::from_map(rhai::Map::new()),
            watched: Vec::new(),
            last_values: HashMap::new(),
            last_error: None,
        };
        peripheral.rebuild_watch_list();
        Ok(peripheral)
    }

    /// Creates an empty REPL session for the API Explorer: a fresh engine
    /// (with the web `update_value` extension), an empty persistent scope, and
    /// no servers yet. Lines are fed one at a time with [`Self::eval_line`];
    /// once a line binds an `android::BluetoothGattServer`, the session hosts
    /// it exactly like a scripted device (advertising, connections,
    /// notifications), so the Explorer's clicks build a real, hostable device.
    pub fn new_session() -> Self {
        let mut engine = new_engine();
        register_web_extensions(&mut engine);
        let ast = engine.compile("").expect("empty script compiles");
        Self {
            engine,
            ast,
            scope: Scope::new(),
            servers: Vec::new(),
            sources: Vec::new(),
            host: LeHost::new(),
            connection: None,
            tick_defined: false,
            on_event_defined: false,
            state: Dynamic::from_map(rhai::Map::new()),
            watched: Vec::new(),
            last_values: HashMap::new(),
            last_error: None,
        }
    }

    /// Evaluates one Rhai statement in the persistent session scope (top-level
    /// `let` bindings persist across calls, so `let svc1 = ...` stays usable by
    /// later Executes). Re-collects the servers the scope now holds and
    /// rebuilds the notify watch-list, so a service or characteristic added by
    /// this line is immediately hosted. Returns the statement's rendered return
    /// value and the events it produced, or the Rhai error as a string.
    pub fn eval_line(&mut self, line: &str) -> Result<ReplOutcome, String> {
        let value = self
            .engine
            .eval_with_scope::<Dynamic>(&mut self.scope, line)
            .map_err(|e| e.to_string())?;
        self.servers = self
            .scope
            .iter()
            .filter_map(|(_, _, value)| value.try_cast::<ScriptGattServer>())
            .collect();
        self.rebuild_watch_list();
        let events = self.drain_events_display();
        Ok(ReplOutcome {
            value: display_value(&value),
            events,
        })
    }

    /// [`Self::eval_line`] rendered as the JSON the Explorer page consumes.
    pub fn eval_line_json(&mut self, line: &str) -> String {
        let result = match self.eval_line(line) {
            Ok(outcome) => ReplResult {
                ok: true,
                value: outcome.value,
                error: None,
                events: outcome.events,
            },
            Err(error) => ReplResult {
                ok: false,
                value: String::new(),
                error: Some(error),
                events: Vec::new(),
            },
        };
        serde_json::to_string(&result)
            .unwrap_or_else(|e| format!("{{\"ok\":false,\"error\":\"{e}\"}}"))
    }

    /// Drains the session's queued events and formats them for the log.
    fn drain_events_display(&mut self) -> Vec<String> {
        match self.engine.eval::<Array>("take_events()") {
            Ok(events) => events
                .into_iter()
                .filter_map(|event| event.try_cast::<Map>())
                .map(|map| format_event(&map))
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Whether the session/script has produced at least one server to host.
    pub fn has_server(&self) -> bool {
        !self.servers.is_empty()
    }

    /// A signature of what the primary server would advertise (name + sorted
    /// 16-bit service UUIDs). The Explorer re-issues advertising when this
    /// changes, so a device gains its new services on the air as it's built.
    pub fn adv_signature(&self) -> String {
        if self.servers.is_empty() {
            return String::new();
        }
        let mut uuids = self.primary_service_uuids_16();
        uuids.sort_unstable();
        format!("{}|{uuids:?}", self.device_name())
    }

    pub(crate) fn primary(&self) -> &ScriptGattServer {
        &self.servers[0]
    }

    /// The first `android::BluetoothGattServer` the script built, or `None` if
    /// it built none. Paired with [`ScriptGattServer::with_server`] this lets a
    /// Rust test drive real ATT traffic at a device a script composed — the
    /// only way to check that a profile binding wired up a live state machine
    /// rather than an inert set of attributes.
    pub fn primary_server(&self) -> Option<&ScriptGattServer> {
        self.servers.first()
    }

    /// The scripted device's name (also what the page shows in its header).
    pub fn device_name(&self) -> String {
        self.primary().with_server(|s| s.device.name.clone())
    }

    /// Stamps the device's on-air identity. The script engine allocates a
    /// per-session placeholder address, but SMP pairing computes with
    /// `device.address`/`address_type` — so the scene must overwrite them
    /// with the address it actually advertises (public, per the advertising
    /// parameters in [`Self::queue_start`]), or pairing against a real
    /// Narrows the `LE_Event_Mask` this peripheral's bring-up asks its
    /// controller for — see
    /// [`LeHost::set_le_event_mask`](crate::device::host::LeHost::set_le_event_mask).
    ///
    /// Only a real controller cares. Simble's own controller and rootcanal
    /// accept any mask, so a dongle-backed scene is the only caller.
    pub fn set_le_event_mask(&mut self, mask: [u8; 8]) {
        self.host.set_le_event_mask(mask);
    }

    /// Stamps the device's on-air identity. The script engine allocates a
    /// per-session placeholder address, but SMP pairing computes with
    /// `device.address`/`address_type` — so the scene must overwrite them
    /// with the address it actually advertises (public, per the advertising
    /// parameters in [`Self::queue_start`]), or pairing against a real
    /// stack fails its confirm/DHKey check.
    pub fn set_identity(&mut self, address: Address) {
        self.primary().with_server(|s| {
            s.device.address = address;
            s.device.address_type = AddressType::Public;
            // Scenes have a real controller, so SMP key distribution waits
            // for the Encryption Change event as the spec requires.
            s.device.defer_key_distribution = true;
        });
        // A broadcast source is addressed by the same identity: its
        // announcement is what a receiver filters on, and the metadata an
        // Assistant hands to a Scan Delegator names this address.
        for source in &self.sources {
            source.set_address(address);
        }
    }

    /// Drains the isochronous SDUs this device has received (media plane).
    pub fn take_audio(&mut self) -> Vec<Vec<u8>> {
        self.primary().with_server(|s| s.device.take_audio())
    }

    /// Records a non-fatal runtime problem for the page's error pane.
    pub fn record_error(&mut self, message: String) {
        self.last_error = Some(message);
    }

    /// Host-side write of a characteristic's value by UUID string — the same
    /// live-database path as the script's `update_value`, exposed to the page
    /// so UI (the lightbulb's colour picker) can drive a value directly. A
    /// subscribed central is notified of the change on the next tick. `uuid` is
    /// the string form (`"FFE9"` or a full 128-bit UUID).
    ///
    /// When the characteristic has an `AttributeHandler` — i.e. it is a control
    /// point owned by a Rust profile — the write is routed through
    /// `GattDatabase::write` so the state machine runs, exactly as it would for
    /// a connected peer's ATT write. `set_value` would store the opcode and
    /// execute nothing, so the Audio page's volume buttons wrote a command that
    /// was never applied. Everything without a handler keeps the direct path,
    /// where bypassing dispatch is the point.
    pub fn set_characteristic_value(&mut self, uuid: &str, bytes: &[u8]) -> Result<(), String> {
        let uuid = uuid.parse::<Uuid>().map_err(|e| e.to_string())?;
        let handle = find_value_handle(self.primary(), uuid)
            .ok_or_else(|| format!("no characteristic with UUID {uuid}"))?;
        self.primary()
            .with_server(|s| {
                if s.device.gatt_db.has_handler(handle) {
                    s.device.gatt_db.write(handle, bytes)
                } else {
                    s.device.gatt_db.set_value(handle, bytes)
                }
            })
            .map_err(|status| format!("set_value failed: ATT error {status}"))
    }

    /// Host-writes a characteristic and notifies it even if the bytes did not
    /// change.
    ///
    /// The value-diff in `flush_value_notifications` is right for a
    /// characteristic that holds *state* — a battery level that has not moved
    /// is not news. It is wrong for one that reports *change*: two identical
    /// HID mouse reports mean the pointer moved twice by the same amount, and
    /// suppressing the second stalls the pointer for anyone dragging at a
    /// steady speed. The same applies to a keystroke repeated by auto-repeat.
    pub fn notify_characteristic_value(&mut self, uuid: &str, bytes: &[u8]) -> Result<(), String> {
        self.set_characteristic_value(uuid, bytes)?;
        let uuid = uuid.parse::<Uuid>().map_err(|e| e.to_string())?;
        if let Some(handle) = find_value_handle(self.primary(), uuid) {
            // Forgetting the memo is what makes the next flush treat this
            // value as new.
            self.last_values.remove(&handle);
        }
        Ok(())
    }

    /// Queues the peripheral's full HCI bring-up: reset, event masks,
    /// advertising parameters, advertising data + scan response carrying the
    /// script device's identity, then advertising enable.
    pub fn queue_start(&self, channel: &HciChannel) -> Result<(), SimbleError> {
        let uuids = self.primary_service_uuids_16();
        let commands = self
            .primary()
            .with_server(|s| self.host.start_advertising(&s.device, &uuids))?;
        for packet in commands {
            channel.inject_host_packet(packet)?;
        }
        self.flush_broadcast_sources(channel)?;
        Ok(())
    }

    /// Sends whatever the script's broadcast sources have queued — the setup
    /// ladder `start_broadcast` began, a teardown, or an SDU.
    fn flush_broadcast_sources(&self, channel: &HciChannel) -> Result<(), SimbleError> {
        for source in &self.sources {
            for packet in source.take_outbox() {
                channel.inject_host_packet(packet)?;
            }
        }
        Ok(())
    }

    /// Turns broadcast-source state transitions into the script's
    /// `on_broadcast_*` / `on_playback_*` callbacks.
    fn dispatch_broadcast_callbacks(&mut self) {
        for source in self.sources.clone() {
            let receiver = Dynamic::from(source.clone());
            for (name, args) in source.take_callbacks() {
                if !crate::scripting::broadcast::defines(&self.ast, name, args.len() + 1) {
                    continue;
                }
                let mut all = vec![receiver.clone()];
                all.extend(args);
                let options = CallFnOptions::new()
                    .eval_ast(false)
                    .bind_this_ptr(&mut self.state);
                let result = self.engine.call_fn_with_options::<Dynamic>(
                    options,
                    &mut self.scope,
                    &self.ast,
                    name,
                    all,
                );
                match result {
                    Ok(_) => self.last_error = None,
                    Err(e) => self.last_error = Some(e.to_string()),
                }
            }
        }
    }

    fn primary_service_uuids_16(&self) -> Vec<u16> {
        self.primary().with_server(|s| {
            s.get_services()
                .iter()
                .filter_map(|service| match service.uuid {
                    Uuid::Uuid16(u) => Some(u),
                    Uuid::Uuid128(_) => None,
                })
                .collect()
        })
    }

    /// Indexes every notify/indicate-capable characteristic and its CCCD:
    /// the descriptor the script attached if present, otherwise the next
    /// CCCD attribute in the database before the following declaration.
    ///
    /// Two passes, because there are two ways a characteristic gets into a
    /// device. `getServices()` returns only what went through the Android
    /// layer — what the *script* built. A Rust profile registrar (`add_bass`,
    /// `add_ascs`, `add_pacs`) writes straight into the `GattDatabase` and
    /// never appears there, so for as long as this had only the first pass,
    /// **no profile-registered characteristic could ever notify**: BASS's
    /// Broadcast Receive State, ASCS's ASEs and control point, all
    /// mandatory-notify, all silent. The second pass walks the database
    /// itself and picks up whatever the first missed.
    fn rebuild_watch_list(&mut self) {
        self.watched.clear();
        self.last_values.clear();
        for (server_index, server) in self.servers.iter().enumerate() {
            server.with_server(|s| {
                for service in s.get_services() {
                    for characteristic in &service.characteristics {
                        let notifying = BluetoothGattCharacteristic::PROPERTY_NOTIFY
                            | BluetoothGattCharacteristic::PROPERTY_INDICATE;
                        if characteristic.properties & notifying == 0 {
                            continue;
                        }
                        let Some(value_handle) = characteristic.value_handle else {
                            continue;
                        };
                        let cccd_handle = characteristic
                            .descriptors
                            .iter()
                            .find(|d| d.uuid == Uuid::CCCD)
                            .and_then(|d| d.handle)
                            .or_else(|| find_cccd_after(&s.device.gatt_db, value_handle));
                        self.watched.push(WatchedCharacteristic {
                            server_index,
                            value_handle,
                            cccd_handle,
                        });
                    }
                }
            });
            let already: Vec<u16> = self.watched.iter().map(|w| w.value_handle).collect();
            let from_database = server.with_server(|s| notifying_in_database(&s.device.gatt_db));
            for (value_handle, cccd_handle) in from_database {
                if already.contains(&value_handle) {
                    continue;
                }
                self.watched.push(WatchedCharacteristic {
                    server_index,
                    value_handle,
                    cccd_handle,
                });
            }
        }
        for watch in &self.watched {
            if let Some(value) = self.attribute_value(watch) {
                self.last_values.insert(watch.value_handle, value);
            }
        }
    }

    fn attribute_value(&self, watch: &WatchedCharacteristic) -> Option<Vec<u8>> {
        self.servers[watch.server_index].with_server(|s| {
            s.device
                .gatt_db
                .attributes
                .get(&watch.value_handle)
                .map(|attribute| attribute.value.clone())
        })
    }

    /// What the client asked for on this characteristic's CCCD (Core Spec
    /// Vol 3, Part G, Section 3.3.3.3): bit 0 notify, bit 1 indicate. Both
    /// matter — several SIG profiles mandate Indicate, and a device that
    /// only ever notifies delivers nothing to those clients.
    pub(crate) fn cccd_subscription(&self, watch: &WatchedCharacteristic) -> CccdSubscription {
        let Some(cccd) = watch.cccd_handle else {
            return CccdSubscription::None;
        };
        let value = self.servers[watch.server_index]
            .with_server(|s| s.device.cccd_value(cccd).unwrap_or(0));
        if value & 0x0002 != 0 {
            CccdSubscription::Indicate
        } else if value & 0x0001 != 0 {
            CccdSubscription::Notify
        } else {
            CccdSubscription::None
        }
    }

    /// Routes one controller-to-host H4 packet: connection events into the
    /// scripted device's connection state, ACL data through reassembly into
    /// the real L2CAP/ATT dispatch, responses back onto the channel.
    pub fn handle_packet(
        &mut self,
        channel: &HciChannel,
        packet: &[u8],
    ) -> Result<(), SimbleError> {
        // The host layer owns HCI event dispatch, ATT/SMP responses, and the
        // ACL fragmentation; this glue only moves its output to the channel
        // and mirrors the connection for `status_json`.
        let outgoing = self
            .primary()
            .clone()
            .with_server(|s| self.host.handle_packet(&mut s.device, packet))?;
        for out in outgoing {
            channel.inject_host_packet(out)?;
        }
        // The broadcast ladder is command/event driven and shares the same
        // controller: each source sees the whole stream and answers only the
        // events it asked for.
        for source in &self.sources {
            for out in source.on_packet(packet) {
                channel.inject_host_packet(out)?;
            }
        }
        self.connection = self.host.connection();
        Ok(())
    }

    /// Delivers queued events (ATT activity, and anything the host pushed
    /// with `push_event`) to the script's `fn on_event(server, event)`.
    ///
    /// State is bound as `this` — Rhai's documented event-handler pattern —
    /// because script functions are pure and cannot see the calling scope,
    /// so a map bound this way is how a device remembers anything between
    /// calls. Handler errors land in `last_error` rather than killing the
    /// device, matching how tick errors are treated.
    fn dispatch_events(&mut self) {
        if !self.on_event_defined {
            return;
        }
        let events = self.primary().take_own_events();
        if events.is_empty() {
            return;
        }
        let server = Dynamic::from(self.primary().clone());
        let Self {
            engine,
            ast,
            scope,
            state,
            last_error,
            ..
        } = self;
        for event in events {
            let options = CallFnOptions::new().eval_ast(false).bind_this_ptr(state);
            let args = (server.clone(), event);
            match engine.call_fn_with_options::<Dynamic>(options, scope, ast, "on_event", args) {
                Ok(_) => *last_error = None,
                Err(e) => *last_error = Some(e.to_string()),
            }
        }
    }

    /// Pushes an event into the running script from outside the stack — a UI
    /// control, a test, a host simulating a condition. Delivered to
    /// `on_event` on the next tick.
    pub fn push_event(&mut self, kind: &str, payload_json: &str) {
        self.primary().push_event(kind, payload_json.to_string());
    }

    /// Drains what the script emitted for the host with `server.emit(...)`.
    pub fn take_emitted(&mut self) -> Vec<String> {
        self.primary().take_emitted()
    }

    /// One host tick: calls the script's `fn tick(server, t)` if defined
    /// (`t` = seconds since Run), then turns any changed notify-capable
    /// value into a real ATT notification for a subscribed central.
    pub fn tick(&mut self, channel: &HciChannel, t_seconds: f64) -> Result<(), SimbleError> {
        // Events first, so a write that arrived since the last tick is
        // handled before the periodic tick sees the world.
        self.dispatch_events();
        if self.tick_defined {
            let args = (Dynamic::from(self.primary().clone()), t_seconds);
            // eval_ast(false): the script body already ran in `run_script`;
            // re-evaluating it here would rebuild the device every tick.
            //
            // bind_this_ptr: `state` is documented as bound for `tick` *and*
            // `on_event`, and only `on_event` ever got it — so a peripheral's
            // `fn tick` could not remember anything between calls, while a
            // central's could. `'this' not bound` is what a script saw.
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
            match result {
                Ok(_) => self.last_error = None,
                Err(e) => self.last_error = Some(e.to_string()),
            }
        }
        // Broadcast callbacks after the script's own tick, so a `fn tick` that
        // called `start_broadcast` sees `on_broadcast_started` in the same
        // pass rather than one tick later.
        self.dispatch_broadcast_callbacks();
        self.flush_broadcast_sources(channel)?;
        self.flush_value_notifications(channel)?;
        // Ship any SDUs the script queued with send_audio (the media plane
        // is unacknowledged, so this is fire-and-forget).
        let sdus = self
            .primary()
            .with_server(|s| std::mem::take(&mut s.device.audio_tx_pending));
        for packet in sdus {
            channel.inject_host_packet(packet)?;
        }
        // The observer queue records every ATT event for scripts; nothing
        // drains it across ticks, so cap it here (scripts that want events
        // must consume them with `take_events()` inside their own tick).
        let _ = self.engine.eval::<Array>("take_events()");
        Ok(())
    }

    fn flush_value_notifications(&mut self, channel: &HciChannel) -> Result<(), SimbleError> {
        let watched = self.watched.clone();
        for watch in watched {
            let Some(current) = self.attribute_value(&watch) else {
                continue;
            };
            if self.last_values.get(&watch.value_handle) == Some(&current) {
                continue;
            }
            self.last_values.insert(watch.value_handle, current.clone());
            let Some((handle, _)) = self.connection else {
                continue;
            };
            let l2cap = match self.cccd_subscription(&watch) {
                CccdSubscription::None => continue,
                CccdSubscription::Notify => self.servers[watch.server_index].with_server(|s| {
                    Ok(s.device
                        .create_notification_for(handle, watch.value_handle, &current))
                }),
                // An indication is confirmed by the peer, so only one may be
                // outstanding; `create_indication` refuses a second and the
                // value is picked up on a later tick.
                CccdSubscription::Indicate => self.servers[watch.server_index].with_server(|s| {
                    s.device
                        .create_indication(handle, watch.value_handle, &current)
                }),
            };
            match l2cap {
                Ok(l2cap) => send_acl(channel, handle, &l2cap)?,
                Err(_) => {
                    // Indication still in flight — re-send this value once the
                    // confirmation lands rather than dropping it.
                    self.last_values.remove(&watch.value_handle);
                }
            }
        }
        Ok(())
    }

    /// The page-facing status snapshot, as JSON.
    pub fn status_json(&self) -> String {
        // An empty REPL session (no server built yet) reports an empty device
        // so the Explorer's viewer can render "nothing here yet" cleanly.
        if self.servers.is_empty() {
            let empty = PeripheralStatus {
                name: String::new(),
                address: String::new(),
                connected: false,
                peer: None,
                tick_defined: false,
                last_error: self.last_error.clone(),
                services: Vec::new(),
            };
            return serde_json::to_string(&empty)
                .unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"));
        }
        // Subscription state is resolved before the server borrow below —
        // `cccd_subscription` borrows the same server, and `with_server`
        // borrows are not reentrant.
        let subscribed_handles: Vec<u16> = self
            .watched
            .iter()
            .filter(|w| self.cccd_subscription(w) != CccdSubscription::None)
            .map(|w| w.value_handle)
            .collect();
        let services = self.primary().with_server(|s| {
            s.get_services()
                .iter()
                .map(|service| ServiceStatus {
                    uuid: service.uuid.to_string(),
                    characteristics: service
                        .characteristics
                        .iter()
                        .map(|characteristic| {
                            let value = characteristic
                                .value_handle
                                .and_then(|h| s.device.gatt_db.attributes.get(&h))
                                .map(|attribute| hex(&attribute.value))
                                .unwrap_or_default();
                            let subscribed = characteristic
                                .value_handle
                                .is_some_and(|h| subscribed_handles.contains(&h));
                            CharacteristicStatus {
                                uuid: characteristic.uuid.to_string(),
                                properties: characteristic.properties as i64,
                                value,
                                subscribed,
                            }
                        })
                        .collect(),
                })
                .collect()
        });
        // Append anything a Rust profile registrar put straight into the
        // database — see `database_only_services`.
        let services = self.primary().with_server(|s| {
            let mut services: Vec<ServiceStatus> = services;
            let known: Vec<String> = services.iter().map(|x| x.uuid.clone()).collect();
            services.extend(database_only_services(
                &s.device.gatt_db,
                &known,
                &subscribed_handles,
            ));
            services
        });
        let status = PeripheralStatus {
            name: self.device_name(),
            address: self.primary().with_server(|s| s.device.address.to_string()),
            connected: self.connection.is_some(),
            peer: self.connection.map(|(_, peer)| peer.to_string()),
            tick_defined: self.tick_defined,
            last_error: self.last_error.clone(),
            services,
        };
        serde_json::to_string(&status).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
    }
}

/// Every notify/indicate-capable characteristic in `db`, as
/// `(value_handle, cccd_handle)`, read from the Characteristic Declarations
/// themselves rather than from the Android service list.
///
/// This is what reaches profile registrars: `add_bass` and friends write into
/// the database and never touch `BluetoothGattServer::get_services`, so a
/// declaration is the only record that a Broadcast Receive State exists at
/// all. The declaration's value is `[properties, value_handle(2), uuid]`
/// (Vol 3, Part G, Section 3.3.1).
fn notifying_in_database(db: &crate::gatt::GattDatabase) -> Vec<(u16, Option<u16>)> {
    const NOTIFYING: u8 = crate::gatt::CharacteristicProperties::NOTIFY
        | crate::gatt::CharacteristicProperties::INDICATE;
    db.attributes
        .values()
        .filter(|attribute| attribute.uuid == Uuid::CHARACTERISTIC)
        .filter_map(|attribute| {
            let [properties, low, high, ..] = attribute.value[..] else {
                return None;
            };
            if properties & NOTIFYING == 0 {
                return None;
            }
            let value_handle = u16::from_le_bytes([low, high]);
            Some((value_handle, find_cccd_after(db, value_handle)))
        })
        .collect()
}

/// Walks the live GATT database and reports the services it holds that the
/// script's own service list does not know about.
///
/// A service registered by a Rust profile registrar (`add_vcs`, `add_pacs`,
/// `add_ascs`, `add_ras`, …) exists only in the `GattDatabase`: it never passed
/// through `server.add_service`, which is what `ScriptGattServer::get_services`
/// enumerates. So a device composed entirely of Rust profiles reported
/// `"services": []` to every consumer — the page's device view, the Explorer,
/// and the MCP status tool — while being a perfectly real device on the air.
/// The Audio page showed "No services yet." for a sink with PACS, ASCS and VCS
/// in its database.
///
/// This is deliberately *additive*: the script's own list is emitted first and
/// unchanged, so every existing page renders exactly what it rendered before,
/// and profile-registered services are appended rather than replacing it.
fn database_only_services(
    db: &crate::gatt::GattDatabase,
    known: &[String],
    subscribed_handles: &[u16],
) -> Vec<ServiceStatus> {
    let mut services: Vec<ServiceStatus> = Vec::new();
    for attribute in db.attributes.values() {
        match attribute.uuid {
            Uuid::PRIMARY_SERVICE | Uuid::SECONDARY_SERVICE => {
                let Some(uuid) = Uuid::from_bytes(&attribute.value) else {
                    continue;
                };
                services.push(ServiceStatus {
                    uuid: uuid.to_string(),
                    characteristics: Vec::new(),
                });
            }
            Uuid::CHARACTERISTIC => {
                // Characteristic declaration value (Core Vol 3, Part G,
                // Section 3.3.1): [properties(1), value_handle(2), uuid].
                let Some(service) = services.last_mut() else {
                    continue;
                };
                let value = &attribute.value;
                if value.len() < 5 {
                    continue;
                }
                let value_handle = u16::from_le_bytes([value[1], value[2]]);
                let Some(uuid) = Uuid::from_bytes(&value[3..]) else {
                    continue;
                };
                service.characteristics.push(CharacteristicStatus {
                    uuid: uuid.to_string(),
                    properties: value[0] as i64,
                    value: db.value(value_handle).map(hex).unwrap_or_default(),
                    subscribed: subscribed_handles.contains(&value_handle),
                });
            }
            _ => {}
        }
    }
    services.retain(|service| !known.contains(&service.uuid));
    services
}

/// Finds the CCCD belonging to the characteristic whose value sits at
/// `value_handle`, scanning forward until the next declaration bounds the
/// characteristic's descriptor group (Core Spec Vol 3, Part G, Section 3.3).
fn find_cccd_after(db: &crate::gatt::GattDatabase, value_handle: u16) -> Option<u16> {
    db.attributes
        .range(value_handle.checked_add(1)?..)
        .take_while(|(_, attribute)| {
            !matches!(
                attribute.uuid,
                Uuid::CHARACTERISTIC | Uuid::PRIMARY_SERVICE | Uuid::SECONDARY_SERVICE
            )
        })
        .find(|(_, attribute)| attribute.uuid == Uuid::CCCD)
        .map(|(&handle, _)| handle)
}

#[cfg(test)]
#[path = "scripted_peripheral_tests.rs"]
mod tests;
