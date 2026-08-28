// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Browser (wasm32) bindings for the GitHub Pages demos in `web/`: Simble
//! compiled to WebAssembly, talking to the visitor's local netsimd over the
//! browser's native `WebSocket` (the same
//! `ws://localhost:7681/v1/websocket/bt?name=<n>&address=<mac>` endpoint as
//! `netsim`, but with the browser doing all RFC 6455 framing).
//!
//! This module is *only* the browser half. The target-independent engines it
//! wraps live in proper homes and compile and unit-test on every target:
//! - scan-report parsing and demo HCI bring-up in
//!   [`crate::transport::scan_report`],
//! - the scripted peripheral in [`crate::device::scripted_peripheral`], the
//!   scene central in [`crate::device::central_device`], and the classic
//!   device in [`crate::device::classic_device`],
//! - the multi-device [`SceneEngine`](crate::scene::engine::SceneEngine),
//! - and the script compile/run entry points in
//!   [`crate::scripting::test_script`].
//!
//! The `web` submodule below (gated `#[cfg(target_arch = "wasm32")]`) wraps
//! `web_sys::WebSocket` and exports the page-facing wasm-bindgen types. Browser
//! pages drive everything from a JS interval calling `tick()` — there are no
//! blocking loops, because wasm shares the page's event loop.

// Re-imported here so the `web` submodule's `super::` references resolve to the
// engines' new homes without touching its body.
#[cfg(target_arch = "wasm32")]
use crate::device::central_device::CentralDevice;
#[cfg(target_arch = "wasm32")]
use crate::device::classic_device::ClassicDevice;
#[cfg(target_arch = "wasm32")]
use crate::device::scripted_peripheral::{DEFAULT_HEART_RATE_SCRIPT, ScriptedPeripheral};
#[cfg(target_arch = "wasm32")]
use crate::scene::engine::SceneEngine;
#[cfg(target_arch = "wasm32")]
use crate::scripting::test_script::run_test_script;
#[cfg(target_arch = "wasm32")]
use crate::transport::scan_report::{
    address_from_ws_url, address_type_name, parse_scan_reports, queue_advertiser_start,
    queue_scanner_start, ws_url_with_wire_address,
};

#[cfg(target_arch = "wasm32")]
mod web {
    //! The browser half: `web_sys::WebSocket` pumping and the wasm-bindgen
    //! types the demo pages instantiate. Each `tick()` call from the page's
    //! JS interval pumps both directions once and returns render-ready JSON.

    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;
    use std::sync::Once;

    use wasm_bindgen::JsCast;
    use wasm_bindgen::prelude::*;
    use web_sys::{BinaryType, MessageEvent, WebSocket};

    use super::super::hci_adapter::HciChannel;
    use super::{
        DEFAULT_HEART_RATE_SCRIPT, ScriptedPeripheral, address_from_ws_url, parse_scan_reports,
        queue_advertiser_start, queue_scanner_start, ws_url_with_wire_address,
    };
    use crate::types::Address;

    /// Panics otherwise vanish into an opaque `unreachable` trap; route them
    /// to the browser console instead.
    fn install_panic_hook() {
        static HOOK: Once = Once::new();
        HOOK.call_once(|| {
            std::panic::set_hook(Box::new(|info| {
                web_sys::console::error_1(&JsValue::from_str(&info.to_string()));
            }));
        });
    }

    fn js_error(message: impl std::fmt::Display) -> JsValue {
        JsValue::from_str(&message.to_string())
    }

    /// The wasm sibling of `NetsimTransport`: same pump shape (drain
    /// `HciChannel` host packets to the socket, drain received messages into
    /// the channel), but the browser owns all WebSocket framing, and receipt
    /// is event-driven — `onmessage` queues packets for the next pump.
    struct WasmWsTransport {
        ws: WebSocket,
        inbound: Rc<RefCell<VecDeque<Vec<u8>>>>,
        _on_message: Closure<dyn FnMut(MessageEvent)>,
    }

    impl WasmWsTransport {
        fn connect(url: &str) -> Result<Self, JsValue> {
            let ws = WebSocket::new(url)?;
            ws.set_binary_type(BinaryType::Arraybuffer);
            let inbound: Rc<RefCell<VecDeque<Vec<u8>>>> = Rc::default();
            let queue = inbound.clone();
            let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
                if let Ok(buffer) = event.data().dyn_into::<js_sys::ArrayBuffer>() {
                    queue
                        .borrow_mut()
                        .push_back(js_sys::Uint8Array::new(&buffer).to_vec());
                }
            });
            ws.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
            Ok(Self {
                ws,
                inbound,
                _on_message: on_message,
            })
        }

        fn ready_state(&self) -> u16 {
            self.ws.ready_state()
        }

        fn is_open(&self) -> bool {
            self.ready_state() == WebSocket::OPEN
        }

        fn pump(&self, channel: &HciChannel) -> Result<(), JsValue> {
            if self.is_open() {
                while let Some(packet) = channel.poll_host_packet() {
                    self.ws.send_with_u8_array(&packet)?;
                }
            }
            loop {
                let next = self.inbound.borrow_mut().pop_front();
                match next {
                    Some(packet) => channel.receive_from_controller(packet).map_err(js_error)?,
                    None => break,
                }
            }
            Ok(())
        }
    }

    impl Drop for WasmWsTransport {
        fn drop(&mut self) {
            self.ws.set_onmessage(None);
            let _ = self.ws.close();
        }
    }

    /// The scanner page's engine: joins netsim as a scanning device and
    /// returns decoded advertising reports as JSON on every tick.
    #[wasm_bindgen]
    pub struct WebScanner {
        transport: WasmWsTransport,
        channel: HciChannel,
        started: bool,
    }

    #[wasm_bindgen]
    impl WebScanner {
        #[wasm_bindgen(constructor)]
        pub fn new(url: &str) -> Result<WebScanner, JsValue> {
            install_panic_hook();
            Ok(Self {
                transport: WasmWsTransport::connect(url)?,
                channel: HciChannel::new(),
                started: false,
            })
        }

        /// 0 = connecting, 1 = open, 2 = closing, 3 = closed — the page's
        /// connection-failure UX keys off this.
        /// Returns the underlying WebSocket ready state (per the WebSocket API).
        pub fn ready_state(&self) -> u16 {
            self.transport.ready_state()
        }

        /// One pump: returns a JSON array of decoded advertising reports
        /// (possibly empty).
        pub fn tick(&mut self) -> Result<String, JsValue> {
            self.transport.pump(&self.channel)?;
            if !self.started && self.transport.is_open() {
                queue_scanner_start(&self.channel).map_err(js_error)?;
                self.started = true;
                self.transport.pump(&self.channel)?;
            }
            let mut reports = Vec::new();
            while let Some(packet) = self.channel.poll_controller_packet() {
                reports.extend(parse_scan_reports(&packet));
            }
            serde_json::to_string(&reports).map_err(js_error)
        }
    }

    /// The **in-page controller** backend: a whole scene of scripted devices
    /// sharing one in-process [`Link`](crate::controller::sim::Link), with no
    /// WebSocket and no netsim. Add peripherals and scanners, `tick()` on a
    /// LC3 for the demo pages: encode PCM into the frames a source puts on
    /// the air, decode the frames a sink received so the page can play
    /// them. Behind the `lc3` feature — the pages build enables it, the
    /// `simble mcp` binary does not need it (SDUs are opaque to the
    /// protocol layers).
    #[cfg(feature = "lc3")]
    #[wasm_bindgen]
    pub struct WebLc3 {
        // The stream configuration lives inside these two now: both are
        // stateful across frames and are built for one configuration.
        encoder: crate::audio::lc3::Lc3Encode,
        decoder: crate::audio::lc3::Lc3Stream,
    }

    #[cfg(feature = "lc3")]
    #[wasm_bindgen]
    impl WebLc3 {
        /// Creates a codec for one stream's configuration — the same values
        /// the ASE was configured with (16 kHz / 10 ms is what simble's PAC
        /// record advertises).
        #[wasm_bindgen(constructor)]
        pub fn new(sample_rate_hz: u32, frame_duration_us: u32) -> Result<WebLc3, JsValue> {
            Ok(Self {
                encoder: crate::audio::lc3::Lc3Encode::new(sample_rate_hz, frame_duration_us)
                    .map_err(js_error)?,
                decoder: crate::audio::lc3::Lc3Stream::new(sample_rate_hz, frame_duration_us)
                    .map_err(js_error)?,
            })
        }

        /// PCM samples per frame, so the page knows how much audio to hand
        /// over and how much to expect back.
        pub fn samples_per_frame(&self) -> usize {
            self.encoder.samples_per_frame()
        }

        /// Encodes one frame of 16-bit PCM into `frame_bytes` of LC3.
        pub fn encode(
            &mut self,
            samples: Vec<i16>,
            frame_bytes: usize,
        ) -> Result<Vec<u8>, JsValue> {
            self.encoder.encode(&samples, frame_bytes).map_err(js_error)
        }

        /// Decodes one LC3 frame back to 16-bit PCM.
        pub fn decode(&mut self, frame: Vec<u8>) -> Result<Vec<i16>, JsValue> {
            self.decoder.decode(&frame).map_err(js_error)
        }
    }

    /// timer, and read each device's state — the browser pages use this when
    /// the backend selector is set to "in-page". Wraps [`SceneEngine`].
    /// The ranging demo's scene: a tag and a locator on one simulated
    /// medium, measured both by RSSI and by Channel Sounding.
    ///
    /// The page owns one timer and calls [`WebRanging::tick`]; everything
    /// else it shows comes out of [`WebRanging::status_json`], which reports
    /// the ground truth alongside both estimates and the raw measurements
    /// behind them. See [`crate::device::ranging_scene`].
    #[wasm_bindgen]
    pub struct WebRanging {
        scene: crate::device::RangingScene,
    }

    #[wasm_bindgen]
    impl WebRanging {
        /// Creates the scene with a tag and a locator at the given addresses.
        #[wasm_bindgen(constructor)]
        pub fn new(tag: &str, locator: &str) -> Result<WebRanging, JsValue> {
            install_panic_hook();
            Ok(Self {
                scene: crate::device::RangingScene::new(
                    tag.parse().map_err(js_error)?,
                    locator.parse().map_err(js_error)?,
                ),
            })
        }

        /// Advances both devices one step.
        pub fn tick(&mut self) {
            self.scene.tick();
        }

        /// Moves the tag to `(x, y)` metres on the floor plan.
        pub fn set_tag_position(&mut self, x: f64, y: f64) {
            self.scene
                .set_tag_position(crate::controller::propagation::Position::new(x, y));
        }

        /// Sets the room the radio propagates through: the transmit power in
        /// dBm, the path-loss exponent, and the shadowing standard deviation
        /// in dB. These are the *truth*; the locator does not learn them.
        pub fn set_room(&mut self, tx_power_dbm: f64, path_loss_exponent: f64, shadowing_db: f64) {
            let mut model = self.scene.path_loss();
            model.tx_power_dbm = tx_power_dbm;
            model.path_loss_exponent = path_loss_exponent;
            model.shadowing_sigma_db = shadowing_db;
            self.scene.set_path_loss(model);
        }

        /// Sets what the locator's RSSI estimator *assumes*: the calibrated
        /// one-metre RSSI and the path-loss exponent. Changing these
        /// re-derives the estimate from samples already collected.
        pub fn set_rssi_assumptions(&mut self, reference_dbm: f64, path_loss_exponent: f64) {
            self.scene
                .set_rssi_assumptions(crate::cs::RssiRangingParams {
                    reference_rssi_dbm: reference_dbm,
                    path_loss_exponent,
                });
        }

        /// Reseeds the medium's noise, so a run repeats exactly.
        pub fn set_noise_seed(&mut self, seed: f64) {
            self.scene.set_noise_seed(seed as u64);
        }

        /// The whole scene as JSON: truth, room, link state, and both
        /// methods' inputs, estimates, and errors.
        pub fn status_json(&self) -> String {
            self.scene.status_json()
        }
    }

    #[wasm_bindgen]
    pub struct WebLink {
        scene: super::SceneEngine,
    }

    #[wasm_bindgen]
    impl WebLink {
        /// Creates an empty in-page scene.
        #[wasm_bindgen(constructor)]
        pub fn new() -> WebLink {
            install_panic_hook();
            Self {
                scene: super::SceneEngine::new(),
            }
        }

        /// Adds a scripted peripheral at `address` (e.g. `"AA:BB:CC:00:00:01"`);
        /// returns its device index, or the script/address error.
        pub fn add_peripheral(&mut self, address: &str, script: &str) -> Result<usize, JsValue> {
            let address = address.parse().map_err(js_error)?;
            self.scene.add_peripheral(address, script).map_err(js_error)
        }

        /// Adds a scanner at `address`; returns its device index.
        pub fn add_scanner(&mut self, address: &str) -> Result<usize, JsValue> {
            let address = address.parse().map_err(js_error)?;
            Ok(self.scene.add_scanner(address))
        }

        /// Adds a central at `address` that connects to and discovers the
        /// peripheral at `target`; returns its device index.
        pub fn add_central(&mut self, address: &str, target: &str) -> Result<usize, JsValue> {
            let address = address.parse().map_err(js_error)?;
            let target = target.parse().map_err(js_error)?;
            Ok(self.scene.add_central(address, target))
        }

        /// Adds a **BR/EDR** device at `address` — the fifth thing a scene can
        /// host, and the only one that is not LE.
        ///
        /// `role` is `"acceptor"` for a device that makes itself discoverable
        /// and connectable and serves an echoing serial port on
        /// `rfcomm_channel`, or `"initiator"` for one that inquires for
        /// `target`, resolves its name, pages it, queries its SDP, opens the
        /// serial port that record advertises and sends `payload` over it.
        /// `target` is ignored for an acceptor, and `rfcomm_channel` for an
        /// initiator — which channel to open is exactly what SDP is asked.
        ///
        /// Read its progress back with
        /// [`Self::classic_status_json`]; a BR/EDR link has no advertising
        /// report to watch, so the phase list is the only view of it there is.
        pub fn add_classic_device(
            &mut self,
            address: &str,
            role: &str,
            name: &str,
            rfcomm_channel: u8,
            target: &str,
            payload: &str,
        ) -> Result<usize, JsValue> {
            let address = address.parse().map_err(js_error)?;
            let device = match role {
                "acceptor" => super::ClassicDevice::acceptor(
                    name,
                    // Rendering / audio-video, wearable headset: what a
                    // simble peripheral has always claimed to be.
                    [0x04, 0x04, 0x24],
                    rfcomm_channel,
                ),
                "initiator" => super::ClassicDevice::initiator(
                    name,
                    [0x0C, 0x02, 0x5A], // smartphone
                    target.parse().map_err(js_error)?,
                    payload.as_bytes().to_vec(),
                ),
                other => {
                    return Err(js_error(format!(
                        "unknown classic role {other:?}: expected \"acceptor\" or \"initiator\""
                    )));
                }
            };
            Ok(self.scene.add_classic_device(address, device))
        }

        /// The BR/EDR status of classic device `index`: its phase, what its
        /// inquiry found, the ACL connection, what SDP answered, and the
        /// RFCOMM data link's credit window. `undefined` if that device is
        /// not a classic one.
        pub fn classic_status_json(&self, index: usize) -> Option<String> {
            self.scene.classic_status_json(index)
        }

        /// Adds a *scripted* central at `address` — a Rhai script that builds
        /// an `android::BluetoothGatt`, connects it, and reacts in callbacks.
        /// Returns its device index, or the script/address error, so a page
        /// can show a compile failure on the line that caused it.
        pub fn add_scripted_central(
            &mut self,
            address: &str,
            script: &str,
        ) -> Result<usize, JsValue> {
            let address = address.parse().map_err(js_error)?;
            self.scene
                .add_scripted_central(address, script)
                .map_err(js_error)
        }

        /// Points scripted central `index` at `target`, overriding the address
        /// its script named — for a page that allocates addresses itself.
        pub fn scripted_central_set_target(
            &mut self,
            index: usize,
            target: &str,
        ) -> Result<(), JsValue> {
            let target = target.parse().map_err(js_error)?;
            match self.scene.scripted_central_mut(index) {
                Some(central) => {
                    central.set_target(target);
                    Ok(())
                }
                None => Err(js_error("not a scripted central")),
            }
        }

        /// Drains what scripted central `index` emitted with
        /// `client.emit(kind, payload)` — the script's channel to the page.
        pub fn scripted_central_emitted(&mut self, index: usize) -> js_sys::Array {
            let out = js_sys::Array::new();
            if let Some(central) = self.scene.scripted_central_mut(index) {
                for message in central.take_emitted() {
                    out.push(&JsValue::from_str(&message));
                }
            }
            out
        }

        /// Queues a read on scripted central `index`, naming the
        /// characteristic by UUID string (`"2A37"` or a full 128-bit UUID) —
        /// what the discovered tree a page renders already holds.
        pub fn scripted_central_read(&mut self, index: usize, uuid: &str) -> Result<(), JsValue> {
            let uuid: crate::types::Uuid = uuid.parse().map_err(js_error)?;
            match self.scene.scripted_central_mut(index) {
                Some(central) => {
                    central.read(uuid);
                    Ok(())
                }
                None => Err(js_error("not a scripted central")),
            }
        }

        /// Queues a write (Write Request) on scripted central `index`.
        pub fn scripted_central_write(
            &mut self,
            index: usize,
            uuid: &str,
            value: Vec<u8>,
        ) -> Result<(), JsValue> {
            let uuid: crate::types::Uuid = uuid.parse().map_err(js_error)?;
            match self.scene.scripted_central_mut(index) {
                Some(central) => {
                    central.write(uuid, value, true);
                    Ok(())
                }
                None => Err(js_error("not a scripted central")),
            }
        }

        /// Queues enabling (or disabling) notifications on scripted central
        /// `index`.
        pub fn scripted_central_subscribe(
            &mut self,
            index: usize,
            uuid: &str,
            enable: bool,
        ) -> Result<(), JsValue> {
            let uuid: crate::types::Uuid = uuid.parse().map_err(js_error)?;
            match self.scene.scripted_central_mut(index) {
                Some(central) => {
                    central.subscribe(uuid, enable);
                    Ok(())
                }
                None => Err(js_error("not a scripted central")),
            }
        }

        /// The first error a scripted central's callbacks raised — a failed
        /// `assert`, or an operation naming a characteristic the peer does not
        /// have. `undefined` while the script is behaving.
        pub fn scripted_central_failure(&self, index: usize) -> Option<String> {
            self.scene
                .scripted_central(index)
                .and_then(|c| c.failure().map(str::to_string))
        }

        /// The discovered-GATT JSON of central `index` (`undefined` if not a
        /// central).
        pub fn central_status_json(&self, index: usize) -> Option<String> {
            self.scene.central_status_json(index)
        }

        /// Queue a read of `value_handle` on central `index`.
        pub fn central_read(&mut self, index: usize, value_handle: u16) {
            self.scene.central_read(index, value_handle);
        }

        /// Queue a write of `value` to `value_handle` on central `index`.
        pub fn central_write(&mut self, index: usize, value_handle: u16, value: Vec<u8>) {
            self.scene.central_write(index, value_handle, value);
        }

        /// Queue enabling notifications on `value_handle` for central `index`.
        pub fn central_subscribe(&mut self, index: usize, value_handle: u16) {
            self.scene.central_subscribe(index, value_handle);
        }

        /// Host-write `value` into characteristic `uuid` of peripheral
        /// `index` and notify it even if the bytes are unchanged — what a
        /// report of *change* (a relative mouse report, a repeated key)
        /// needs.
        pub fn peripheral_notify_value(
            &mut self,
            index: usize,
            uuid: &str,
            value: Vec<u8>,
        ) -> Result<(), JsValue> {
            self.scene
                .peripheral_notify_value(index, uuid, &value)
                .map_err(js_error)
        }

        /// Drive central `index` as a HID host — read the peer's Report Map
        /// and subscribe to its input Reports. False until discovery is done,
        /// so a page calls it each tick until it takes.
        pub fn central_start_hid(&mut self, index: usize) -> bool {
            self.scene.central_start_hid(index)
        }

        /// The HID input central `index` has decoded since the last call:
        /// `{kind, ready, report_map, events:[…]}`. Draining.
        pub fn central_hid_events_json(&mut self, index: usize) -> String {
            self.scene.central_hid_events_json(index)
        }

        /// Host-write `value` into characteristic `uuid` of peripheral `index`
        /// (updates the live GATT database and notifies subscribers).
        pub fn peripheral_set_value(
            &mut self,
            index: usize,
            uuid: &str,
            value: Vec<u8>,
        ) -> Result<(), JsValue> {
            self.scene
                .peripheral_set_value(index, uuid, &value)
                .map_err(js_error)
        }

        /// The number of devices in the scene.
        /// Streams one isochronous SDU (a codec frame's worth of audio) from
        /// central `index` to the peripheral it is connected to.
        pub fn central_send_audio(&mut self, index: usize, sdu: Vec<u8>) -> bool {
            self.scene.central_send_audio(index, &sdu)
        }

        /// Drains the SDUs peripheral `index` has received, as an array of
        /// byte arrays — what the page feeds to its audio output.
        pub fn peripheral_take_audio(&mut self, index: usize) -> js_sys::Array {
            let out = js_sys::Array::new();
            for sdu in self.scene.peripheral_take_audio(index) {
                out.push(&js_sys::Uint8Array::from(&sdu[..]).into());
            }
            out
        }

        pub fn device_count(&self) -> usize {
            self.scene.device_count()
        }

        /// Advances the whole scene one step at simulated time `t_seconds`.
        pub fn tick(&mut self, t_seconds: f64) {
            self.scene.tick(t_seconds);
        }

        /// The GATT status JSON of peripheral `index`, or `undefined` if that
        /// device isn't a peripheral.
        pub fn peripheral_status_json(&self, index: usize) -> Option<String> {
            self.scene.peripheral_status_json(index)
        }

        /// New scan reports for scanner `index` as a JSON array (drained on read).
        pub fn scanner_reports_json(&mut self, index: usize) -> String {
            self.scene.scanner_reports_json(index)
        }
    }

    /// A lightweight, advertise-only device the scanner page spins up to
    /// populate an otherwise-empty netsim scene (no GATT server, no script —
    /// just a name, an optional 16-bit service UUID, and optional manufacturer
    /// data on the air). Several of these run on their own sockets so the
    /// scanner demos something on first open.
    #[wasm_bindgen]
    pub struct WebAdvertiser {
        transport: WasmWsTransport,
        channel: HciChannel,
        started: bool,
        name: String,
        service_uuid: u16,
        mfg_company: u16,
        mfg_data: Vec<u8>,
    }

    #[wasm_bindgen]
    impl WebAdvertiser {
        /// `service_uuid` of 0 means "no service UUID"; an empty `mfg_data`
        /// means "no manufacturer data".
        #[wasm_bindgen(constructor)]
        pub fn new(
            url: &str,
            name: &str,
            service_uuid: u16,
            mfg_company: u16,
            mfg_data: Vec<u8>,
        ) -> Result<WebAdvertiser, JsValue> {
            install_panic_hook();
            Ok(Self {
                transport: WasmWsTransport::connect(url)?,
                channel: HciChannel::new(),
                started: false,
                name: name.to_string(),
                service_uuid,
                mfg_company,
                mfg_data,
            })
        }

        /// Returns the underlying WebSocket ready state (per the WebSocket API).
        pub fn ready_state(&self) -> u16 {
            self.transport.ready_state()
        }

        /// One pump: on first open, issues the advertising bring-up; then keeps
        /// the socket drained. Advertise-only, so any inbound controller
        /// packets (a central probing) are discarded.
        pub fn tick(&mut self) -> Result<(), JsValue> {
            self.transport.pump(&self.channel)?;
            if !self.started && self.transport.is_open() {
                queue_advertiser_start(
                    &self.channel,
                    &self.name,
                    self.service_uuid,
                    self.mfg_company,
                    &self.mfg_data,
                )
                .map_err(js_error)?;
                self.started = true;
            }
            while self.channel.poll_controller_packet().is_some() {}
            self.transport.pump(&self.channel)?;
            Ok(())
        }
    }

    /// The HRM page's engine: a running Simble whose device is defined by an
    /// editable Rhai script (see [`ScriptedPeripheral`]).
    #[wasm_bindgen]
    pub struct WebPeripheral {
        transport: WasmWsTransport,
        channel: HciChannel,
        peripheral: ScriptedPeripheral,
        started: bool,
        /// The on-air address netsim advertises for this device, kept so a
        /// script rebuild re-stamps the identity SMP computes with.
        address: Option<Address>,
    }

    #[wasm_bindgen]
    impl WebPeripheral {
        #[wasm_bindgen(constructor)]
        pub fn new(url: &str, script: &str) -> Result<WebPeripheral, JsValue> {
            install_panic_hook();
            let mut peripheral = ScriptedPeripheral::run_script(script).map_err(js_error)?;
            // The device must own the address netsim advertises for it, not
            // the script engine's placeholder — SMP mixes it into the pairing
            // crypto, and a real controller drives key distribution off the
            // Encryption Change event.
            if let Some(address) = address_from_ws_url(url) {
                peripheral.set_identity(address);
            }

            let address = address_from_ws_url(url);
            // netsim reads the URL address LSB-first; connect with the wire
            // form so the device lands on the air where the page says it is.
            let url = ws_url_with_wire_address(url);
            Ok(Self {
                transport: WasmWsTransport::connect(&url)?,
                channel: HciChannel::new(),
                peripheral,
                started: false,
                address,
            })
        }

        /// Tears down the current scripted device and rebuilds it from
        /// `script` on the same socket (Run/Restart button). Errors are the
        /// script's compile/runtime message; on error the old device keeps
        /// running.
        pub fn run_script(&mut self, script: &str) -> Result<(), JsValue> {
            let mut peripheral = ScriptedPeripheral::run_script(script).map_err(js_error)?;
            if let Some(address) = self.address {
                peripheral.set_identity(address);
            }
            self.peripheral = peripheral;
            self.channel = HciChannel::new();
            self.started = false;
            Ok(())
        }

        /// Returns the underlying WebSocket ready state (per the WebSocket API).
        pub fn ready_state(&self) -> u16 {
            self.transport.ready_state()
        }

        /// Drains the isochronous SDUs this device has received, as an array
        /// of byte arrays — the netsim counterpart of
        /// `WebLink.peripheral_take_audio`, so a page-hosted sink can play
        /// audio streamed to it by a real peer rather than only by an
        /// in-page source.
        pub fn take_audio(&mut self) -> js_sys::Array {
            let out = js_sys::Array::new();
            for sdu in self.peripheral.take_audio() {
                out.push(&js_sys::Uint8Array::from(&sdu[..]).into());
            }
            out
        }

        /// Writes a characteristic's value from the page (the lightbulb's
        /// colour picker). `uuid` is the string form; on the next tick a
        /// subscribed central is notified of the change.
        pub fn set_value(&mut self, uuid: &str, value: Vec<u8>) -> Result<(), JsValue> {
            self.peripheral
                .set_characteristic_value(uuid, &value)
                .map_err(js_error)
        }

        /// Writes a characteristic and notifies it even when the bytes are
        /// unchanged — the netsim counterpart of
        /// `WebLink::peripheral_notify_value`, and the reason the HID domain
        /// can run here at all. See
        /// [`ScriptedPeripheral::notify_characteristic_value`]: a HID input
        /// report describes *change*, so two identical reports are two
        /// events, and the value-diff that is right for a battery level would
        /// swallow the second of them.
        pub fn notify_value(&mut self, uuid: &str, value: Vec<u8>) -> Result<(), JsValue> {
            self.peripheral
                .notify_characteristic_value(uuid, &value)
                .map_err(js_error)
        }

        /// One pump + script tick; `t_seconds` is seconds since the current
        /// script was Run. Returns the peripheral status JSON.
        pub fn tick(&mut self, t_seconds: f64) -> Result<String, JsValue> {
            self.transport.pump(&self.channel)?;
            if !self.started && self.transport.is_open() {
                self.peripheral
                    .queue_start(&self.channel)
                    .map_err(js_error)?;
                self.started = true;
            }
            while let Some(packet) = self.channel.poll_controller_packet() {
                if let Err(e) = self.peripheral.handle_packet(&self.channel, &packet) {
                    self.peripheral.record_error(e.to_string());
                }
            }
            if let Err(e) = self.peripheral.tick(&self.channel, t_seconds) {
                self.peripheral.record_error(e.to_string());
            }
            self.transport.pump(&self.channel)?;
            Ok(self.peripheral.status_json())
        }
    }

    /// An LE Audio **source** hosted in the page and running on netsim: the
    /// central that connects to a sink, configures its endpoint, opens a real
    /// CIS, and streams SDUs to it.
    ///
    /// [`WebPeripheral`] is the sink half. Until this existed a foreign stack
    /// had to be the source for any LE Audio test, because simble could
    /// accept a CIS but never open one. The pieces it drives —
    /// [`CisCentral`](crate::device::CisCentral) for the media plane and
    /// [`AseConfig`](crate::profiles::ascs_client::AseConfig) for the control
    /// plane — live in the library, so this type is only the browser's
    /// WebSocket and a running order.
    #[wasm_bindgen]
    pub struct WebSource {
        transport: WasmWsTransport,
        channel: HciChannel,
        central: super::CentralDevice,
        cis: crate::device::CisCentral,
        ase: crate::profiles::ascs_client::AseConfig,
        started: bool,
        /// Whether the three ASE Control Point writes have been queued.
        ase_requested: bool,
        /// Whether CIS establishment has been kicked off.
        cis_requested: bool,
        /// SDUs handed over before the stream was ready, so audio can be
        /// queued the moment a file is loaded rather than only after the
        /// handshake finishes.
        pending_audio: VecDeque<Vec<u8>>,
        /// SDUs discarded while waiting for the stream to open.
        dropped: u32,
        error: Option<String>,
    }

    #[wasm_bindgen]
    impl WebSource {
        /// Connects a source to netsim at `url` and aims it at `target`
        /// (e.g. "CC:1E:57:00:00:06").
        #[wasm_bindgen(constructor)]
        pub fn new(url: &str, target: &str) -> Result<WebSource, JsValue> {
            install_panic_hook();
            let target: Address = target
                .parse()
                .map_err(|_| JsValue::from_str("target is not a Bluetooth address"))?;
            let url = ws_url_with_wire_address(url);
            Ok(Self {
                transport: WasmWsTransport::connect(&url)?,
                channel: HciChannel::new(),
                central: super::CentralDevice::new(target),
                cis: crate::device::CisCentral::new(crate::device::CisConfig::default()),
                ase: crate::profiles::ascs_client::AseConfig::default(),
                started: false,
                ase_requested: false,
                cis_requested: false,
                pending_audio: VecDeque::new(),
                dropped: 0,
                error: None,
            })
        }

        /// Returns the underlying WebSocket ready state (per the WebSocket API).
        pub fn ready_state(&self) -> u16 {
            self.transport.ready_state()
        }

        /// True once SDUs handed to [`send_audio`](Self::send_audio) will
        /// actually reach the sink.
        pub fn is_streaming(&self) -> bool {
            self.cis.is_streaming()
        }

        /// Hands one SDU to the stream.
        ///
        /// Once the stream is up this goes straight out. The queue exists
        /// only to bridge the gap before the CIS opens, so that a page can
        /// start feeding a decoded file the moment the user picks it — it is
        /// deliberately not in the streaming path. Putting it there cost
        /// real audio: a throttled browser tab wakes rarely and then hands
        /// over a large burst, which overran the bound and silently
        /// discarded the overflow.
        ///
        /// Pre-stream buffering is still bounded, because a handshake that
        /// never completes must not grow the queue without limit; what it
        /// drops is counted rather than lost quietly.
        pub fn send_audio(&mut self, sdu: Vec<u8>) {
            if self.cis.is_streaming() {
                if let Some(packet) = self.cis.send_sdu(&sdu) {
                    let _ = self.channel.inject_host_packet(packet);
                }
                return;
            }
            self.pending_audio.push_back(sdu);
            while self.pending_audio.len() > 200 {
                self.pending_audio.pop_front();
                self.dropped += 1;
            }
        }

        /// One pump: bring the controller up, advance the connection, the ASE
        /// configuration and the CIS, then drain queued audio onto the
        /// stream. Returns render-ready status JSON.
        pub fn tick(&mut self) -> Result<String, JsValue> {
            self.transport.pump(&self.channel)?;

            if !self.started && self.transport.is_open() {
                // Reset and both event masks, then the CIS host feature —
                // which must be declared before any connection exists, or LE
                // Create CIS is refused later for reasons that look unrelated.
                for packet in crate::device::host::init_commands().into_iter().take(3) {
                    self.channel.send_command(&packet[1..]).map_err(js_error)?;
                }
                for packet in crate::device::CisCentral::init_commands() {
                    self.channel.send_command(&packet[1..]).map_err(js_error)?;
                }
                self.started = true;
            }

            while let Some(packet) = self.channel.poll_controller_packet() {
                self.central.consume(&self.channel, &packet);
                for command in self.cis.on_packet(&packet) {
                    self.channel.send_command(&command[1..]).map_err(js_error)?;
                }
            }
            self.central.produce(&self.channel);

            self.advance_stream();
            self.drain_audio();
            self.transport.pump(&self.channel)?;
            Ok(self.status_json())
        }

        /// Drives the control plane: configure the endpoint once discovery
        /// finds it, then open the stream once those writes have landed.
        fn advance_stream(&mut self) {
            if !self.central.is_ready() {
                return;
            }
            if !self.ase_requested {
                let uuid = crate::profiles::ascs::ascs_uuid::ASE_CONTROL_POINT;
                let Some(control_point) = self.central.characteristic_handle(uuid) else {
                    self.error = Some(
                        "the peer has no ASE Control Point — it is not an LE Audio sink".into(),
                    );
                    self.ase_requested = true;
                    return;
                };
                // Queued together: the central sends one at a time and waits
                // for each response, so this is the ASCS order, not a burst.
                self.central
                    .queue_write(control_point, self.ase.config_codec());
                self.central
                    .queue_write(control_point, self.ase.config_qos());
                self.central.queue_write(control_point, self.ase.enable());
                self.ase_requested = true;
                return;
            }
            // The endpoint is Enabling once the writes have drained; only
            // then does opening a CIS mean anything.
            if !self.cis_requested && self.central.is_idle() && self.error.is_none() {
                let acl_handle = self.central.connection_handle();
                for command in self.cis.start(acl_handle) {
                    let _ = self.channel.send_command(&command[1..]);
                }
                self.cis_requested = true;
            }
        }

        /// Moves queued SDUs onto the stream once it will carry them.
        fn drain_audio(&mut self) {
            if !self.cis.is_streaming() {
                return;
            }
            while let Some(sdu) = self.pending_audio.pop_front() {
                match self.cis.send_sdu(&sdu) {
                    Some(packet) => {
                        let _ = self.channel.inject_host_packet(packet);
                    }
                    None => break,
                }
            }
        }

        /// What the page renders: where the handshake has got to, and why it
        /// stopped if it did.
        pub fn status_json(&self) -> String {
            let stage = if self.error.is_some() {
                "error"
            } else if self.cis.is_streaming() {
                "streaming"
            } else if self.cis_requested {
                "opening the stream"
            } else if self.ase_requested {
                "configuring the endpoint"
            } else if self.central.is_ready() {
                "discovered"
            } else if self.transport.is_open() {
                "connecting"
            } else {
                "offline"
            };
            format!(
                r#"{{"stage":"{}","streaming":{},"cis_handle":{},"queued":{},"dropped":{},"error":{}}}"#,
                stage,
                self.cis.is_streaming(),
                match self.cis.cis_handle() {
                    Some(handle) => handle.to_string(),
                    None => "null".to_string(),
                },
                self.pending_audio.len(),
                self.dropped,
                match &self.error {
                    Some(message) => format!("{message:?}"),
                    None => "null".to_string(),
                }
            )
        }
    }

    // -- Broadcast / Auracast ----------------------------------------------
    //
    // The connectionless media plane, both ends. Unlike every other pair on
    // this site these two devices never meet: there is no ACL, no GATT and no
    // pairing between them, so neither wrapper has a `target` and neither can
    // report anything about the other except what it heard on the air.
    //
    // netsim only. The in-page `Link` does not model periodic advertising or a
    // BIG, so there is nothing here for it to carry.

    /// Hex, space-separated — the form the pages already show wire bytes in.
    fn hex_bytes(bytes: &[u8]) -> String {
        bytes
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The names of the HCI statuses these two devices can actually produce.
    /// A page showing "0x1D" and nothing else makes the reader look it up; the
    /// interesting failures here (a source that is encrypted, a BIG that never
    /// established) deserve to say what they are.
    fn hci_status_name(status: u8) -> &'static str {
        match status {
            0x02 => "Unknown Connection Identifier",
            0x07 => "Memory Capacity Exceeded",
            0x0C => "Command Disallowed",
            0x11 => "Unsupported Feature or Parameter Value",
            0x12 => "Invalid HCI Command Parameters",
            // What a receiver is told when the source tears its BIG down: the
            // BIG Sync Lost event carries the terminating side's reason.
            0x13 => "Remote User Terminated Connection",
            0x14 => "Remote Device Terminated due to Low Resources",
            0x15 => "Remote Device Terminated due to Power Off",
            0x16 => "Connection Terminated by Local Host",
            0x1A => "Unsupported Remote Feature",
            0x1D => "Insufficient Security",
            0x1E => "Parameter Out of Mandatory Range",
            0x22 => "LMP/LL Response Timeout",
            0x3D => "Connection Terminated due to MIC Failure",
            0x3E => "Connection Failed to be Established",
            0x42 => "Unknown Advertising Identifier",
            0x43 => "Limit Reached",
            0x44 => "Operation Cancelled by Host",
            0x45 => "Packet Too Long",
            0xFF => "malformed event",
            _ => "unknown status",
        }
    }

    /// One BASE, rendered the same way whichever end of the broadcast is
    /// holding it. The Broadcast page puts the source's copy beside the
    /// receiver's and compares them field by field, which only means anything
    /// if both were serialized by the same code.
    fn base_json(base: &crate::profiles::bap::BasicAudioAnnouncement) -> serde_json::Value {
        use crate::profiles::bap;
        let subgroups: Vec<serde_json::Value> = base
            .subgroups
            .iter()
            .map(|subgroup| {
                let config = &subgroup.codec_specific_configuration;
                let bis: Vec<serde_json::Value> = subgroup
                    .bis
                    .iter()
                    .map(|bis| {
                        let location = bis.codec_specific_configuration.audio_channel_allocation;
                        serde_json::json!({
                            "index": bis.index,
                            "audio_location": location,
                            "location_name": location.map(bap::audio_location::describe),
                        })
                    })
                    .collect();
                let metadata: Vec<serde_json::Value> = bap::describe_metadata(&subgroup.metadata)
                    .into_iter()
                    .map(|(name, value)| serde_json::json!({ "name": name, "value": value }))
                    .collect();
                serde_json::json!({
                    "codec_id": hex_bytes(&subgroup.codec_id),
                    "codec_name": if subgroup.codec_id == bap::LC3_CODEC_ID {
                        "LC3"
                    } else {
                        "not LC3"
                    },
                    "sampling_frequency_hz": config.sampling_frequency.map(|f| f.hz()),
                    "frame_duration_us": config.frame_duration.map(|d| d.us()),
                    "octets_per_codec_frame": config.octets_per_codec_frame,
                    "codec_frames_per_sdu": config.codec_frames_per_sdu,
                    "metadata_hex": hex_bytes(&subgroup.metadata),
                    "metadata": metadata,
                    "bis": bis,
                })
            })
            .collect();
        serde_json::json!({
            "presentation_delay": base.presentation_delay,
            "subgroups": subgroups,
        })
    }

    /// A Broadcast_Code from what the page's text field holds: 16 octets,
    /// left-justified, zero-padded (BAP 3.7.1). Refused rather than truncated
    /// if it is too long — silently dropping the tail would produce a code
    /// that works nowhere and looks right everywhere.
    fn broadcast_code(code: Option<String>) -> Result<Option<[u8; 16]>, JsValue> {
        let Some(code) = code.filter(|c| !c.is_empty()) else {
            return Ok(None);
        };
        let bytes = code.as_bytes();
        if bytes.len() > 16 {
            return Err(js_error(format!(
                "a Broadcast Code is at most 16 octets; \"{code}\" is {}",
                bytes.len()
            )));
        }
        let mut padded = [0u8; 16];
        padded[..bytes.len()].copy_from_slice(bytes);
        Ok(Some(padded))
    }

    /// An **Auracast broadcast source** on netsim: an extended advertising set
    /// carrying the Broadcast Audio Announcement, a periodic train carrying the
    /// BASE, and a BIG whose BISes this page writes LC3 into.
    ///
    /// Wraps [`BigBroadcaster`](crate::device::BigBroadcaster), which is
    /// transport-free, so this type is only the browser's WebSocket, a running
    /// order, and the status a page renders. The interop scripts in
    /// `tests/interop/` drive the same device against Bumble.
    #[wasm_bindgen]
    pub struct WebBigBroadcaster {
        transport: WasmWsTransport,
        channel: HciChannel,
        broadcaster: crate::device::BigBroadcaster,
        started: bool,
        /// SDUs accepted per BIS — the count the page reports as "sent".
        sent: u64,
        /// SDUs refused because the BIG was not streaming yet.
        refused: u64,
    }

    #[wasm_bindgen]
    impl WebBigBroadcaster {
        /// Creates a source that will publish `broadcast_id` under
        /// `broadcast_name` on `num_bis` streams. A non-empty `code` encrypts
        /// the BISes, which a receiver then needs the same code to join.
        #[wasm_bindgen(constructor)]
        pub fn new(
            url: &str,
            broadcast_id: u32,
            broadcast_name: &str,
            num_bis: u8,
            code: Option<String>,
        ) -> Result<WebBigBroadcaster, JsValue> {
            install_panic_hook();
            if num_bis == 0 || num_bis > 4 {
                return Err(js_error("a BIG here carries between one and four BISes"));
            }
            let config = crate::device::BroadcastConfig {
                broadcast_id: broadcast_id & 0x00FF_FFFF,
                broadcast_name: broadcast_name.to_string(),
                num_bis,
                broadcast_code: broadcast_code(code)?,
                ..Default::default()
            };
            let url = ws_url_with_wire_address(url);
            Ok(Self {
                transport: WasmWsTransport::connect(&url)?,
                channel: HciChannel::new(),
                broadcaster: crate::device::BigBroadcaster::new(config),
                started: false,
                sent: 0,
                refused: 0,
            })
        }

        /// Returns the underlying WebSocket ready state (per the WebSocket API).
        pub fn ready_state(&self) -> u16 {
            self.transport.ready_state()
        }

        /// True once SDUs written here go out over the air.
        pub fn is_streaming(&self) -> bool {
            self.broadcaster.is_streaming()
        }

        /// Writes one SDU to BIS `bis_index` (1-based, as in the BASE).
        /// Returns whether it went out: before the data paths are open the
        /// controller would drop it, so it is refused and counted instead.
        ///
        /// There is deliberately no queue. A broadcaster has no peer to wait
        /// for — the BIG is up or it is not — and audio held back while a
        /// throttled tab catches up would be played late to every receiver at
        /// once.
        pub fn send_sdu(&mut self, bis_index: u8, sdu: Vec<u8>) -> bool {
            match self.broadcaster.send_sdu(bis_index, &sdu) {
                Some(packet) => {
                    let _ = self.channel.inject_host_packet(packet);
                    if bis_index == 1 {
                        self.sent += 1;
                    }
                    true
                }
                None => {
                    if bis_index == 1 {
                        self.refused += 1;
                    }
                    false
                }
            }
        }

        /// Tears the BIG down. The advertising set stays up until this device
        /// is dropped, which is also what stops the periodic train.
        pub fn terminate(&mut self) {
            let _ = self
                .channel
                .inject_host_packet(self.broadcaster.terminate());
            let _ = self.transport.pump(&self.channel);
        }

        /// One pump: bring the controller up, advance the setup sequence, and
        /// return render-ready status JSON.
        pub fn tick(&mut self) -> Result<String, JsValue> {
            self.transport.pump(&self.channel)?;

            if !self.started && self.transport.is_open() {
                // Reset and both event masks. The post-Reset default event mask
                // excludes LE Meta Events, so LE Create BIG Complete — the only
                // announcement of the BIS handles — would never arrive.
                for packet in crate::device::host::init_commands().into_iter().take(3) {
                    self.channel.send_command(&packet[1..]).map_err(js_error)?;
                }
                for packet in self.broadcaster.start() {
                    self.channel.inject_host_packet(packet).map_err(js_error)?;
                }
                self.started = true;
            }

            while let Some(packet) = self.channel.poll_controller_packet() {
                for command in self.broadcaster.on_packet(&packet) {
                    self.channel.inject_host_packet(command).map_err(js_error)?;
                }
            }
            self.transport.pump(&self.channel)?;
            Ok(self.status_json())
        }

        /// Everything the page renders, including the two payloads this source
        /// puts on the air: the advertising data a scanner sees and the BASE
        /// the periodic train carries.
        pub fn status_json(&self) -> String {
            use crate::device::BroadcastState;
            let state = self.broadcaster.state();
            let config = self.broadcaster.config();
            let (stage, failed) = match state {
                BroadcastState::Idle if !self.transport.is_open() => ("offline", None),
                BroadcastState::Idle => ("starting", None),
                BroadcastState::SettingAdvertisingParameters
                | BroadcastState::SettingAdvertisingData => ("advertising set", None),
                BroadcastState::SettingPeriodicParameters | BroadcastState::SettingPeriodicData => {
                    ("periodic train", None)
                }
                BroadcastState::EnablingAdvertising
                | BroadcastState::EnablingPeriodicAdvertising => ("on the air", None),
                BroadcastState::CreatingBig => ("creating the BIG", None),
                BroadcastState::OpeningDataPaths => ("opening data paths", None),
                BroadcastState::Streaming => ("streaming", None),
                BroadcastState::Terminated => ("terminated", None),
                BroadcastState::Failed(status) => ("failed", Some(status)),
            };
            let value = serde_json::json!({
                "stage": stage,
                "state": format!("{state:?}"),
                "streaming": self.broadcaster.is_streaming(),
                "failed": failed,
                "failed_name": failed.map(hci_status_name),
                "bis_handles": self.broadcaster.bis_handles(),
                "sent": self.sent,
                "refused": self.refused,
                "config": {
                    "broadcast_id": config.broadcast_id,
                    "broadcast_name": config.broadcast_name,
                    "advertising_sid": config.advertising_sid,
                    "num_bis": config.num_bis,
                    "max_sdu": config.max_sdu,
                    "sdu_interval_us": config.sdu_interval_us,
                    "rtn": config.rtn,
                    "max_transport_latency_ms": config.max_transport_latency_ms,
                    "phy": config.phy,
                    "encrypted": config.broadcast_code.is_some(),
                    "sampling_frequency_hz": config.sampling_frequency.hz(),
                    "frame_duration_us": config.frame_duration.us(),
                    "octets_per_codec_frame": config.octets_per_codec_frame,
                },
                // The two payloads, exactly as they go out. The BASE's octets
                // are what a receiver reassembles off the periodic train, so
                // the page can compare the two strings directly.
                "advertising_data": hex_bytes(&config.advertising_data()),
                "base_hex": hex_bytes(&config.base().to_bytes()),
                "base": base_json(&config.base()),
            });
            value.to_string()
        }
    }

    /// An **Auracast broadcast sink** on netsim: scans for a Broadcast Audio
    /// Announcement, syncs to the source's periodic train, reads the BASE and
    /// the BIGInfo off it, joins the BIG and collects SDUs per BIS.
    ///
    /// Wraps [`BigReceiver`](crate::device::BigReceiver). Several of these can
    /// exist at once against one source and none of them tells the source
    /// anything — that is what makes it a broadcast.
    #[wasm_bindgen]
    pub struct WebBigReceiver {
        transport: WasmWsTransport,
        channel: HciChannel,
        receiver: crate::device::BigReceiver,
        started: bool,
        /// Whether scanning has been turned off after joining. rootcanal keeps
        /// delivering every advertisement in the simulation otherwise, which on
        /// a page with several receivers is most of the traffic.
        scanning_stopped: bool,
        /// One queue of undelivered SDUs per BIS slot, in BIS index order.
        audio: Vec<VecDeque<Vec<u8>>>,
        /// SDUs received per BIS slot, counted before any bound is applied.
        counts: Vec<u64>,
        /// SDUs dropped because the page did not collect them in time.
        dropped: u64,
    }

    /// How much undelivered audio one BIS may hold — about two seconds at a
    /// 10 ms SDU interval. A hidden tab's timer runs at 1 Hz while the stream
    /// keeps arriving at 100 SDUs a second, so without a bound the queue is
    /// unbounded memory; with one, what it discards is counted rather than
    /// quietly lost.
    const RECEIVER_QUEUE_LIMIT: usize = 200;

    #[wasm_bindgen]
    impl WebBigReceiver {
        /// Creates a receiver. `broadcast_id` filters which source to join —
        /// omit it to take the first Auracast broadcast seen. `code` is the
        /// Broadcast Code for an encrypted source.
        #[wasm_bindgen(constructor)]
        pub fn new(
            url: &str,
            broadcast_id: Option<u32>,
            code: Option<String>,
        ) -> Result<WebBigReceiver, JsValue> {
            install_panic_hook();
            let config = crate::device::ReceiverConfig {
                broadcast_id: broadcast_id.map(|id| id & 0x00FF_FFFF),
                broadcast_code: broadcast_code(code)?,
                ..Default::default()
            };
            let url = ws_url_with_wire_address(url);
            Ok(Self {
                transport: WasmWsTransport::connect(&url)?,
                channel: HciChannel::new(),
                receiver: crate::device::BigReceiver::new(config),
                started: false,
                scanning_stopped: false,
                audio: Vec::new(),
                counts: Vec::new(),
                dropped: 0,
            })
        }

        /// Returns the underlying WebSocket ready state (per the WebSocket API).
        pub fn ready_state(&self) -> u16 {
            self.transport.ready_state()
        }

        /// True once SDUs are arriving.
        pub fn is_receiving(&self) -> bool {
            self.receiver.is_receiving()
        }

        /// Drains the SDUs received on BIS `bis_index` (1-based, as in the
        /// BASE), as an array of byte arrays.
        ///
        /// Per BIS, not merged: LC3 carries decoder state between frames, so
        /// two streams through one decoder corrupt both — and on this page the
        /// two BISes are the left and right of a stereo pair, which is only
        /// audible if they stay apart.
        pub fn take_audio(&mut self, bis_index: u8) -> js_sys::Array {
            let out = js_sys::Array::new();
            if bis_index == 0 {
                return out;
            }
            let Some(queue) = self.audio.get_mut(usize::from(bis_index - 1)) else {
                return out;
            };
            for sdu in queue.drain(..) {
                out.push(&js_sys::Uint8Array::from(&sdu[..]).into());
            }
            out
        }

        /// Leaves the BIG. The device stays on the air and keeps its periodic
        /// sync, so the page can show a receiver that has stopped listening
        /// without pretending it left the room.
        pub fn terminate(&mut self) {
            let _ = self.channel.inject_host_packet(self.receiver.terminate());
            let _ = self.transport.pump(&self.channel);
        }

        /// One pump: advance the synchronization, collect whatever audio
        /// arrived, and return render-ready status JSON.
        pub fn tick(&mut self) -> Result<String, JsValue> {
            self.transport.pump(&self.channel)?;

            if !self.started && self.transport.is_open() {
                for packet in crate::device::host::init_commands().into_iter().take(3) {
                    self.channel.send_command(&packet[1..]).map_err(js_error)?;
                }
                for packet in self.receiver.start() {
                    self.channel.inject_host_packet(packet).map_err(js_error)?;
                }
                self.started = true;
            }

            while let Some(packet) = self.channel.poll_controller_packet() {
                for command in self.receiver.on_packet(&packet) {
                    self.channel.inject_host_packet(command).map_err(js_error)?;
                }
            }

            // Once the BIG is joined there is nothing left to look for.
            if self.receiver.is_receiving() && !self.scanning_stopped {
                self.channel
                    .inject_host_packet(self.receiver.stop_scanning())
                    .map_err(js_error)?;
                self.scanning_stopped = true;
            }

            let slots = self.receiver.bis_handles().len();
            if self.audio.len() != slots {
                self.audio.resize_with(slots, VecDeque::new);
                self.counts.resize(slots, 0);
            }
            while let Some(sdu) = self.receiver.poll_sdu() {
                let Some(slot) = self
                    .receiver
                    .bis_handles()
                    .iter()
                    .position(|&handle| handle == sdu.handle)
                else {
                    continue;
                };
                self.counts[slot] += 1;
                let queue = &mut self.audio[slot];
                queue.push_back(sdu.payload);
                while queue.len() > RECEIVER_QUEUE_LIMIT {
                    queue.pop_front();
                    self.dropped += 1;
                }
            }

            self.transport.pump(&self.channel)?;
            Ok(self.status_json())
        }

        /// Everything the page renders: where synchronization has got to, the
        /// source it found, the BASE it read back, the BIGInfo the controller
        /// reported, and the per-BIS counts.
        pub fn status_json(&self) -> String {
            use crate::device::ReceiverState;
            let state = self.receiver.state();
            let (stage, failed) = match state {
                ReceiverState::Idle if !self.transport.is_open() => ("offline", None),
                ReceiverState::Idle | ReceiverState::SettingScanParameters => ("starting", None),
                ReceiverState::Scanning => ("scanning", None),
                ReceiverState::SyncingToPeriodicAdvertising => ("syncing to the train", None),
                ReceiverState::WaitingForAnnouncement => ("reading the announcement", None),
                ReceiverState::SyncingToBig => ("joining the BIG", None),
                ReceiverState::OpeningDataPaths => ("opening data paths", None),
                ReceiverState::Receiving => ("receiving", None),
                ReceiverState::Terminated => ("left the BIG", None),
                ReceiverState::Lost(reason) => ("lost", Some(reason)),
                ReceiverState::Failed(status) => ("failed", Some(status)),
            };
            let source = self.receiver.found().map(|found| {
                serde_json::json!({
                    "address": Address::new(found.address).to_string(),
                    "address_type": super::address_type_name(found.address_type),
                    "advertising_sid": found.advertising_sid,
                    "broadcast_id": found.broadcast_id,
                })
            });
            let big_info = self.receiver.big_info().map(|info| {
                serde_json::json!({
                    "num_bis": info.num_bis,
                    "nse": info.nse,
                    "iso_interval": info.iso_interval.get(),
                    "bn": info.bn,
                    "pto": info.pto,
                    "irc": info.irc,
                    "max_pdu": info.max_pdu.get(),
                    "sdu_interval_us": info.sdu_interval.get(),
                    "max_sdu": info.max_sdu.get(),
                    "phy": info.phy,
                    "framing": info.framing,
                    "encrypted": info.encryption != 0,
                })
            });
            let handles = self.receiver.bis_handles();
            let streams: Vec<serde_json::Value> = handles
                .iter()
                .enumerate()
                .map(|(slot, &handle)| {
                    serde_json::json!({
                        "index": slot + 1,
                        "handle": handle,
                        "sdus": self.counts.get(slot).copied().unwrap_or(0),
                        "queued": self.audio.get(slot).map(VecDeque::len).unwrap_or(0),
                    })
                })
                .collect();
            let value = serde_json::json!({
                "stage": stage,
                "state": format!("{state:?}"),
                "receiving": self.receiver.is_receiving(),
                "failed": failed,
                "failed_name": failed.map(hci_status_name),
                "source": source,
                "sync_handle": self.receiver.sync_handle(),
                "base": self.receiver.base().map(base_json),
                // The octets as they arrived, for comparison with the source's.
                "base_hex": self.receiver.base_bytes().map(hex_bytes),
                "big_info": big_info,
                "streams": streams,
                "sdu_count": self.receiver.sdu_count(),
                "dropped": self.dropped,
            });
            value.to_string()
        }
    }

    // -- end Broadcast -----------------------------------------------------

    /// A **scripted central on netsim**: the client half of
    /// [`WebPeripheral`], driven by a Rhai script.
    ///
    /// `ScriptedCentral` is transport-free -- H4 packets in, H4 packets out --
    /// so nothing about it assumed the in-page link it was first wired to.
    /// This is the same wrapper `WebPeripheral` is, over the same
    /// `WasmWsTransport` + `HciChannel` pair that `WebSource` already uses to
    /// put a central on netsim. It exists so a scripted client can face a real
    /// controller, and an Android emulator, rather than only its own scene.
    #[wasm_bindgen]
    pub struct WebScriptedCentral {
        transport: WasmWsTransport,
        channel: HciChannel,
        central: crate::scripting::ScriptedCentral,
        started: bool,
    }

    #[wasm_bindgen]
    impl WebScriptedCentral {
        /// Connects a scripted central to netsim at `url` and runs `script`.
        #[wasm_bindgen(constructor)]
        pub fn new(url: &str, script: &str) -> Result<WebScriptedCentral, JsValue> {
            install_panic_hook();
            let central =
                crate::scripting::ScriptedCentral::run_script(script).map_err(js_error)?;
            // netsim reads the URL address LSB-first; connect with the wire
            // form so this device lands on the air where the page says it is.
            let url = ws_url_with_wire_address(url);
            Ok(Self {
                transport: WasmWsTransport::connect(&url)?,
                channel: HciChannel::new(),
                central,
                started: false,
            })
        }

        /// Returns the underlying WebSocket ready state (per the WebSocket API).
        pub fn ready_state(&self) -> u16 {
            self.transport.ready_state()
        }

        /// Aims the script at `address`, overriding whatever it connected to.
        pub fn set_target(&mut self, address: &str) -> Result<(), JsValue> {
            let target: Address = address
                .parse()
                .map_err(|_| JsValue::from_str("target is not a Bluetooth address"))?;
            self.central.set_target(target);
            Ok(())
        }

        /// The first `assert(...)` failure, which is the run's verdict.
        pub fn failure(&self) -> Option<String> {
            self.central.failure().map(str::to_string)
        }

        /// Queues a read of `uuid`, as the in-page `scripted_central_read`
        /// does. A page drives the script rather than replacing it: the
        /// request joins the same outbox the script's own calls use.
        pub fn read(&mut self, uuid: &str) -> Result<(), JsValue> {
            let uuid: crate::types::Uuid =
                uuid.parse().map_err(|_| JsValue::from_str("bad UUID"))?;
            self.central.read(uuid);
            Ok(())
        }

        /// Queues a write of `value` to `uuid`.
        pub fn write(
            &mut self,
            uuid: &str,
            value: Vec<u8>,
            with_response: bool,
        ) -> Result<(), JsValue> {
            let uuid: crate::types::Uuid =
                uuid.parse().map_err(|_| JsValue::from_str("bad UUID"))?;
            self.central.write(uuid, value, with_response);
            Ok(())
        }

        /// Queues enabling or disabling notifications on `uuid`.
        pub fn subscribe(&mut self, uuid: &str, enable: bool) -> Result<(), JsValue> {
            let uuid: crate::types::Uuid =
                uuid.parse().map_err(|_| JsValue::from_str("bad UUID"))?;
            self.central.subscribe(uuid, enable);
            Ok(())
        }

        /// Messages the script emitted since the last call.
        pub fn emitted(&mut self) -> js_sys::Array {
            let out = js_sys::Array::new();
            for message in self.central.take_emitted() {
                out.push(&JsValue::from_str(&message));
            }
            out
        }

        /// One pump: bring the controller up, hand it whatever the script
        /// produced, and return the client's status JSON.
        pub fn tick(&mut self, t_seconds: f64) -> Result<String, JsValue> {
            self.transport.pump(&self.channel)?;

            if !self.started && self.transport.is_open() {
                // Reset and both event masks. The post-Reset default event mask
                // excludes LE Meta Events, so nothing would ever report a
                // connection completing.
                for packet in crate::device::host::init_commands().into_iter().take(3) {
                    self.channel.send_command(&packet[1..]).map_err(js_error)?;
                }
                // The script ran at construction, so its connect is already
                // waiting in the outbox.
                self.drain()?;
                self.started = true;
            }

            while let Some(packet) = self.channel.poll_controller_packet() {
                for out in self.central.on_packet(&packet) {
                    self.channel.inject_host_packet(out).map_err(js_error)?;
                }
            }
            for out in self.central.tick(t_seconds) {
                self.channel.inject_host_packet(out).map_err(js_error)?;
            }
            self.drain()?;

            self.transport.pump(&self.channel)?;
            Ok(self.central.status_json())
        }

        /// Moves anything the script queued outside a packet callback -- a
        /// read or subscribe issued from the page -- onto the wire.
        fn drain(&mut self) -> Result<(), JsValue> {
            for packet in self.central.take_outbox() {
                self.channel.inject_host_packet(packet).map_err(js_error)?;
            }
            Ok(())
        }
    }

    /// The API Explorer's engine: a live [`ScriptedPeripheral`] session built
    /// one Rhai statement at a time. Each Execute in the page calls
    /// [`WebSession::eval_line`] with the single generated line; the session
    /// scope persists across calls (so `svc1`, `chr1`, … stay usable), and the
    /// device is hosted on netsim as soon as a server exists. netsim is
    /// optional — building and inspecting a device works fully offline; the
    /// socket only carries advertising/notifications when it's reachable.
    #[wasm_bindgen]
    pub struct WebSession {
        transport: WasmWsTransport,
        channel: HciChannel,
        peripheral: ScriptedPeripheral,
        started: bool,
        adv_signature: String,
    }

    #[wasm_bindgen]
    impl WebSession {
        #[wasm_bindgen(constructor)]
        pub fn new(url: &str) -> Result<WebSession, JsValue> {
            install_panic_hook();
            Ok(Self {
                transport: WasmWsTransport::connect(url)?,
                channel: HciChannel::new(),
                peripheral: ScriptedPeripheral::new_session(),
                started: false,
                adv_signature: String::new(),
            })
        }

        /// Returns the underlying WebSocket ready state (per the WebSocket API).
        pub fn ready_state(&self) -> u16 {
            self.transport.ready_state()
        }

        /// Evaluates one Rhai line in the persistent session scope and returns
        /// the JSON result (`ok`, `value`, `error`, `events`). Works whether or
        /// not netsim is connected — this only touches the in-page engine.
        pub fn eval_line(&mut self, line: &str) -> String {
            self.peripheral.eval_line_json(line)
        }

        /// One pump + host tick. Once a server exists it advertises (re-issuing
        /// the bring-up whenever the built device's name/services change),
        /// handles connections, and flushes value-change notifications, exactly
        /// like [`WebPeripheral`]. Returns the peripheral status JSON.
        pub fn tick(&mut self, t_seconds: f64) -> Result<String, JsValue> {
            self.transport.pump(&self.channel)?;
            if self.peripheral.has_server() {
                let signature = self.peripheral.adv_signature();
                if signature != self.adv_signature {
                    // The built device changed — restart advertising so the new
                    // name/services go on the air (mirrors run_script's reset).
                    self.adv_signature = signature;
                    self.channel = HciChannel::new();
                    self.started = false;
                }
                if !self.started && self.transport.is_open() {
                    self.peripheral
                        .queue_start(&self.channel)
                        .map_err(js_error)?;
                    self.started = true;
                }
                while let Some(packet) = self.channel.poll_controller_packet() {
                    if let Err(e) = self.peripheral.handle_packet(&self.channel, &packet) {
                        self.peripheral.record_error(e.to_string());
                    }
                }
                if let Err(e) = self.peripheral.tick(&self.channel, t_seconds) {
                    self.peripheral.record_error(e.to_string());
                }
                self.transport.pump(&self.channel)?;
            }
            Ok(self.peripheral.status_json())
        }

        /// The current session's device status JSON (for the live viewer)
        /// without pumping the socket — used right after an Execute so the
        /// viewer reflects the new object immediately.
        pub fn status_json(&self) -> String {
            self.peripheral.status_json()
        }
    }

    // -- Car page: the hands-free car kit ---------------------------------
    //
    // Both endpoints live in one wasm object driven by one timer, because a
    // two-tab design silently stalls: Chrome throttles a hidden tab hard
    // enough that a device in one misses protocol deadlines.

    /// The Car page's engine: a phone and a head unit on one HFP link.
    /// Wraps [`CarKit`](crate::device::car_kit::CarKit), which needs no
    /// transport of its own — there is no WebSocket and no netsim here.
    #[wasm_bindgen]
    pub struct WebCarKit {
        kit: crate::device::car_kit::CarKit,
    }

    #[wasm_bindgen]
    impl WebCarKit {
        /// Creates the pair and starts the head unit reaching for the phone.
        #[wasm_bindgen(constructor)]
        pub fn new() -> WebCarKit {
            install_panic_hook();
            let mut kit = crate::device::car_kit::CarKit::new();
            kit.start();
            Self { kit }
        }

        /// One step of the link. `now_ms` is the page's clock.
        pub fn tick(&mut self, now_ms: f64) {
            self.kit.tick(now_ms.max(0.0) as u64);
        }

        /// Everything the page renders. `since_seq` selects the AT lines the
        /// page has not appended yet.
        pub fn status_json(&self, since_seq: f64) -> String {
            self.kit.status_json(since_seq.max(0.0) as u64)
        }

        /// Routes one UI action. `argument` is the number for `dial`, the
        /// operator name for `operator`, and the level for the gain and
        /// indicator commands; it is ignored otherwise. Returns whether the
        /// action was accepted, so the page can grey out what the link
        /// cannot do yet.
        pub fn command(&mut self, name: &str, argument: &str) -> bool {
            use crate::classic::hfp::AgIndicator;
            let level = || argument.parse::<u8>().unwrap_or(0);
            let value = || argument.parse::<u32>().unwrap_or(0);
            match name {
                "incoming" => self.kit.incoming_call(argument),
                "phone-dial" => self.kit.phone_dial(argument),
                "phone-end" => self.kit.phone_end_call(),
                "answer" => self.kit.answer(),
                "hangup" => self.kit.hang_up(),
                "car-dial" => self.kit.car_dial(argument),
                "speaker" => self.kit.set_speaker_gain(level()),
                "microphone" => self.kit.set_microphone_gain(level()),
                "mute" => self.kit.set_microphone_muted(argument == "1"),
                "voice" => self.kit.set_voice_recognition(argument == "1"),
                "calls" => self.kit.query_calls(),
                "service" => self.kit.set_indicator(AgIndicator::Service, value()),
                "signal" => self.kit.set_indicator(AgIndicator::Signal, value()),
                "battery" => self.kit.set_indicator(AgIndicator::BatteryCharge, value()),
                "roam" => self.kit.set_indicator(AgIndicator::Roam, value()),
                "operator" => {
                    self.kit.set_operator(argument);
                    true
                }
                _ => false,
            }
        }
    }

    impl Default for WebCarKit {
        fn default() -> Self {
            Self::new()
        }
    }

    // -- end Car page ------------------------------------------------------

    // -- USB speaker (Audio page, "USB dongle" controller) -------------------

    /// The other half: a Classic A2DP **source** on a second dongle,
    /// walking the same ladder `examples/a2dp_source.rs` climbs —
    /// [`crate::device::a2dp_source_runner::A2dpSourceRunner`] is that
    /// ladder, extracted — with the page supplying PCM and reading the log.
    /// Point it at a real speaker in pairing mode, or at this page's own
    /// [`WebA2dpSink`] on the other dongle for a full loop over real RF.
    #[wasm_bindgen]
    pub struct WebA2dpSource {
        transport: WasmWsTransport,
        channel: HciChannel,
        runner: crate::device::a2dp_source_runner::A2dpSourceRunner,
        inquiry_length: u8,
        log: Vec<String>,
        failure: Option<String>,
    }

    #[wasm_bindgen]
    impl WebA2dpSource {
        /// Connects to the bridge at `url` (with `?device=` picking the
        /// dongle). `target` is the speaker's address, or empty to inquire
        /// and take the first Audio/Video device that answers.
        #[wasm_bindgen(constructor)]
        pub fn new(url: &str, target: &str) -> Result<WebA2dpSource, JsValue> {
            use crate::device::a2dp_source_runner::A2dpSourceRunner;
            use crate::device::classic_host::{inquiry_mode, io_capability, scan_enable};
            use std::str::FromStr as _;

            install_panic_hook();
            let target = if target.trim().is_empty() {
                None
            } else {
                Some(Address::from_str(target.trim()).map_err(js_error)?)
            };
            let transport = WasmWsTransport::connect(url)?;
            let runner = A2dpSourceRunner::new(target, io_capability::NO_INPUT_NO_OUTPUT, true);
            let channel = HciChannel::new();
            for packet in runner
                .host()
                .start_commands()
                .into_iter()
                // A source is neither discoverable nor connectable: it does
                // the finding.
                .chain(runner.host().set_scan_enable(scan_enable::NONE))
                // Extended inquiry results carry the peer's name in EIR,
                // which is what the page's target picker lists.
                .chain(runner.host().set_inquiry_mode(inquiry_mode::WITH_EXTENDED))
            {
                channel.inject_host_packet(packet).map_err(js_error)?;
            }
            Ok(Self {
                transport,
                channel,
                runner,
                inquiry_length: 8,
                log: vec!["source up, waiting for the bridge socket".to_string()],
                failure: None,
            })
        }

        /// One pump plus one ladder step. `now_ms` is the worker's clock.
        pub fn tick(&mut self, now_ms: f64) {
            if self.failure.is_some() {
                return;
            }
            if let Err(e) = self.transport.pump(&self.channel) {
                self.fail(format!("bridge socket: {e:?}"));
                return;
            }
            while let Some(packet) = self.channel.poll_controller_packet() {
                match self.runner.handle_packet(&packet) {
                    Ok(outgoing) => {
                        for out in outgoing {
                            let _ = self.channel.inject_host_packet(out);
                        }
                    }
                    Err(e) => self.log.push(format!("host: {e}")),
                }
            }
            match self.runner.step(now_ms, self.inquiry_length) {
                Ok(packets) => {
                    for packet in packets {
                        let _ = self.channel.inject_host_packet(packet);
                    }
                }
                Err(e) => {
                    self.fail(e);
                    return;
                }
            }
            self.log.extend(self.runner.take_log());
            self.runner.feed(now_ms);
            for packet in self.runner.poll() {
                let _ = self.channel.inject_host_packet(packet);
            }
        }

        /// Interleaved stereo PCM at 44 100 Hz for the stream; the runner
        /// meters it out at real time.
        pub fn queue_pcm(&mut self, samples: &[i16]) {
            self.runner.queue_pcm(samples);
        }

        /// Samples queued and not yet sent — the page's low-water mark.
        pub fn pending_samples(&self) -> u32 {
            self.runner.pending_samples() as u32
        }

        /// Ends the run.
        pub fn finish(&mut self) {
            self.runner.finish();
        }

        /// Render-ready state; `since` counts log lines already rendered.
        pub fn status_json(&self, since: f64) -> String {
            let since = (since.max(0.0) as usize).min(self.log.len());
            serde_json::json!({
                "stage": self.runner.rung().label(),
                "highest": self.runner.highest().label(),
                "socket_open": self.transport.is_open(),
                "packets_sent": self.runner.packets_sent(),
                "negotiated": self.runner.negotiated(),
                "failure": self.failure,
                "discovered": self.runner.discovered().iter().map(|d| {
                    serde_json::json!({
                        "address": d.address.to_string(),
                        "name": d.name,
                        "class": u32::from_le_bytes([
                            d.class_of_device[0],
                            d.class_of_device[1],
                            d.class_of_device[2],
                            0,
                        ]),
                    })
                }).collect::<Vec<_>>(),
                "log": self.log[since..],
                "log_len": self.log.len(),
            })
            .to_string()
        }

        fn fail(&mut self, reason: String) {
            self.log.push(format!("FAIL: {reason}"));
            self.failure = Some(reason);
        }
    }

    /// A Classic A2DP sink over the `simble --usb` WebSocket bridge: the
    /// browser half of a *real* speaker. The bridge owns a physical dongle
    /// and relays raw HCI both ways, so the phone that pairs with this is a
    /// real phone on real radio — the same ladder `examples/a2dp_sink.rs`
    /// climbs natively, driven from a page that can actually play the PCM.
    ///
    /// The LE devices on the Audio page each own a netsim socket; this owns
    /// the bridge socket, which serves ONE client at a time — the page must
    /// not also point a scanner or a source at it.
    #[wasm_bindgen]
    pub struct WebA2dpSink {
        transport: WasmWsTransport,
        channel: HciChannel,
        host: crate::device::ClassicHost,
        /// Decoded interleaved PCM awaiting `take_pcm()`.
        pcm: Vec<i16>,
        decoded_frames: usize,
        undecodable_bytes: usize,
        /// Milestone lines, appended once each; the page renders `log[since..]`.
        log: Vec<String>,
        avdtp_reported: usize,
        /// Per-layer tallies, so a lossy run names the layer that lost:
        /// H4 packets in from the socket, split by type; media SDUs that
        /// reached the AVDTP handler; host-level parse rejections.
        events_in: usize,
        acl_in: usize,
        media_sdus: usize,
        host_errors: usize,
        /// RTP sequence tracking: packets that never arrived leave no bytes
        /// to count as undecodable — a click in the audio is their only
        /// trace unless the sequence numbers are watched.
        last_rtp_seq: Option<u16>,
        lost_packets: usize,
        said_connected: bool,
        said_paired: bool,
        said_encrypted: bool,
        // The same encryption dance the native example does, for the same
        // reason: a phone will not open an A2DP media channel on an
        // unencrypted link, and after a re-bond it does not always start
        // encryption itself. Authentication first — Set Connection
        // Encryption is only valid on an authenticated link.
        asked_for_authentication: bool,
        asked_for_encryption: bool,
        saw_authentication_complete: bool,
        failure: Option<String>,
    }

    #[wasm_bindgen]
    impl WebA2dpSink {
        /// Connects to the bridge at `url` (e.g. `ws://127.0.0.1:32323/`)
        /// and queues the whole bring-up: reset, event masks, name, Class of
        /// Device `0x240414` (Loudspeaker), SSP, inquiry + page scan, and an
        /// EIR carrying `name` and the Audio Sink service class. The queue
        /// drains once the socket opens.
        /// `keys_json` restores bonds from an earlier life of this sink:
        /// `[{"peer":"AA:BB:..","key":"32 hex chars","key_type":4}, …]`. A
        /// key store that died with the page made every reload a stranger
        /// to the phone that kept its half — an endless pair-again loop.
        #[wasm_bindgen(constructor)]
        pub fn new(url: &str, name: &str, keys_json: &str) -> Result<WebA2dpSink, JsValue> {
            use crate::classic::a2dp::make_audio_sink_service_sdp_records;
            use crate::classic::sdp::SdpServer;
            use crate::device::a2dp::A2dpSink;
            use crate::device::classic_host::{
                LinkKey, authentication_requirements, io_capability, scan_enable,
            };
            use crate::device::{ClassicHost, SdpHandler};
            use std::str::FromStr as _;

            install_panic_hook();
            const AUDIO_SINK_SERVICE_UUID: u16 = 0x110B;
            const SINK_SERVICE_RECORD_HANDLE: u32 = 0x0001_000B;

            let transport = WasmWsTransport::connect(url)?;
            let mut host = ClassicHost::new(name, [0x14, 0x04, 0x24]);
            // A speaker has no display and no keypad; claiming otherwise
            // escalates SSP to Numeric Comparison against a box that cannot
            // show the number.
            host.set_io_capability(
                io_capability::NO_INPUT_NO_OUTPUT,
                authentication_requirements::GENERAL_BONDING,
            );
            let mut sdp = SdpHandler::new(SdpServer::new());
            sdp.server_mut().service_records.insert(
                SINK_SERVICE_RECORD_HANDLE,
                make_audio_sink_service_sdp_records(SINK_SERVICE_RECORD_HANDLE, None),
            );
            host.register_handler(Box::new(sdp)).map_err(js_error)?;
            host.register_handler(Box::new(A2dpSink::new()))
                .map_err(js_error)?;
            if !keys_json.trim().is_empty()
                && let Ok(entries) = serde_json::from_str::<Vec<serde_json::Value>>(keys_json)
            {
                for entry in entries {
                    let (Some(peer), Some(hex), Some(key_type)) = (
                        entry["peer"].as_str(),
                        entry["key"].as_str(),
                        entry["key_type"].as_u64(),
                    ) else {
                        continue;
                    };
                    let Ok(peer) = Address::from_str(peer) else {
                        continue;
                    };
                    let mut value = [0u8; 16];
                    if hex.len() == 32
                        && (0..16).all(|i| {
                            u8::from_str_radix(&hex[2 * i..2 * i + 2], 16)
                                .map(|b| {
                                    value[i] = b;
                                    true
                                })
                                .unwrap_or(false)
                        })
                    {
                        host.insert_link_key(
                            peer,
                            LinkKey {
                                value,
                                key_type: key_type as u8,
                            },
                        );
                    }
                }
            }

            let channel = HciChannel::new();
            for packet in host
                .start_commands()
                .into_iter()
                .chain(host.set_scan_enable(scan_enable::INQUIRY_AND_PAGE))
                .chain(host.set_extended_inquiry_response(name, &[AUDIO_SINK_SERVICE_UUID]))
            {
                channel.inject_host_packet(packet).map_err(js_error)?;
            }
            Ok(Self {
                transport,
                channel,
                host,
                pcm: Vec::new(),
                decoded_frames: 0,
                undecodable_bytes: 0,
                log: vec![format!(
                    "sink up as {name:?}, waiting for the bridge socket"
                )],
                avdtp_reported: 0,
                events_in: 0,
                acl_in: 0,
                media_sdus: 0,
                host_errors: 0,
                last_rtp_seq: None,
                lost_packets: 0,
                said_connected: false,
                said_paired: false,
                said_encrypted: false,
                asked_for_authentication: false,
                asked_for_encryption: false,
                saw_authentication_complete: false,
                failure: None,
            })
        }

        /// One pump of both directions plus the security drive. Call from
        /// the page's timer.
        pub fn tick(&mut self) {
            use crate::classic::avdtp::AvdtpEvent;
            use crate::device::a2dp::A2dpSink;

            if self.failure.is_some() {
                return;
            }
            if let Err(e) = self.transport.pump(&self.channel) {
                self.fail(format!("bridge socket: {e:?}"));
                return;
            }
            while let Some(packet) = self.channel.poll_controller_packet() {
                match packet.first() {
                    Some(&0x04) => self.events_in += 1,
                    Some(&0x02) => self.acl_in += 1,
                    _ => {}
                }
                // Authentication Complete (Vol 4, Part E, 7.7.6): the link
                // key was actually used, as opposed to merely existing.
                if packet.len() > 2 && packet[0] == 0x04 && packet[1] == 0x06 {
                    self.saw_authentication_complete = true;
                }
                match self.host.handle_packet(&packet) {
                    Ok(outgoing) => {
                        for out in outgoing {
                            let _ = self.channel.inject_host_packet(out);
                        }
                    }
                    Err(e) => {
                        self.host_errors += 1;
                        self.log.push(format!("host: {e}"));
                    }
                }
            }
            for packet in self.host.poll() {
                let _ = self.channel.inject_host_packet(packet);
            }

            if !self.said_connected
                && let Some((handle, peer)) = self.host.connection()
            {
                self.log.push(format!(
                    "the phone paged us; ACL up with {peer} on handle {handle:#06x}"
                ));
                self.said_connected = true;
            }
            let security = self.host.security();
            if !self.said_paired {
                if let Some(status) = security.pairing_status.filter(|s| *s != 0x00) {
                    self.fail(format!(
                        "pairing failed: Simple Pairing Complete status {status:#04x}"
                    ));
                    return;
                }
                if security.authenticated {
                    self.log.push("bonded".to_string());
                    self.said_paired = true;
                }
            }
            if self.said_paired && !self.asked_for_authentication {
                self.asked_for_authentication = true;
                for packet in self.host.authenticate() {
                    let _ = self.channel.inject_host_packet(packet);
                }
            }
            if self.saw_authentication_complete && !self.asked_for_encryption {
                self.asked_for_encryption = true;
                for packet in self.host.encrypt(true) {
                    let _ = self.channel.inject_host_packet(packet);
                }
            }
            if !self.said_encrypted && self.host.security().encrypted {
                self.log.push("encrypted".to_string());
                self.said_encrypted = true;
            }

            let Some(sink) = self.host.handler_mut::<A2dpSink>() else {
                self.fail("the sink handler vanished".to_string());
                return;
            };
            while self.avdtp_reported < sink.events().len() {
                let line = match &sink.events()[self.avdtp_reported] {
                    AvdtpEvent::StreamConfigured { seid } => format!("SEID {seid} configured"),
                    AvdtpEvent::StreamOpened { seid } => format!("SEID {seid} open"),
                    AvdtpEvent::StreamStarted { seid } => format!("SEID {seid} streaming"),
                    AvdtpEvent::StreamSuspended { seid } => format!("SEID {seid} suspended"),
                    AvdtpEvent::StreamClosed { seid } => format!("SEID {seid} closed"),
                    other => format!("{other:?}"),
                };
                self.log.push(format!("avdtp: {line}"));
                self.avdtp_reported += 1;
            }
            let frames = sink.take_frames();
            if !frames.is_empty() {
                self.media_sdus += frames.len();
                for frame in &frames {
                    if let Some(last) = self.last_rtp_seq {
                        let gap = frame.sequence_number.wrapping_sub(last).wrapping_sub(1);
                        // A retransmission or wrap glitch looks like a huge
                        // "gap"; count only plausible runs of loss.
                        if gap > 0 && gap < 1000 {
                            self.lost_packets += gap as usize;
                        }
                    }
                    self.last_rtp_seq = Some(frame.sequence_number);
                }
                let audio = A2dpSink::decode(&frames);
                self.decoded_frames += audio.frames;
                self.undecodable_bytes += audio.undecodable_bytes;
                self.pcm.extend_from_slice(&audio.pcm);
            }
        }

        /// The decoded samples that arrived since the last call, interleaved
        /// `i16` at [`Self::sample_rate`] × [`Self::channels`]. The page owns
        /// playback: a wasm module cannot start an `AudioContext`.
        pub fn take_pcm(&mut self) -> Vec<i16> {
            std::mem::take(&mut self.pcm)
        }

        /// The negotiated sampling rate in Hz, or 0 before Set_Configuration.
        pub fn sample_rate(&self) -> u32 {
            use crate::classic::a2dp::sbc::sampling_frequency as sf;
            let Some(configuration) = self.configuration() else {
                return 0;
            };
            match configuration.sampling_frequency {
                x if x == sf::SF_16000 => 16000,
                x if x == sf::SF_32000 => 32000,
                x if x == sf::SF_44100 => 44100,
                x if x == sf::SF_48000 => 48000,
                _ => 0,
            }
        }

        /// Channels in the interleaved PCM: 1 for mono, 2 otherwise, 0
        /// before configuration.
        pub fn channels(&self) -> u32 {
            use crate::classic::a2dp::sbc::channel_mode;
            match self.configuration() {
                None => 0,
                Some(c) if c.channel_mode == channel_mode::MONO => 1,
                Some(_) => 2,
            }
        }

        /// Render-ready state. `since` is how many log lines the page has
        /// already appended; only later ones are included.
        pub fn status_json(&self, since: f64) -> String {
            let since = (since.max(0.0) as usize).min(self.log.len());
            let stage = if self.failure.is_some() {
                "failed"
            } else if self.decoded_frames > 0 {
                "streaming"
            } else if self.said_encrypted {
                "encrypted"
            } else if self.said_paired {
                "paired"
            } else if self.said_connected {
                "connected"
            } else if self.transport.is_open() {
                "waiting"
            } else {
                "connecting"
            };
            serde_json::json!({
                "stage": stage,
                "socket_open": self.transport.is_open(),
                "frames": self.decoded_frames,
                "undecodable_bytes": self.undecodable_bytes,
                "events_in": self.events_in,
                "acl_in": self.acl_in,
                "media_sdus": self.media_sdus,
                "host_errors": self.host_errors,
                "lost_packets": self.lost_packets,
                "sample_rate": self.sample_rate(),
                "channels": self.channels(),
                "failure": self.failure,
                // The silicon's own answer to Read BD_ADDR — the address a
                // phone actually sees, which no page-side constant is.
                "bd_addr": self.host.local_address().map(|a| a.to_string()),
                "link_keys": self.host.all_link_keys().iter().map(|(peer, key)| {
                    serde_json::json!({
                        "peer": peer.to_string(),
                        "key": key.value.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                        "key_type": key.key_type,
                    })
                }).collect::<Vec<_>>(),
                "log": self.log[since..],
                "log_len": self.log.len(),
            })
            .to_string()
        }

        fn configuration(&self) -> Option<crate::classic::a2dp::SbcMediaCodecInformation> {
            self.host
                .handler::<crate::device::a2dp::A2dpSink>()?
                .configuration()
        }

        fn fail(&mut self, reason: String) {
            self.log.push(format!("FAIL: {reason}"));
            self.failure = Some(reason);
        }
    }

    /// The default HRM script, so the page needs no separate fetch.
    ///
    /// Despite the name this builds a *thermometer* — the Playground serves it
    /// as exactly that. Kept for the Playground; a page that wants a specific
    /// device should ask [`catalog_script`] for it by name.
    #[wasm_bindgen]
    pub fn default_heart_rate_script() -> String {
        DEFAULT_HEART_RATE_SCRIPT.to_string()
    }

    /// A device script from the shared catalog, by name.
    ///
    /// The catalog is the one definition of what `"hrm"` or `"thermometer"`
    /// means: MCP's `example` tool, the scene loader and now the demo pages
    /// all read it, so a device cannot mean one thing to an agent and another
    /// in a browser. Returns `undefined` for an unknown name rather than a
    /// placeholder, so a typo fails where it is made.
    #[wasm_bindgen]
    pub fn catalog_script(name: &str) -> Option<String> {
        crate::devices::catalog::script(name).map(str::to_string)
    }

    // -- the bulk-transfer benchmark ---------------------------------------
    //
    // Three wrappers for one measurement, because the two ends may sit on
    // different controllers. In-page both halves share a simulated medium in
    // this object; on netsim each half owns a socket; on the `simble --usb`
    // bridge each half owns a dongle. The Rust in
    // [`crate::device::throughput`] is identical in all three — only where
    // the packets go differs, which is the whole point of being able to
    // compare the numbers.

    /// The clock the benchmark is fed.
    ///
    /// `performance.now()` where there is a window (sub-millisecond,
    /// monotonic, unaffected by the wall clock being set), and `Date.now()`
    /// in a worker where there is not. Never `std::time::Instant`, which
    /// panics on `wasm32-unknown-unknown`.
    fn now_ms() -> f64 {
        web_sys::window()
            .and_then(|window| window.performance())
            .map(|performance| performance.now())
            .unwrap_or_else(js_sys::Date::now)
    }

    /// The bulk-transfer benchmark with both ends in this tab, over the
    /// in-process simulated medium.
    ///
    /// The numbers this produces measure **simble's own stack and a
    /// simulated link** — ATT, L2CAP fragmenting into 27-octet packets, the
    /// in-process controller — and no radio at all. A page showing them
    /// beside a dongle's must say so.
    ///
    /// [`Self::pump`] runs the scene for a slice of wall clock rather than a
    /// fixed number of steps, so a page stays responsive without the
    /// measurement becoming a measurement of the page's frame rate.
    #[wasm_bindgen]
    pub struct WebBulkBench {
        scene: crate::device::throughput::ThroughputScene,
        log: Vec<String>,
    }

    /// The in-page sink's address.
    const BULK_SINK_ADDRESS: Address = Address::new([0x0B, 0x00, 0x00, 0x57, 0x1E, 0xCC]);
    /// The in-page central's address.
    const BULK_CENTRAL_ADDRESS: Address = Address::new([0x0C, 0x00, 0x00, 0x57, 0x1E, 0xCC]);

    #[wasm_bindgen]
    impl WebBulkBench {
        /// One run, configured by the JSON a settings panel produces (see
        /// `BulkOptions`). Unknown keys and malformed JSON fall back to the
        /// defaults rather than refusing to run.
        #[wasm_bindgen(constructor)]
        pub fn new(options_json: &str) -> WebBulkBench {
            install_panic_hook();
            let options = crate::device::throughput::BulkOptions::from_json(options_json);
            Self {
                scene: crate::device::throughput::ThroughputScene::new(
                    BULK_SINK_ADDRESS,
                    BULK_CENTRAL_ADDRESS,
                    options,
                ),
                log: Vec::new(),
            }
        }

        /// Advances the run for up to `budget_ms` of wall clock, then hands
        /// back the report so far. Call it again until
        /// [`Self::is_finished`].
        pub fn pump(&mut self, budget_ms: f64) -> String {
            let deadline = now_ms() + budget_ms.max(1.0);
            while !self.scene.central().is_finished() {
                let now = now_ms();
                if now > deadline {
                    break;
                }
                self.scene.tick(now);
            }
            self.log.extend(self.scene.central_mut().take_log());
            self.scene.report_json()
        }

        /// Whether the run reached its end, successfully or not.
        pub fn is_finished(&self) -> bool {
            self.scene.central().is_finished()
        }

        /// What the run measured.
        pub fn report_json(&self) -> String {
            self.scene.report_json()
        }

        /// The progress lines, oldest first.
        pub fn log(&self) -> js_sys::Array {
            self.log
                .iter()
                .map(|line| JsValue::from_str(line))
                .collect()
        }
    }

    /// The benchmark **peripheral** on a controller of its own: a netsim
    /// socket, or the `simble --usb` bridge holding a dongle.
    ///
    /// It counts the bytes that arrive and stamps when the last one did,
    /// which is the half of the measurement the central cannot make. The
    /// page relays those numbers to [`WebBulkCentral::note_server`] so the
    /// transfer segment ends at arrival rather than at the central's last
    /// queued write.
    #[wasm_bindgen]
    pub struct WebBulkSink {
        transport: WasmWsTransport,
        channel: HciChannel,
        sink: crate::device::throughput::BulkSink,
        started: bool,
    }

    #[wasm_bindgen]
    impl WebBulkSink {
        /// Joins the controller at `url` as a benchmark sink advertising at
        /// `address`. `legacy_masks` narrows the LE event mask to what a
        /// Bluetooth 4.0 dongle accepts — a real part refuses the wider one
        /// outright and then reports no connection at all.
        #[wasm_bindgen(constructor)]
        pub fn new(url: &str, address: &str, legacy_masks: bool) -> Result<WebBulkSink, JsValue> {
            install_panic_hook();
            let address: Address = address
                .parse()
                .map_err(|_| JsValue::from_str("address is not a Bluetooth address"))?;
            let url = ws_url_with_wire_address(url);
            let mut sink = crate::device::throughput::BulkSink::new("simble-bulk-sink", address);
            if legacy_masks {
                sink.set_le_event_mask(crate::device::host::LE_EVENT_MASK_CORE_4_0);
            }
            Ok(Self {
                transport: WasmWsTransport::connect(&url)?,
                channel: HciChannel::new(),
                sink,
                started: false,
            })
        }

        /// The underlying WebSocket ready state (per the WebSocket API).
        pub fn ready_state(&self) -> u16 {
            self.transport.ready_state()
        }

        /// One pump. Returns the counters as JSON — what arrived, and when.
        pub fn tick(&mut self) -> Result<String, JsValue> {
            self.transport.pump(&self.channel)?;
            if !self.started && self.transport.is_open() {
                for packet in self.sink.start_commands() {
                    self.channel.inject_host_packet(packet).map_err(js_error)?;
                }
                self.started = true;
            }
            let now = now_ms();
            for packet in self.sink.poll() {
                let _ = self.channel.inject_host_packet(packet);
            }
            while let Some(packet) = self.channel.poll_controller_packet() {
                for out in self.sink.on_packet(&packet, now) {
                    let _ = self.channel.inject_host_packet(out);
                }
            }
            self.transport.pump(&self.channel)?;
            Ok(self.counters_json())
        }

        /// What the sink has seen, as JSON.
        pub fn counters_json(&self) -> String {
            let counters = self.sink.counters();
            serde_json::to_string(&counters).unwrap_or_else(|_| "{}".to_string())
        }

        /// Bytes received since the last `BEGIN`.
        pub fn bytes(&self) -> f64 {
            self.sink.counters().bytes as f64
        }

        /// Writes those bytes arrived in.
        pub fn chunks(&self) -> u32 {
            self.sink.counters().chunks
        }

        /// When the most recent byte landed, on the page's clock, or `None`
        /// if nothing has.
        pub fn last_byte_ms(&self) -> Option<f64> {
            self.sink.counters().last_byte_ms
        }

        /// Whether a central is connected.
        pub fn is_connected(&self) -> bool {
            self.sink.is_connected()
        }
    }

    /// LE Set Scan Enable — off.
    const SCAN_OFF: [u8; 5] = [0x0C, 0x20, 0x02, 0x00, 0x00];
    /// What the page renders while the scan is still going.
    const DISCOVERING_JSON: &str =
        "{\"phase\":\"discovering\",\"complete\":false,\"failure\":null,\"bytes_sent\":0}";

    /// A discovery that never found anything is still a measurement, and has
    /// to serialise like one so the page's table and CSV keep their shape.
    fn failed_report_json(why: &str) -> String {
        let quoted = serde_json::to_string(why).unwrap_or_else(|_| "\"failed\"".to_string());
        format!(
            "{{\"phase\":\"failed\",\"complete\":false,\"failure\":{quoted},\
             \"bytes_sent\":0,\"confirmation\":\"unconfirmed\"}}"
        )
    }

    /// The benchmark **central** on a controller of its own.
    ///
    /// Point it at a [`WebBulkSink`] on another socket (netsim) or another
    /// dongle (the bridge). Against a peer that is not a benchmark sink the
    /// run still measures discovery, connection and negotiation and then
    /// fails with a reason, which is a data point rather than a hang.
    #[wasm_bindgen]
    pub struct WebBulkCentral {
        transport: WasmWsTransport,
        channel: HciChannel,
        runner: crate::device::throughput::BulkCentral,
        started: bool,
        log: Vec<String>,
        /// Set when the peer has to be found before it can be aimed at.
        discover: Option<Discovery>,
        /// How long the scan took, once it has finished. Kept beside the
        /// report rather than inside it: finding a peer happens *before*
        /// `BulkCentral::start`, so it is not one of the four segments and
        /// must not be folded into `discover_ms`, which measures bring-up and
        /// hearing a peer already known.
        scan_taken_ms: Option<f64>,
        /// Why discovery gave up, if it did.
        failed: Option<String>,
    }

    /// The scan that precedes a run against a peer whose address is not
    /// knowable in advance.
    ///
    /// A phone advertises from a resolvable private address that rotates, and
    /// Android does not tell even its own app what that address currently is.
    /// So there is nothing to write down: the peer is found by the service it
    /// advertises, and the run is aimed at whatever answers.
    struct Discovery {
        options_json: String,
        legacy_masks: bool,
        scanning: bool,
        give_up_at_ms: Option<f64>,
        began_ms: Option<f64>,
        /// Which peer to accept, by advertised name. Empty means the first
        /// one carrying the service.
        ///
        /// With two phones running the sink, the service alone is ambiguous:
        /// the scan would take whichever advertised first while the counters
        /// were fetched from whichever endpoint was configured, and those need
        /// not be the same phone. The name is the only handle available —
        /// Android advertises from a rotating private address it will not
        /// disclose even to its own app.
        name: String,
        /// Addresses heard advertising the service, and the names their scan
        /// responses carried.
        ///
        /// A legacy advertisement is 31 octets and a 128-bit service UUID
        /// takes 16 of them, so the sink puts its name in the *scan response*
        /// — a second report, with no service UUID in it. Neither report
        /// alone identifies a named peer, and waiting for one carrying both
        /// waits forever. They are correlated by address, which is stable for
        /// as long as a scan lasts even when it is a rotating private one.
        heard: Vec<(String, bool, Option<String>)>,
    }

    impl Discovery {
        /// Folds one report in, and says whether that address now satisfies
        /// both halves.
        fn note(&mut self, address: &str, has_service: bool, name: Option<&str>) -> bool {
            let entry = match self.heard.iter_mut().find(|(a, _, _)| a == address) {
                Some(entry) => entry,
                None => {
                    self.heard.push((address.to_string(), false, None));
                    self.heard.last_mut().expect("just pushed")
                }
            };
            entry.1 |= has_service;
            if let Some(name) = name {
                entry.2 = Some(name.to_string());
            }
            let named = self.name.is_empty() || entry.2.as_deref() == Some(self.name.as_str());
            entry.1 && named
        }
    }

    #[wasm_bindgen]
    impl WebBulkCentral {
        /// Joins the controller at `url` and aims at `target`, configured by
        /// the same settings JSON [`WebBulkBench`] takes.
        #[wasm_bindgen(constructor)]
        pub fn new(
            url: &str,
            target: &str,
            options_json: &str,
            legacy_masks: bool,
        ) -> Result<WebBulkCentral, JsValue> {
            install_panic_hook();
            let target: Address = target
                .parse()
                .map_err(|_| JsValue::from_str("target is not a Bluetooth address"))?;
            let url = ws_url_with_wire_address(url);
            let options = crate::device::throughput::BulkOptions::from_json(options_json);
            let mut runner = crate::device::throughput::BulkCentral::new(target, options);
            if legacy_masks {
                runner.set_le_event_mask(crate::device::host::LE_EVENT_MASK_CORE_4_0);
            }
            Ok(Self {
                transport: WasmWsTransport::connect(&url)?,
                channel: HciChannel::new(),
                runner,
                started: false,
                log: Vec::new(),
                discover: None,
                scan_taken_ms: None,
                failed: None,
            })
        }

        /// Joins the controller at `url` and aims at whatever advertises the
        /// bulk service, rather than at an address given in advance.
        ///
        /// This is what a phone needs. See [`Discovery`].
        #[wasm_bindgen(js_name = discovering)]
        pub fn discovering(
            url: &str,
            options_json: &str,
            legacy_masks: bool,
            name: &str,
        ) -> Result<WebBulkCentral, JsValue> {
            install_panic_hook();
            let url = ws_url_with_wire_address(url);
            let options = crate::device::throughput::BulkOptions::from_json(options_json);
            // A placeholder target: the runner is rebuilt, unstarted, the
            // moment the scan produces a real one.
            let mut runner = crate::device::throughput::BulkCentral::new(
                Address::from_be_bytes([0; 6]),
                options,
            );
            if legacy_masks {
                runner.set_le_event_mask(crate::device::host::LE_EVENT_MASK_CORE_4_0);
            }
            Ok(Self {
                transport: WasmWsTransport::connect(&url)?,
                channel: HciChannel::new(),
                runner,
                started: false,
                log: Vec::new(),
                discover: Some(Discovery {
                    options_json: options_json.to_string(),
                    legacy_masks,
                    scanning: false,
                    give_up_at_ms: None,
                    began_ms: None,
                    name: name.to_string(),
                    heard: Vec::new(),
                }),
                scan_taken_ms: None,
                failed: None,
            })
        }

        /// The underlying WebSocket ready state (per the WebSocket API).
        pub fn ready_state(&self) -> u16 {
            self.transport.ready_state()
        }

        /// One pump plus one step. Returns the report as JSON.
        pub fn tick(&mut self) -> Result<String, JsValue> {
            self.transport.pump(&self.channel)?;
            let now = now_ms();
            if let Some(interim) = self.poll_discovery(now)? {
                self.transport.pump(&self.channel)?;
                return Ok(interim);
            }
            if !self.started && self.transport.is_open() {
                for packet in self.runner.start(now) {
                    self.channel.inject_host_packet(packet).map_err(js_error)?;
                }
                self.started = true;
            }
            if self.started {
                while let Some(packet) = self.channel.poll_controller_packet() {
                    for out in self.runner.on_packet(&packet, now) {
                        let _ = self.channel.inject_host_packet(out);
                    }
                }
                for packet in self.runner.step(now) {
                    let _ = self.channel.inject_host_packet(packet);
                }
            }
            self.log.extend(self.runner.take_log());
            self.transport.pump(&self.channel)?;
            Ok(self.runner.report_json())
        }

        /// Tells the run what the peripheral saw. `last_byte_ms` must be on
        /// the same clock this object uses — which it is when both halves
        /// live in one page, and is why netsim runs are `server-stamped`
        /// rather than merely `peer-reported`.
        pub fn note_server(&mut self, bytes: f64, chunks: u32, last_byte_ms: Option<f64>) {
            self.runner
                .note_server(crate::device::throughput::SinkCounters {
                    bytes: bytes.max(0.0) as u64,
                    chunks,
                    expected: 0,
                    first_byte_ms: None,
                    last_byte_ms,
                });
        }

        /// Whether the run reached its end, successfully or not.
        pub fn is_finished(&self) -> bool {
            self.failed.is_some() || self.runner.is_finished()
        }

        /// What the run measured.
        pub fn report_json(&self) -> String {
            match &self.failed {
                Some(why) => failed_report_json(why),
                None => self.runner.report_json(),
            }
        }

        /// How long finding the peer took, in milliseconds, or `-1` if this
        /// run was aimed at an address and never scanned.
        ///
        /// Reported separately because it happens *before* the run starts:
        /// folding it into `discover_ms` would mix finding a stranger with
        /// bring-up against a peer already known.
        pub fn scan_ms(&self) -> f64 {
            self.scan_taken_ms.unwrap_or(-1.0)
        }

        /// Scans for the bulk service, and re-aims the run at what answers.
        ///
        /// Returns `Some(json)` while the scan is still going, which the page
        /// shows as progress; `None` once there is a target (or once there
        /// never will be), so `tick` proceeds to the run itself.
        ///
        /// The discovery state is taken out for the duration rather than
        /// borrowed, so this can use `self.channel` freely.
        fn poll_discovery(&mut self, now: f64) -> Result<Option<String>, JsValue> {
            let Some(mut d) = self.discover.take() else {
                return Ok(None);
            };
            // Nothing to scan with until the controller socket is up.
            if !self.transport.is_open() {
                self.discover = Some(d);
                return Ok(Some(DISCOVERING_JSON.to_string()));
            }
            if !d.scanning {
                queue_scanner_start(&self.channel).map_err(js_error)?;
                d.scanning = true;
                d.began_ms = Some(now);
                // The run's own configured patience, not a second
                // invented number: a caller who widens the timeout
                // because the air is busy means it for the scan too.
                let patience =
                    crate::device::throughput::BulkOptions::from_json(&d.options_json).timeout_ms;
                d.give_up_at_ms = Some(now + patience);
                self.log.push(if d.name.is_empty() {
                    "scanning for a bulk sink".to_string()
                } else {
                    format!("scanning for {}", d.name)
                });
            }

            let wanted = crate::device::throughput::bulk_uuid::SERVICE.to_string();
            let mut found: Option<String> = None;
            while let Some(packet) = self.channel.poll_controller_packet() {
                for report in parse_scan_reports(&packet) {
                    let has_service = report
                        .service_uuids
                        .iter()
                        .any(|u| u.eq_ignore_ascii_case(&wanted));
                    if d.note(&report.address, has_service, report.name.as_deref()) {
                        found = Some(report.address.clone());
                    }
                }
            }

            if let Some(address) = found {
                let Ok(target) = address.parse::<Address>() else {
                    self.discover = Some(d);
                    return Ok(Some(DISCOVERING_JSON.to_string()));
                };
                // Stop scanning before connecting: a controller still in scan
                // mode has not freed what the connection needs.
                self.channel.send_command(&SCAN_OFF).map_err(js_error)?;
                let options = crate::device::throughput::BulkOptions::from_json(&d.options_json);
                let mut runner = crate::device::throughput::BulkCentral::new(target, options);
                if d.legacy_masks {
                    runner.set_le_event_mask(crate::device::host::LE_EVENT_MASK_CORE_4_0);
                }
                self.runner = runner;
                let took = d.began_ms.map(|began| now - began);
                self.scan_taken_ms = took;
                self.log.push(match took {
                    Some(ms) if !d.name.is_empty() => {
                        format!("found {} at {address} in {ms:.0} ms", d.name)
                    }
                    Some(ms) => format!("found a sink at {address} in {ms:.0} ms"),
                    None => format!("found a sink at {address}"),
                });
                return Ok(None);
            }

            if d.give_up_at_ms.is_some_and(|at| now > at) {
                self.failed = Some(if d.name.is_empty() {
                    "nothing advertising the bulk service — is SimBLE Android running \
                     and in the foreground?"
                        .to_string()
                } else {
                    format!(
                        "no advertisement from {} carrying the bulk service — is SimBLE Android \
                         running and in the foreground on that phone?",
                        d.name
                    )
                });
                // Returning None here would fall through to starting the run,
                // which then aimed at the placeholder address and reported a
                // transfer to 00:00:00:00:00:00.
                return Ok(Some(self.report_json()));
            }

            self.discover = Some(d);
            Ok(Some(DISCOVERING_JSON.to_string()))
        }

        /// The progress lines, oldest first.
        pub fn log(&self) -> js_sys::Array {
            self.log
                .iter()
                .map(|line| JsValue::from_str(line))
                .collect()
        }
    }

    /// Runs a Rhai test script (device-building + `assert(...)`) and returns
    /// `{"ok":true}` if every assertion passed, or `{"ok":false,"error":"…"}`
    /// with the failure message.
    #[wasm_bindgen]
    pub fn run_test(script: &str) -> String {
        match super::run_test_script(script) {
            Ok(()) => "{\"ok\":true,\"error\":\"\"}".to_string(),
            Err(e) => {
                let msg = serde_json::to_string(&e).unwrap_or_else(|_| "\"error\"".to_string());
                format!("{{\"ok\":false,\"error\":{msg}}}")
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub use web::{
    WebAdvertiser, WebBulkBench, WebBulkCentral, WebBulkSink, WebCarKit, WebPeripheral, WebScanner,
    WebSession, default_heart_rate_script, run_test,
};
