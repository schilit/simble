// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! A scene of scripted peripherals whose controllers live on the far side of
//! a real [`HciTransport`] — netsim today, a USB dongle tomorrow — rather
//! than the in-process `SceneEngine` radio. Peripheral-only by design: the
//! far side (an Android emulator, a real phone) plays the central.

use super::scan_report::{ScanReport, parse_scan_reports, queue_scanner_start};
use super::{HciChannel, HciTransport};
use crate::device::scripted_peripheral::ScriptedPeripheral;
use std::sync::Arc;

/// One scripted peripheral on a live backend: the script/GATT logic, the
/// [`HciChannel`] between host logic and controller, and the transport that
/// *is* its controller.
struct LiveDevice<T: HciTransport> {
    peripheral: Box<ScriptedPeripheral>,
    channel: Arc<HciChannel>,
    transport: T,
    started: bool,
}

/// A scanner on a live backend: its own controller, listening on the air and
/// keeping the latest advertising report per advertiser. A radio plays one
/// role, so this is a separate controller from any peripheral in the scene —
/// which is why a backend hands it its own transport (a second dongle, another
/// netsim connection). Unlike the in-process scene's scanner, what this hears
/// is whatever is actually on the medium: real devices on real RF.
struct LiveScanner<T: HciTransport> {
    channel: Arc<HciChannel>,
    transport: T,
    started: bool,
    /// Advertiser addresses in first-heard order, parallel to `reports`.
    order: Vec<String>,
    /// The latest report per advertiser, so a device that advertises many times
    /// over a scan window collapses to one entry with its freshest content.
    reports: Vec<ScanReport>,
}

/// The live-backend counterpart of the in-process `SceneEngine`: each
/// peripheral owns its transport, and the backend routes advertising and
/// data between them and whatever else shares its ether.
pub struct LiveScene<T: HciTransport> {
    devices: Vec<LiveDevice<T>>,
    /// An optional scanner sharing the backend's medium, on its own controller.
    scanner: Option<LiveScanner<T>>,
    /// Script-clock seconds handed to `fn tick`; advanced by [`tick`](Self::tick).
    t: f64,
}

impl<T: HciTransport> Default for LiveScene<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: HciTransport> LiveScene<T> {
    /// Creates an empty live scene.
    pub fn new() -> Self {
        Self {
            devices: Vec::new(),
            scanner: None,
            t: 0.0,
        }
    }

    /// Runs `script`, stamps `address` as the device's on-air identity (SMP
    /// computes with it — see `ScriptedPeripheral::set_identity`), then joins
    /// it to the backend over the transport `connect` returns for it (how to
    /// connect — a WebSocket per device, a shared dongle — is
    /// backend-specific, and `connect` gets the built peripheral so it can
    /// use its name/GATT in the handshake). Returns the device index.
    pub fn add_peripheral(
        &mut self,
        address: crate::types::Address,
        script: &str,
        connect: impl FnOnce(&mut ScriptedPeripheral) -> Result<T, String>,
    ) -> Result<usize, String> {
        let mut peripheral = ScriptedPeripheral::run_script(script)?;
        peripheral.set_identity(address);
        // `&mut`, not `&`: a backend may have to *configure* the peripheral
        // for the controller it just opened, not merely read it. The USB
        // backend narrows the LE event mask here, because the dongle it
        // found rejects the default one and says so only to itself.
        let transport = connect(&mut peripheral)?;
        let index = self.devices.len();
        self.devices.push(LiveDevice {
            peripheral: Box::new(peripheral),
            channel: Arc::new(HciChannel::new()),
            transport,
            started: false,
        });
        Ok(index)
    }

    /// Moves packets for every device without advancing the script clock:
    /// queues bring-up on a device's first pump, ferries transport traffic
    /// both ways, and handles whatever the backend delivered (connections,
    /// ATT requests). Runs from the MCP actor loop between requests, so
    /// peripherals answer their centrals even while no tool call is active.
    /// Failures land in the device's `last_error` rather than tearing the
    /// scene down.
    pub fn pump(&mut self) {
        for device in &mut self.devices {
            if !device.started {
                if let Err(e) = device.peripheral.queue_start(&device.channel) {
                    device.peripheral.record_error(e.to_string());
                }
                device.started = true;
            }
            if let Err(e) = device.transport.pump(&device.channel) {
                device.peripheral.record_error(e.to_string());
                continue;
            }
            while let Some(packet) = device.channel.poll_controller_packet() {
                if let Err(e) = device.peripheral.handle_packet(&device.channel, &packet) {
                    device.peripheral.record_error(e.to_string());
                }
            }
        }
        self.pump_scanner();
    }

    /// Ferries the scanner's controller both ways and folds each advertising
    /// report into `reports`, latest-per-advertiser. Its first pump queues the
    /// scan bring-up (reset, event masks a 4.0 dongle accepts, active-scan
    /// parameters, enable) — the same `queue_scanner_start` a page or the bulk
    /// example uses, so a CSR8510 answers it.
    fn pump_scanner(&mut self) {
        let Some(scanner) = self.scanner.as_mut() else {
            return;
        };
        if !scanner.started {
            let _ = queue_scanner_start(&scanner.channel);
            scanner.started = true;
        }
        if scanner.transport.pump(&scanner.channel).is_err() {
            return;
        }
        while let Some(packet) = scanner.channel.poll_controller_packet() {
            for report in parse_scan_reports(&packet) {
                match scanner.order.iter().position(|a| a == &report.address) {
                    Some(i) => scanner.reports[i] = report,
                    None => {
                        scanner.order.push(report.address.clone());
                        scanner.reports.push(report);
                    }
                }
            }
        }
    }

    /// Advances the script clock by `seconds` (each device's `fn tick` runs
    /// once at the new time), then pumps.
    pub fn tick(&mut self, seconds: f64) {
        self.t += seconds;
        let t = self.t;
        for device in &mut self.devices {
            if let Err(e) = device.peripheral.tick(&device.channel, t) {
                device.peripheral.record_error(e.to_string());
            }
        }
        self.pump();
    }

    /// The current script-clock time in seconds.
    pub fn now(&self) -> f64 {
        self.t
    }

    /// The earliest absolute wake time (script-clock seconds) any device has
    /// declared with the `wake_at` binding, or `None` if none has — the scene's
    /// sans-io deadline. The backend exposes it to hosts as a microsecond clock.
    pub fn next_deadline(&self) -> Option<f64> {
        self.devices
            .iter()
            .filter_map(|d| d.peripheral.next_wake())
            .reduce(f64::min)
    }

    /// The number of peripherals in the scene.
    pub fn device_count(&self) -> usize {
        self.devices.len()
    }

    /// The GATT status JSON of peripheral `index` (same shape as the
    /// in-process scene's), or `None` for an unknown index.
    pub fn peripheral_status_json(&self, index: usize) -> Option<String> {
        Some(self.devices.get(index)?.peripheral.status_json())
    }

    /// Joins a scanner to the scene on `transport` (a controller the backend
    /// opened for it). Idempotent: a scene keeps one scanner, so this replaces
    /// any earlier one and starts its window fresh.
    pub fn add_scanner(&mut self, transport: T) {
        self.scanner = Some(LiveScanner {
            channel: Arc::new(HciChannel::new()),
            transport,
            started: false,
            order: Vec::new(),
            reports: Vec::new(),
        });
    }

    /// Whether a scanner is already listening on this scene.
    pub fn has_scanner(&self) -> bool {
        self.scanner.is_some()
    }

    /// The scanner's advertising reports as a JSON array (one per advertiser,
    /// latest content), or `None` if no scanner has been added.
    pub fn scanner_reports_json(&self) -> Option<String> {
        let scanner = self.scanner.as_ref()?;
        Some(serde_json::to_string(&scanner.reports).unwrap_or_else(|_| "[]".to_string()))
    }
}
