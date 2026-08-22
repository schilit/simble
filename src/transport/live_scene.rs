// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! A scene of scripted peripherals whose controllers live on the far side of
//! a real [`HciTransport`] — netsim today, a USB dongle tomorrow — rather
//! than the in-process `SceneEngine` radio. Peripheral-only by design: the
//! far side (an Android emulator, a real phone) plays the central.

use super::wasm_ws::ScriptedPeripheral;
use super::{HciChannel, HciTransport};
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

/// The live-backend counterpart of the in-process `SceneEngine`: each
/// peripheral owns its transport, and the backend routes advertising and
/// data between them and whatever else shares its ether.
pub struct LiveScene<T: HciTransport> {
    devices: Vec<LiveDevice<T>>,
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
        connect: impl FnOnce(&ScriptedPeripheral) -> Result<T, String>,
    ) -> Result<usize, String> {
        let mut peripheral = ScriptedPeripheral::run_script(script)?;
        peripheral.set_identity(address);
        let transport = connect(&peripheral)?;
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

    /// The number of peripherals in the scene.
    pub fn device_count(&self) -> usize {
        self.devices.len()
    }

    /// The GATT status JSON of peripheral `index` (same shape as the
    /// in-process scene's), or `None` for an unknown index.
    pub fn peripheral_status_json(&self, index: usize) -> Option<String> {
        Some(self.devices.get(index)?.peripheral.status_json())
    }
}
