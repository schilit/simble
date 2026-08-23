// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! `android::BluetoothHidHost` — the **HID host** as a script can reach it.
//!
//! ## The name, and how close the correspondence really is
//!
//! Rule 5 of `docs/scripting-profile-apis.md` asks that a reader be able to
//! tell how closely a binding tracks the Android API it is named for. For this
//! one the answer has two halves, and they differ.
//!
//! **The role and the type name: an exact match.** Android's
//! `BluetoothHidHost` (`BluetoothProfile.HID_HOST`, once
//! `BluetoothInputDevice`) is the HID *host* — the computer's end of the link,
//! the opposite end from `BluetoothHidDevice`. It is **not** Classic-only, as
//! is easy to assume: the proxy spans both transports, and the transport split
//! happens well below it. `HidHostService.java`, which backs the proxy,
//! contains no transport-specific code at all — it calls `connectHidNative`
//! and lets the native stack decide. That decision is made in `bta/hh/`, the
//! BTA HID Host module, where `bta_hh_le.c` implements HOGP proper: the HID
//! service, Report Map (0x2A4B), Report (0x2A4D), and CCCD notification
//! registration via `bta_hh_le_register_input_notif`. Crucially it dispatches
//! into the *same* `bta_hh_sm_execute` state machine, with the same
//! `tBTA_HH_CONN` callbacks, that Classic BR/EDR HID uses. One profile, one
//! proxy, two transports — so `BluetoothHidHost` is the honest name for this
//! role, and `@SystemApi` is a deployment gate that the spec has already ruled
//! irrelevant to a simulator.
//!
//! This binding implements the **HOGP (LE) half** of what that proxy covers.
//! The Classic half is out of reach for a reason that has nothing to do with
//! naming: a scene cannot host a BR/EDR device at all yet (see the "Classic"
//! section of the spec).
//!
//! **The decoded-input callbacks: ours, not Android's.** Android's proxy hands
//! an app *raw* reports — `ACTION_REPORT` carrying `EXTRA_REPORT` — and stops
//! there. Turning those bytes into "a key went down" happens above Bluetooth
//! entirely, in the input stack. So `on_report` below mirrors Android;
//! `on_key_down`, `on_pointer` and the rest have **no counterpart on this
//! proxy** and are named for what they are rather than after an Android
//! callback that does not exist. They are the reason the binding is worth
//! having: [`crate::device::HidHost`] already does that decoding in Rust.
//!
//! ## What it is
//!
//! A central that knows what a HID peer is. Underneath it composes two things
//! that already exist and are already tested:
//!
//! - [`ScriptGattClient`] for the GATT half — the HID host *is* a central, so
//!   it borrows the whole central-role binding rather than reinventing a
//!   connection. `on_connection_state_change` (Android's
//!   `ACTION_CONNECTION_STATE_CHANGED`), `on_services_discovered`, `on_error`
//!   and `tick` all still work on it.
//! - [`crate::device::HidHost`] for the protocol half — reading the Report
//!   Map, deciding whether the peer is a keyboard or a mouse, and differencing
//!   consecutive input reports into press and release edges.
//!
//! **No report is decoded in Rhai.** The script sees the *result* of decoding.
//! Re-parsing a USB HID report descriptor in a script would be the wrong seam,
//! and rule 1 of the spec forbids it.
//!
//! ```rhai
//! let host = android::BluetoothHidHost("Computer");
//! host.connect("AA:BB:CC:00:00:01");
//!
//! fn on_identified(host, kind, report_map) {
//!     assert(kind == "keyboard", "the Report Map says keyboard");
//! }
//! fn on_key_down(host, key) {
//!     host.emit("typed", key.character);
//! }
//! ```
//!
//! Discovery is automatic, which is the one place this deliberately does more
//! than the GATT binding: once services are discovered the host reads every
//! Report Map and subscribes to every notifying Report characteristic on its
//! own, because that is what a HID host *is*. A script that had to do it by
//! hand would be re-implementing [`crate::device::HidHost::plan`]. It matches
//! Android, where an app never issues those reads either.
//!
//! **Not bound.** `setProtocolMode`, `getReport`/`setReport`, `sendData`,
//! `virtualUnplug` and the idle-time pair all need device state that
//! [`crate::device::HidHost`] does not model — it is a decoder, not a full
//! HOGP host. Binding them would mean writing that state machine first, so
//! they are absent rather than faked.

use std::cell::RefCell;
use std::rc::Rc;

use rhai::{Array, Dynamic, Engine, EvalAltResult, Map, Module};

use crate::device::HidEvent;
use crate::device::central::CentralEvent;
use crate::scripting::bindings::runtime_error;
use crate::scripting::client::ScriptGattClient;
use crate::types::Address;

/// One pending script callback: a function name and the arguments after the
/// receiver. Same shape the Auracast proxies use.
type Callback = (&'static str, Vec<Dynamic>);

/// Script-side handle to a HID host. `Rc<RefCell>` for the reason
/// [`ScriptGattClient`] is: Rhai requires `Clone`, a live state machine must
/// not be, so every copy a script holds drives the same host.
#[derive(Clone)]
pub struct ScriptHidHost {
    inner: Rc<RefCell<HostInner>>,
}

struct HostInner {
    name: String,
    /// The GATT half. A HID host *is* a central.
    client: ScriptGattClient,
    /// The protocol half: Report Map parsing and report differencing.
    host: crate::device::HidHost,
    /// Whether the discovery-driven plan has been issued. Guards against a
    /// second `ServicesDiscovered` re-reading and re-subscribing everything.
    planned: bool,
    /// The input Report value handles the plan subscribed to, so a
    /// notification on one can be told from the Battery Level a HOGP device
    /// also notifies — only the former is a report.
    report_handles: Vec<u16>,
    pending: Vec<Callback>,
}

impl ScriptHidHost {
    fn create(name: &str) -> Self {
        Self {
            inner: Rc::new(RefCell::new(HostInner {
                name: name.to_string(),
                client: ScriptGattClient::create_for(name),
                host: crate::device::HidHost::new(),
                planned: false,
                report_handles: Vec::new(),
                pending: Vec::new(),
            })),
        }
    }

    /// The host's name.
    pub fn name(&self) -> String {
        self.inner.borrow().name.clone()
    }

    /// The GATT client underneath — what hosts this as a central.
    pub fn client(&self) -> ScriptGattClient {
        self.inner.borrow().client.clone()
    }

    /// Borrows the decoding half, for a host that wants the same view the
    /// scene central publishes (the page's Report Map pane, a test).
    pub fn with_hid<R>(&self, f: impl FnOnce(&mut crate::device::HidHost) -> R) -> R {
        f(&mut self.inner.borrow_mut().host)
    }

    /// Turns one central event into the HID host's callbacks, driving the
    /// HOGP bookkeeping — read the Report Map, subscribe to every input
    /// Report, decode what arrives — that makes them meaningful.
    pub fn observe(&self, event: &CentralEvent) -> Vec<Callback> {
        self.inner.borrow_mut().observe(event);
        std::mem::take(&mut self.inner.borrow_mut().pending)
    }
}

impl HostInner {
    /// Issues the plan a freshly discovered HID peer needs, once.
    ///
    /// Both queues are **handle-addressed**. A HID service may publish several
    /// characteristics that share the Report UUID (0x2A4D) — an input report,
    /// an output report for keyboard LEDs, a feature report — and
    /// `plan` names handles for exactly that reason. Queueing by UUID would
    /// collapse them onto whichever one discovery happened to find first,
    /// which is how a host ends up subscribing to an output report and
    /// receiving no input at all.
    fn start(&mut self, services: &[crate::client::DiscoveredService]) {
        if self.planned {
            return;
        }
        let plan = self.host.plan(services);
        if plan.is_empty() {
            // Not a HID peer. Nothing is queued and `ready` stays false; a
            // script that cares can say so from `on_services_discovered`.
            return;
        }
        self.planned = true;
        self.report_handles = plan.subscribe.clone();
        // The Report Map is read before the subscriptions go out, because a
        // report that arrives before the descriptor has been parsed is
        // uninterpretable and `HidHost` drops it rather than guessing.
        for handle in plan.read {
            self.client.with_central(|c| c.queue_read_at(handle));
        }
        for handle in plan.subscribe {
            self.client
                .with_central(|c| c.queue_subscribe_at(handle, true));
        }
    }

    fn observe(&mut self, event: &CentralEvent) {
        match event {
            CentralEvent::ServicesDiscovered { .. } => {
                let services: Vec<_> = self.client.with_central(|c| c.services().to_vec());
                self.start(&services);
            }
            CentralEvent::CharacteristicRead {
                handle,
                value,
                status,
                ..
            } if *status == 0 => {
                self.host.on_read(*handle, value);
            }
            CentralEvent::CharacteristicChanged { handle, value, .. } => {
                self.host.on_notification(*handle, value);
                // Android's own surface: `ACTION_REPORT` carries the report
                // bytes and nothing more. Raised before the decoded callbacks
                // so a script sees the wire before its meaning.
                if self.report_handles.contains(handle) {
                    self.pending
                        .push(("on_report", vec![Dynamic::from_blob(value.clone())]));
                }
            }
            _ => {}
        }
        // Whatever the last step decoded, in order.
        for decoded in self.host.drain_events() {
            self.push_decoded(&decoded);
        }
    }

    fn push_decoded(&mut self, event: &HidEvent) {
        // The one-stream form first, so a script that would rather match on
        // `on_input` than define seven handlers sees every event.
        self.pending
            .push(("on_input", vec![Dynamic::from_map(input_map(event))]));
        let call: Callback = match event {
            HidEvent::Identified { kind, report_map } => (
                "on_identified",
                vec![kind.label().into(), Dynamic::from_blob(report_map.clone())],
            ),
            HidEvent::KeyDown { .. } => ("on_key_down", vec![Dynamic::from_map(key_map(event))]),
            HidEvent::KeyUp { usage } => ("on_key_up", vec![(*usage as i64).into()]),
            HidEvent::RollOver => ("on_rollover", Vec::new()),
            HidEvent::Pointer { dx, dy, wheel } => (
                "on_pointer",
                vec![
                    (*dx as i64).into(),
                    (*dy as i64).into(),
                    (*wheel as i64).into(),
                ],
            ),
            HidEvent::ButtonDown { button } => ("on_button_down", vec![(*button as i64).into()]),
            HidEvent::ButtonUp { button } => ("on_button_up", vec![(*button as i64).into()]),
        };
        self.pending.push(call);
    }
}

/// A `KeyDown` as a map: `#{usage, character, label, modifiers}`.
///
/// `character` and `label` are `()` when the key produces neither — a script
/// checks `if key.character != ()` rather than comparing against a sentinel.
fn key_map(event: &HidEvent) -> Map {
    let mut map = Map::new();
    if let HidEvent::KeyDown {
        usage,
        modifiers,
        character,
        label,
    } = event
    {
        map.insert("usage".into(), (*usage as i64).into());
        map.insert("modifiers".into(), (*modifiers as i64).into());
        map.insert(
            "character".into(),
            character.map_or(Dynamic::UNIT, |c| c.to_string().into()),
        );
        map.insert(
            "label".into(),
            label.map_or(Dynamic::UNIT, |l| l.to_string().into()),
        );
    }
    map
}

/// Any decoded event as a map, for `fn on_input(host, event)`. `event.type`
/// carries the same snake_case tag [`HidEvent`]'s `serde` representation uses,
/// so a script and the JSON a page reads agree on the vocabulary.
fn input_map(event: &HidEvent) -> Map {
    let mut map = Map::new();
    let mut set = |key: &str, value: Dynamic| {
        map.insert(key.into(), value);
    };
    match event {
        HidEvent::Identified { kind, report_map } => {
            set("type", "identified".into());
            set("kind", kind.label().into());
            set("report_map", Dynamic::from_blob(report_map.clone()));
        }
        HidEvent::KeyDown { .. } => {
            set("type", "key_down".into());
            for (key, value) in key_map(event) {
                set(&key, value);
            }
        }
        HidEvent::KeyUp { usage } => {
            set("type", "key_up".into());
            set("usage", (*usage as i64).into());
        }
        HidEvent::RollOver => set("type", "roll_over".into()),
        HidEvent::Pointer { dx, dy, wheel } => {
            set("type", "pointer".into());
            set("dx", (*dx as i64).into());
            set("dy", (*dy as i64).into());
            set("wheel", (*wheel as i64).into());
        }
        HidEvent::ButtonDown { button } => {
            set("type", "button_down".into());
            set("button", (*button as i64).into());
        }
        HidEvent::ButtonUp { button } => {
            set("type", "button_up".into());
            set("button", (*button as i64).into());
        }
    }
    map
}

/// Registers `android::BluetoothHidHost` and its methods, in the same
/// namespace as `android::BluetoothGatt` — as it is on Android, where the HID
/// host is one of `BluetoothProfile`'s proxies.
pub fn register(engine: &mut Engine, android: &mut Module) {
    engine
        .register_type_with_name::<ScriptHidHost>("BluetoothHidHost")
        .register_get("name", |host: &mut ScriptHidHost| host.name())
        .register_get("peer", |host: &mut ScriptHidHost| {
            host.client().with_central(|c| c.target().to_string())
        })
        .register_get("connected", |host: &mut ScriptHidHost| {
            host.client().with_central(|c| c.connection_handle() != 0)
        })
        .register_get("discovered", |host: &mut ScriptHidHost| {
            host.client().with_central(|c| c.is_ready())
        })
        // The GATT client underneath, so a script can do plain GATT on the
        // same connection — read the Battery Level a HOGP device also has.
        .register_get("gatt", |host: &mut ScriptHidHost| host.client())
        // What the Report Map said the peer is: "keyboard", "mouse",
        // "game pad", "unsupported", or "unknown" before it has been read.
        .register_get("kind", |host: &mut ScriptHidHost| {
            host.with_hid(|h| h.kind().label().to_string())
        })
        // True once the Report Map has been read and reports subscribed.
        .register_get("ready", |host: &mut ScriptHidHost| {
            host.with_hid(|h| h.is_ready())
        })
        // The Report Map bytes, empty until read.
        .register_get("report_map", |host: &mut ScriptHidHost| {
            host.with_hid(|h| h.report_map().map(<[u8]>::to_vec).unwrap_or_default())
        })
        // The most recent input report, exactly as it arrived — the wire
        // beside its meaning. Empty before the first one.
        .register_get("report", |host: &mut ScriptHidHost| {
            host.with_hid(|h| {
                h.last_report()
                    .map(|(_, bytes)| bytes.to_vec())
                    .unwrap_or_default()
            })
        })
        // The value handle that report arrived on, 0 before the first one.
        .register_get("report_handle", |host: &mut ScriptHidHost| {
            host.with_hid(|h| h.last_report().map_or(0i64, |(handle, _)| handle as i64))
        })
        // The largest input report this host expects, for a script sizing a
        // view of the raw bytes.
        .register_get("report_len_hint", |host: &mut ScriptHidHost| {
            host.with_hid(|h| h.report_len_hint() as i64)
        })
        .register_fn(
            "connect",
            |host: &mut ScriptHidHost, address: &str| -> Result<(), Box<EvalAltResult>> {
                let address = address.parse::<Address>().map_err(|e| {
                    runtime_error(format!("connect: {address:?} is not an address: {e}"))
                })?;
                host.client().connect(address);
                Ok(())
            },
        )
        .register_fn("disconnect", |host: &mut ScriptHidHost| {
            host.client().disconnect();
        })
        .register_fn("services", |host: &mut ScriptHidHost| -> Array {
            host.client()
                .with_central(|c| c.services().iter().map(|s| Dynamic::from(s.uuid)).collect())
        })
        .register_fn(
            // The return path to the page or the test, the same `emit` every
            // other script object offers.
            "emit",
            |host: &mut ScriptHidHost,
             kind: &str,
             payload: Dynamic|
             -> Result<(), Box<EvalAltResult>> { host.client().emit(kind, payload) },
        );

    android.set_native_fn(
        "BluetoothHidHost",
        |name: &str| -> Result<ScriptHidHost, Box<EvalAltResult>> {
            Ok(ScriptHidHost::create(name))
        },
    );
}
