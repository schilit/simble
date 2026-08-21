// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Multi-device simulation runtime, registry, and REST router for Simble.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::api::dto::{CreateDeviceRequest, CreateDeviceResponse, SetAdvertisingRequest};
use crate::device::VirtualDevice;
use crate::devices::{BleKeyboard, BleMouse, EddystoneUidBeacon, HeartRateMonitor, IBeacon};
use crate::transport::HciChannel;
use crate::types::{Address, SimbleError};

/// Summary representation of a registered Simble Virtual Device.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceSummary {
    /// Device id.
    pub device_id: String,
    /// Name.
    pub name: String,
    /// Address.
    pub address: String,
    /// Is advertising.
    pub is_advertising: bool,
}

/// Managed virtual device container holding device state and transport channel.
pub struct ManagedDevice {
    /// Device.
    pub device: Arc<Mutex<VirtualDevice>>,
    /// Hci channel.
    pub hci_channel: Arc<HciChannel>,
}

/// Central registry and lifecycle manager for all simulated Bluetooth devices.
#[derive(Default)]
pub struct SimbleManager {
    devices: Mutex<HashMap<String, ManagedDevice>>,
}

impl SimbleManager {
    /// Creates a new empty SimbleManager instance.
    pub fn new() -> Self {
        Self {
            devices: Mutex::new(HashMap::new()),
        }
    }

    /// Creates and registers a new virtual device based on a `CreateDeviceRequest`.
    pub fn create_device(
        &self,
        req: CreateDeviceRequest,
    ) -> Result<CreateDeviceResponse, SimbleError> {
        let addr = req
            .address
            .unwrap_or_else(|| Address::from_be_bytes([0xF0, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5]));
        let device_id = if req.name.is_empty() {
            format!(
                "simble-{}",
                addr.to_string().replace(':', "").to_lowercase()
            )
        } else {
            req.name.clone()
        };

        let dev = match req.predefined_template.as_deref() {
            Some("heart_rate_monitor") => HeartRateMonitor::new(&req.name, addr).device,
            Some("keyboard") => BleKeyboard::new(&req.name, addr).device,
            Some("mouse") => BleMouse::new(&req.name, addr).device,
            Some("eddystone_beacon") => {
                let namespace = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A];
                let instance = [0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0x10];
                EddystoneUidBeacon::new(&req.name, addr, &namespace, &instance, -20).device
            }
            Some("ibeacon") => {
                let uuid = [0xAA; 16];
                IBeacon::new(
                    &req.name,
                    addr,
                    crate::types::Uuid::Uuid128(uuid),
                    100,
                    1,
                    -59,
                )
                .device
            }
            _ => VirtualDevice::new(&req.name, addr, req.address_type),
        };

        let hci_channel = Arc::new(HciChannel::new());
        let managed = ManagedDevice {
            device: Arc::new(Mutex::new(dev)),
            hci_channel,
        };

        let mut devices = self.devices.lock().unwrap();
        devices.insert(device_id.clone(), managed);

        Ok(CreateDeviceResponse {
            device_id,
            address: addr,
            status: "CREATED".to_string(),
        })
    }

    /// Retrieves a reference to a registered VirtualDevice by ID.
    pub fn get_device(&self, device_id: &str) -> Option<Arc<Mutex<VirtualDevice>>> {
        let devices = self.devices.lock().unwrap();
        devices.get(device_id).map(|m| Arc::clone(&m.device))
    }

    /// Retrieves a reference to the HCI transport channel for a device.
    pub fn get_hci_channel(&self, device_id: &str) -> Option<Arc<HciChannel>> {
        let devices = self.devices.lock().unwrap();
        devices.get(device_id).map(|m| Arc::clone(&m.hci_channel))
    }

    /// Removes a virtual device from the registry.
    pub fn remove_device(&self, device_id: &str) -> bool {
        let mut devices = self.devices.lock().unwrap();
        devices.remove(device_id).is_some()
    }

    /// Lists all registered devices with their current advertising status.
    pub fn list_devices(&self) -> Vec<DeviceSummary> {
        let devices = self.devices.lock().unwrap();
        devices
            .iter()
            .map(|(id, m)| {
                let dev = m.device.lock().unwrap();
                DeviceSummary {
                    device_id: id.clone(),
                    name: dev.name.clone(),
                    address: dev.address.to_string(),
                    is_advertising: dev.advertising_data.is_some(),
                }
            })
            .collect()
    }

    /// Updates a characteristic value in a device's GATT database and optionally sends notifications.
    pub fn write_characteristic(
        &self,
        device_id: &str,
        handle: u16,
        value: &[u8],
        notify: bool,
    ) -> Result<Option<Vec<u8>>, SimbleError> {
        let dev_arc = self
            .get_device(device_id)
            .ok_or_else(|| SimbleError::DeviceNotFound(device_id.to_string()))?;
        let mut dev = dev_arc.lock().unwrap();

        dev.gatt_db
            .set_value(handle, value)
            .map_err(|e| SimbleError::Gatt(format!("GATT error code {e}")))?;

        if notify {
            let pdu = dev.create_notification(handle, value);
            Ok(Some(pdu))
        } else {
            Ok(None)
        }
    }

    /// Updates the advertising state and parameters for a device.
    pub fn set_advertising(
        &self,
        device_id: &str,
        req: SetAdvertisingRequest,
    ) -> Result<(), SimbleError> {
        let dev_arc = self
            .get_device(device_id)
            .ok_or_else(|| SimbleError::DeviceNotFound(device_id.to_string()))?;
        let mut dev = dev_arc.lock().unwrap();

        if req.enabled {
            let mut ad = crate::gap::AdvertisingData::new();
            if let Some(name) = req.complete_local_name {
                ad = ad.with_name(name);
            } else if !dev.name.is_empty() {
                let name = dev.name.clone();
                ad = ad.with_name(name);
            }
            dev.advertising_data = Some(ad);
        } else {
            dev.advertising_data = None;
        }

        Ok(())
    }

    /// Dispatches an HTTP REST request to the appropriate Simble handler.
    ///
    /// Returns (HTTP Status Code, JSON response bytes).
    pub fn handle_http_request(&self, method: &str, path: &str, body: &[u8]) -> (u16, Vec<u8>) {
        match (method, path) {
            ("POST", "/v1/simble/devices") => {
                match serde_json::from_slice::<CreateDeviceRequest>(body) {
                    Ok(req) => match self.create_device(req) {
                        Ok(resp) => (200, serde_json::to_vec(&resp).unwrap()),
                        Err(e) => (400, format!(r#"{{"error":"{e}"}}"#).into_bytes()),
                    },
                    Err(e) => (
                        400,
                        format!(r#"{{"error":"Invalid JSON: {e}"}}"#).into_bytes(),
                    ),
                }
            }
            ("GET", "/v1/simble/devices") => {
                let list = self.list_devices();
                (200, serde_json::to_vec(&list).unwrap())
            }
            _ => {
                // Check parameterized routes: /v1/simble/devices/{id}/...
                if let Some(rest) = path.strip_prefix("/v1/simble/devices/") {
                    if let Some((device_id, action)) = rest.split_once('/') {
                        if action == "advertising" && method == "POST" {
                            match serde_json::from_slice::<SetAdvertisingRequest>(body) {
                                Ok(req) => match self.set_advertising(device_id, req) {
                                    Ok(()) => (200, br#"{"status":"OK"}"#.to_vec()),
                                    Err(e) => (400, format!(r#"{{"error":"{e}"}}"#).into_bytes()),
                                },
                                Err(e) => (
                                    400,
                                    format!(r#"{{"error":"Invalid JSON: {e}"}}"#).into_bytes(),
                                ),
                            }
                        } else if let Some(handle_str) = action.strip_prefix("characteristics/") {
                            if method == "PUT" {
                                if let Ok(handle) = handle_str.parse::<u16>() {
                                    match self.write_characteristic(device_id, handle, body, true) {
                                        Ok(_) => (200, br#"{"status":"OK"}"#.to_vec()),
                                        Err(e) => {
                                            (400, format!(r#"{{"error":"{e}"}}"#).into_bytes())
                                        }
                                    }
                                } else {
                                    (400, br#"{"error":"Invalid handle parameter"}"#.to_vec())
                                }
                            } else {
                                (405, br#"{"error":"Method Not Allowed"}"#.to_vec())
                            }
                        } else {
                            (404, br#"{"error":"Not Found"}"#.to_vec())
                        }
                    } else if method == "DELETE" {
                        if self.remove_device(rest) {
                            (200, br#"{"status":"DELETED"}"#.to_vec())
                        } else {
                            (404, br#"{"error":"Device not found"}"#.to_vec())
                        }
                    } else {
                        (404, br#"{"error":"Not Found"}"#.to_vec())
                    }
                } else {
                    (404, br#"{"error":"Not Found"}"#.to_vec())
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AddressType;

    #[test]
    fn test_simble_manager_device_lifecycle() {
        let manager = SimbleManager::new();

        // 1. Create a Heart Rate Monitor from predefined template
        let create_req = CreateDeviceRequest {
            name: "MyHeartRate".to_string(),
            address: Some(Address::from_be_bytes([0x11, 0x22, 0x33, 0x44, 0x55, 0x66])),
            address_type: AddressType::Random,
            role: crate::api::dto::DeviceRole::Peripheral,
            predefined_template: Some("heart_rate_monitor".to_string()),
        };
        let resp = manager.create_device(create_req).expect("create device");
        assert_eq!(resp.device_id, "MyHeartRate");

        // 2. List devices
        let devices = manager.list_devices();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].name, "MyHeartRate");

        // 3. Write characteristic and notify
        // Battery level is handle 6 in the HRM device
        let notif = manager
            .write_characteristic("MyHeartRate", 6, &[95], true)
            .expect("write");
        assert!(notif.is_some());

        // 4. Remove device
        assert!(manager.remove_device("MyHeartRate"));
        assert_eq!(manager.list_devices().len(), 0);
    }

    #[test]
    fn test_simble_manager_http_routing() {
        let manager = SimbleManager::new();

        // POST /v1/simble/devices
        let body = br#"{"name":"TestDev","address":"AA:BB:CC:DD:EE:FF","address_type":"random","role":"peripheral","predefined_template":"keyboard"}"#;
        let (code, resp) = manager.handle_http_request("POST", "/v1/simble/devices", body);
        assert_eq!(code, 200);
        assert!(String::from_utf8_lossy(&resp).contains("TestDev"));

        // GET /v1/simble/devices
        let (code, resp) = manager.handle_http_request("GET", "/v1/simble/devices", &[]);
        assert_eq!(code, 200);
        assert!(String::from_utf8_lossy(&resp).contains("TestDev"));

        // DELETE /v1/simble/devices/TestDev
        let (code, _) = manager.handle_http_request("DELETE", "/v1/simble/devices/TestDev", &[]);
        assert_eq!(code, 200);
    }
}
