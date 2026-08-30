// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! The multi-device scene engine.
//!
//! [`SceneEngine`] hosts a set of scripted peripherals, scene centrals, and
//! classic devices on one shared link and advances them together on each
//! [`SceneEngine::tick`] — the natively-testable core the browser Scene page
//! and the MCP scene tools drive. It was extracted from the wasm transport so
//! every surface reaches it here rather than reaching up into a browser module.

use crate::controller::sim::Link;
use crate::device::central_device::CentralDevice;
use crate::device::classic_device::ClassicDevice;
use crate::device::scripted_peripheral::ScriptedPeripheral;
use crate::scripting::ScriptedCentral;
use crate::transport::hci_adapter::HciChannel;
use crate::transport::scan_report::{ScanReport, parse_scan_reports, queue_scanner_start};
use crate::types::Address;

/// The role a device plays in a [`SceneEngine`].
enum SceneRole {
    /// A scripted GATT peripheral that advertises and serves. Boxed because a
    /// `ScriptedPeripheral` is much larger than the scanner variant.
    Peripheral(Box<ScriptedPeripheral>),
    /// A scanner accumulating the advertising reports it has seen.
    Scanner(Vec<ScanReport>),
    /// A central that connects to a peripheral and discovers its GATT.
    Central(Box<CentralDevice>),
    /// A central whose behaviour is a Rhai script (`android::BluetoothGatt`).
    /// Boxed for the same reason the peripheral is: it carries an engine.
    ScriptedCentral(Box<ScriptedCentral>),
    /// A BR/EDR device — the fifth thing a scene can host, and the only one
    /// that is not LE. Boxed: it carries a whole `ClassicHost`.
    Classic(Box<ClassicDevice>),
}

/// One device in a scene: the controller-side [`HciChannel`] it shares with the
/// [`Link`], its role, and whether its HCI bring-up has been queued yet.
struct SceneDevice {
    channel: std::sync::Arc<HciChannel>,
    role: SceneRole,
    started: bool,
}

/// An in-process scene of Rhai devices sharing one [`Link`] — the browser's
/// "in-page controller" backend, and a native, netsim-free way to run many
/// devices together. Peripherals advertise and serve GATT; scanners collect
/// advertising reports; the shared [`Link`] routes between them. Transport-free
/// (no WebSocket, no netsim), so it runs identically native and on wasm32, and
/// a single page can host a whole scene.
pub struct SceneEngine {
    link: Link,
    devices: Vec<SceneDevice>,
}

impl Default for SceneEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl SceneEngine {
    /// Creates an empty scene.
    pub fn new() -> Self {
        Self {
            link: Link::new(),
            devices: Vec::new(),
        }
    }

    /// Adds a scripted peripheral at `address`; returns its device index (or the
    /// script error).
    pub fn add_peripheral(&mut self, address: Address, script: &str) -> Result<usize, String> {
        let mut peripheral = ScriptedPeripheral::run_script(script)?;
        peripheral.set_identity(address);
        let channel = self.link.add_device(address);
        let index = self.devices.len();
        self.devices.push(SceneDevice {
            channel,
            role: SceneRole::Peripheral(Box::new(peripheral)),
            started: false,
        });
        Ok(index)
    }

    /// Adds a scanner at `address`; returns its device index.
    pub fn add_scanner(&mut self, address: Address) -> usize {
        let channel = self.link.add_device(address);
        let index = self.devices.len();
        self.devices.push(SceneDevice {
            channel,
            role: SceneRole::Scanner(Vec::new()),
            started: false,
        });
        index
    }

    /// Adds a central at `address` that connects to and discovers the peripheral
    /// at `target`; returns its device index.
    pub fn add_central(&mut self, address: Address, target: Address) -> usize {
        let channel = self.link.add_device(address);
        let index = self.devices.len();
        self.devices.push(SceneDevice {
            channel,
            role: SceneRole::Central(Box::new(CentralDevice::new(target))),
            started: false,
        });
        index
    }

    /// Adds a *scripted* central at `address`: a Rhai script that builds an
    /// `android::BluetoothGatt`, connects it and reacts in callbacks. Returns
    /// its device index, or the script error.
    ///
    /// The script names its own target with `client.connect("AA:BB:…")`, so
    /// unlike [`Self::add_central`] the scene does not supply one — the
    /// script is the whole behaviour.
    pub fn add_scripted_central(
        &mut self,
        address: Address,
        script: &str,
    ) -> Result<usize, String> {
        let central = ScriptedCentral::run_script(script)?;
        let channel = self.link.add_device(address);
        let index = self.devices.len();
        self.devices.push(SceneDevice {
            channel,
            role: SceneRole::ScriptedCentral(Box::new(central)),
            started: false,
        });
        Ok(index)
    }

    /// Adds a **BR/EDR** device at `address`; returns its device index.
    ///
    /// This is the fifth thing a scene can host, beside the four LE roles
    /// above, and the first that speaks Bluetooth Classic. Build the device
    /// with `ClassicDevice::acceptor` (discoverable, connectable, serving
    /// SDP and an echoing RFCOMM port) or `ClassicDevice::initiator`
    /// (inquires, pages, queries SDP, opens the advertised serial port).
    ///
    /// Nothing about it is LE: it shares the [`Link`] with the LE devices
    /// because they share a simulated room and an ACL router, not because
    /// they share a transport.
    pub fn add_classic_device(&mut self, address: Address, device: ClassicDevice) -> usize {
        let channel = self.link.add_device(address);
        let index = self.devices.len();
        self.devices.push(SceneDevice {
            channel,
            role: SceneRole::Classic(Box::new(device)),
            started: false,
        });
        index
    }

    /// The classic device at `index`, or `None` if that device is something
    /// else — the handle a test needs for its phase, what its inquiry found,
    /// and what came back over its serial port.
    pub fn classic_device(&self, index: usize) -> Option<&ClassicDevice> {
        match self.devices.get(index)?.role {
            SceneRole::Classic(ref d) => Some(d),
            _ => None,
        }
    }

    /// Mutable access to the BR/EDR device at `index` — what a profile above
    /// the link needs in order to ask for an audio connection or put audio
    /// on it.
    pub fn classic_device_mut(&mut self, index: usize) -> Option<&mut ClassicDevice> {
        match self.devices.get_mut(index)?.role {
            SceneRole::Classic(ref mut d) => Some(d),
            _ => None,
        }
    }

    /// The BR/EDR status JSON of classic device `index` (see
    /// `ClassicDevice::status_json`), or `None` if it isn't one.
    pub fn classic_status_json(&self, index: usize) -> Option<String> {
        Some(self.classic_device(index)?.status_json())
    }

    /// The number of devices in the scene.
    pub fn device_count(&self) -> usize {
        self.devices.len()
    }

    /// The earliest absolute wake time (script-clock seconds) any peripheral has
    /// declared with the `wake_at` binding, or `None` — the deterministic scene's
    /// sans-io deadline, folded across devices.
    pub fn next_deadline(&self) -> Option<f64> {
        self.devices
            .iter()
            .filter_map(|d| match &d.role {
                SceneRole::Peripheral(p) => p.next_wake(),
                _ => None,
            })
            .reduce(f64::min)
    }

    /// Advances the whole scene one step at simulated time `t_seconds`: queues
    /// each device's bring-up on its first tick, lets peripherals run their
    /// scripts and emit notifications, routes advertising and data across the
    /// shared [`Link`], then delivers the results back to each device.
    pub fn tick(&mut self, t_seconds: f64) {
        for device in &mut self.devices {
            if !device.started {
                let _ = match &mut device.role {
                    SceneRole::Peripheral(p) => p.queue_start(&device.channel),
                    SceneRole::Scanner(_) => queue_scanner_start(&device.channel),
                    // Both centrals queue their own bring-up: the scene one
                    // in `produce`, the scripted one when its script called
                    // `connect`.
                    SceneRole::Central(_) | SceneRole::ScriptedCentral(_) => Ok(()),
                    SceneRole::Classic(c) => {
                        c.queue_start(&device.channel);
                        Ok(())
                    }
                };
                device.started = true;
            }
        }
        // Devices produce (peripherals: script tick + notifications; centrals:
        // the connection request and discovery flow).
        for device in &mut self.devices {
            match &mut device.role {
                SceneRole::Peripheral(p) => {
                    if let Err(e) = p.tick(&device.channel, t_seconds) {
                        p.record_error(e.to_string());
                    }
                }
                SceneRole::Central(c) => c.produce(&device.channel),
                SceneRole::ScriptedCentral(c) => {
                    for packet in c.tick(t_seconds) {
                        let _ = device.channel.inject_host_packet(packet);
                    }
                }
                SceneRole::Classic(c) => c.produce(&device.channel),
                SceneRole::Scanner(_) => {}
            }
        }
        // Route across the shared medium.
        self.link.tick();
        // Consume the delivered events.
        for device in &mut self.devices {
            match &mut device.role {
                SceneRole::Peripheral(p) => {
                    while let Some(pkt) = device.channel.poll_controller_packet() {
                        if let Err(e) = p.handle_packet(&device.channel, &pkt) {
                            p.record_error(e.to_string());
                        }
                    }
                }
                SceneRole::Scanner(reports) => {
                    while let Some(pkt) = device.channel.poll_controller_packet() {
                        reports.extend(parse_scan_reports(&pkt));
                    }
                }
                SceneRole::Central(c) => {
                    while let Some(pkt) = device.channel.poll_controller_packet() {
                        c.consume(&device.channel, &pkt);
                    }
                }
                SceneRole::ScriptedCentral(c) => {
                    while let Some(pkt) = device.channel.poll_controller_packet() {
                        for out in c.on_packet(&pkt) {
                            let _ = device.channel.inject_host_packet(out);
                        }
                    }
                }
                SceneRole::Classic(c) => {
                    while let Some(pkt) = device.channel.poll_controller_packet() {
                        c.consume(&device.channel, &pkt);
                    }
                }
            }
        }
    }

    /// The scripted central at `index`, or `None` if that device is something
    /// else — the handle a host needs for its status, its emitted messages
    /// and whether one of its `assert`s failed.
    pub fn scripted_central(&self, index: usize) -> Option<&ScriptedCentral> {
        match self.devices.get(index)?.role {
            SceneRole::ScriptedCentral(ref c) => Some(c),
            _ => None,
        }
    }

    /// Mutable access to the scripted central at `index` (draining emitted
    /// messages needs it).
    pub fn scripted_central_mut(&mut self, index: usize) -> Option<&mut ScriptedCentral> {
        match self.devices.get_mut(index)?.role {
            SceneRole::ScriptedCentral(ref mut c) => Some(c),
            _ => None,
        }
    }

    /// The GATT status JSON of peripheral `index` (see
    /// `ScriptedPeripheral::status_json`), or `None` if it isn't a peripheral.
    pub fn peripheral_status_json(&self, index: usize) -> Option<String> {
        match self.devices.get(index)?.role {
            SceneRole::Peripheral(ref p) => Some(p.status_json()),
            SceneRole::Scanner(_)
            | SceneRole::Central(_)
            | SceneRole::ScriptedCentral(_)
            | SceneRole::Classic(_) => None,
        }
    }

    /// The discovered-GATT JSON of central `index`, or `None` if it isn't a
    /// central.
    pub fn central_status_json(&self, index: usize) -> Option<String> {
        match self.devices.get(index)?.role {
            SceneRole::Central(ref c) => Some(c.status_json()),
            SceneRole::ScriptedCentral(ref c) => Some(c.status_json()),
            SceneRole::Peripheral(_) | SceneRole::Scanner(_) | SceneRole::Classic(_) => None,
        }
    }

    /// Queue a read of `value_handle` on central `index`.
    pub fn central_read(&mut self, index: usize, value_handle: u16) {
        if let Some(SceneRole::Central(c)) = self.devices.get_mut(index).map(|d| &mut d.role) {
            c.queue_read(value_handle);
        }
    }

    /// Queue a write of `value` to `value_handle` on central `index`.
    pub fn central_write(&mut self, index: usize, value_handle: u16, value: Vec<u8>) {
        if let Some(SceneRole::Central(c)) = self.devices.get_mut(index).map(|d| &mut d.role) {
            c.queue_write(value_handle, value);
        }
    }

    /// Streams one isochronous SDU from central `index` to the peripheral it
    /// is connected to — the media plane a real LE Audio source drives.
    /// Returns false if the central has no connection yet.
    pub fn central_send_audio(&mut self, index: usize, sdu: &[u8]) -> bool {
        let Some(SceneRole::Central(central)) = self.devices.get(index).map(|d| &d.role) else {
            return false;
        };
        let handle = central.client.connection_handle;
        if handle == 0 {
            return false;
        }
        let sequence = central.audio_tx_sequence;
        if let Some(SceneRole::Central(central)) = self.devices.get_mut(index).map(|d| &mut d.role)
        {
            central.audio_tx_sequence = sequence.wrapping_add(1);
        }
        let packet = crate::packets::build_iso_packet(handle, sequence, sdu);
        let _ = self.devices[index].channel.inject_host_packet(packet);
        true
    }

    /// Drains the SDUs peripheral `index` has received, oldest first.
    pub fn peripheral_take_audio(&mut self, index: usize) -> Vec<Vec<u8>> {
        match self.devices.get_mut(index).map(|d| &mut d.role) {
            Some(SceneRole::Peripheral(p)) => p.take_audio(),
            _ => Vec::new(),
        }
    }

    /// Queue enabling notifications on `value_handle` for central `index`.
    pub fn central_subscribe(&mut self, index: usize, value_handle: u16) {
        if let Some(SceneRole::Central(c)) = self.devices.get_mut(index).map(|d| &mut d.role) {
            c.queue_subscribe(value_handle);
        }
    }

    /// Host-writes `value` into characteristic `uuid` of peripheral `index`
    /// (the in-page equivalent of `WebPeripheral::set_value`): updates the live
    /// GATT database and notifies any subscribed central.
    pub fn peripheral_set_value(
        &mut self,
        index: usize,
        uuid: &str,
        value: &[u8],
    ) -> Result<(), String> {
        match self.devices.get_mut(index).map(|d| &mut d.role) {
            Some(SceneRole::Peripheral(p)) => p.set_characteristic_value(uuid, value),
            _ => Err("not a peripheral".to_string()),
        }
    }

    /// Host-writes `value` into characteristic `uuid` of peripheral `index`
    /// and notifies it even when the bytes are unchanged — see
    /// `ScriptedPeripheral::notify_characteristic_value`.
    pub fn peripheral_notify_value(
        &mut self,
        index: usize,
        uuid: &str,
        value: &[u8],
    ) -> Result<(), String> {
        match self.devices.get_mut(index).map(|d| &mut d.role) {
            Some(SceneRole::Peripheral(p)) => p.notify_characteristic_value(uuid, value),
            _ => Err("not a peripheral".to_string()),
        }
    }

    /// Drives central `index` as a HID host: reads the peer's Report Map and
    /// subscribes to its input Reports. Returns false until the central has
    /// finished discovery, so a caller polls this once per tick until it
    /// takes.
    pub fn central_start_hid(&mut self, index: usize) -> bool {
        match self.devices.get_mut(index).map(|d| &mut d.role) {
            Some(SceneRole::Central(c)) => c.hid_start(),
            _ => false,
        }
    }

    /// The HID input central `index` has decoded since the last call (see
    /// `CentralDevice::hid_events_json`).
    pub fn central_hid_events_json(&mut self, index: usize) -> String {
        match self.devices.get_mut(index).map(|d| &mut d.role) {
            Some(SceneRole::Central(c)) => c.hid_events_json(),
            _ => "{}".to_string(),
        }
    }

    /// The scan reports scanner `index` has collected as a JSON array, draining
    /// them so each call returns only what's new.
    pub fn scanner_reports_json(&mut self, index: usize) -> String {
        match self.devices.get_mut(index).map(|d| &mut d.role) {
            Some(SceneRole::Scanner(reports)) => {
                let json = serde_json::to_string(&reports).unwrap_or_else(|_| "[]".to_string());
                reports.clear();
                json
            }
            _ => "[]".to_string(),
        }
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod scene_tests;

#[cfg(test)]
#[path = "engine_classic_tests.rs"]
mod classic_scene_tests;
