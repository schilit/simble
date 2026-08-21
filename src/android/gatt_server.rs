// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! `BluetoothGattServer`/`BluetoothGattServerCallback`-shaped API, mirroring
//! `android.bluetooth.BluetoothGattServer` and
//! `android.bluetooth.BluetoothGattServerCallback`. `BluetoothGattServer`
//! wraps a real `VirtualDevice`; `add_service` registers attributes in its
//! `GattDatabase`, and `BluetoothGattServer` implements the device-agnostic
//! `AttServerObserver` trait so real ATT dispatch events (see
//! `crate::device::att_server`) are translated into real
//! `BluetoothGattServerCallback` calls — not just cosmetic shapes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::device::{AttServerObserver, VirtualDevice};
use crate::types::{Address, AddressType, Uuid};

use super::device::BluetoothDevice;
use super::gatt_service::{
    BluetoothGatt, BluetoothGattCharacteristic, BluetoothGattDescriptor, BluetoothGattService,
    permissions_from_android, properties_from_android,
};

/// Connection-state constants mirroring `android.bluetooth.BluetoothProfile`.
pub struct BluetoothProfile;

impl BluetoothProfile {
    /// State disconnected.
    pub const STATE_DISCONNECTED: i32 = 0;
    /// State connecting.
    pub const STATE_CONNECTING: i32 = 1;
    /// State connected.
    pub const STATE_CONNECTED: i32 = 2;
    /// State disconnecting.
    pub const STATE_DISCONNECTING: i32 = 3;
}

/// Mirrors `android.bluetooth.BluetoothGattServerCallback`: every method has
/// a default no-op body, so implementors only override what they care about.
/// Requires `Send` since it's held behind an `Arc<Mutex<_>>` alongside
/// `AttServerObserver` (see [`AttServerObserver`]'s doc comment).
pub trait BluetoothGattServerCallback: Send {
    /// Mirrors `BluetoothGattServerCallback.onConnectionStateChange`: a peer
    /// connected or disconnected.
    fn on_connection_state_change(
        &mut self,
        _device: &BluetoothDevice,
        _status: i32,
        _new_state: i32,
    ) {
    }
    /// Mirrors `BluetoothGattServerCallback.onServiceAdded`: a service finished
    /// registering.
    fn on_service_added(&mut self, _status: i32, _service: &BluetoothGattService) {}
    /// Mirrors `BluetoothGattServerCallback.onCharacteristicReadRequest`.
    fn on_characteristic_read_request(
        &mut self,
        _device: &BluetoothDevice,
        _request_id: i32,
        _offset: i32,
        _characteristic: &BluetoothGattCharacteristic,
    ) {
    }
    /// Mirrors `BluetoothGattServerCallback.onCharacteristicWriteRequest`.
    #[allow(clippy::too_many_arguments)]
    fn on_characteristic_write_request(
        &mut self,
        _device: &BluetoothDevice,
        _request_id: i32,
        _characteristic: &BluetoothGattCharacteristic,
        _prepared_write: bool,
        _response_needed: bool,
        _offset: i32,
        _value: &[u8],
    ) {
    }
    /// Mirrors `BluetoothGattServerCallback.onDescriptorReadRequest`.
    fn on_descriptor_read_request(
        &mut self,
        _device: &BluetoothDevice,
        _request_id: i32,
        _offset: i32,
        _descriptor: &BluetoothGattDescriptor,
    ) {
    }
    /// Mirrors `BluetoothGattServerCallback.onDescriptorWriteRequest`.
    #[allow(clippy::too_many_arguments)]
    fn on_descriptor_write_request(
        &mut self,
        _device: &BluetoothDevice,
        _request_id: i32,
        _descriptor: &BluetoothGattDescriptor,
        _prepared_write: bool,
        _response_needed: bool,
        _offset: i32,
        _value: &[u8],
    ) {
    }
    /// Mirrors `BluetoothGattServerCallback.onExecuteWrite`.
    fn on_execute_write(&mut self, _device: &BluetoothDevice, _request_id: i32, _execute: bool) {}
    /// Mirrors `BluetoothGattServerCallback.onNotificationSent`.
    fn on_notification_sent(&mut self, _device: &BluetoothDevice, _status: i32) {}
    /// Mirrors `BluetoothGattServerCallback.onMtuChanged`.
    fn on_mtu_changed(&mut self, _device: &BluetoothDevice, _mtu: i32) {}
}

struct CallbackState {
    callback: Option<Box<dyn BluetoothGattServerCallback>>,
    services: HashMap<Uuid, BluetoothGattService>,
    characteristics: HashMap<u16, BluetoothGattCharacteristic>,
    descriptors: HashMap<u16, BluetoothGattDescriptor>,
    connections: HashMap<u16, Address>,
    next_request_id: i32,
    pending_requests: HashMap<i32, u16>,
}

impl CallbackState {
    fn device_for(&self, connection_handle: u16) -> Option<BluetoothDevice> {
        self.connections
            .get(&connection_handle)
            .map(|&address| BluetoothDevice::new(address, AddressType::Public))
    }

    fn allocate_request_id(&mut self, attribute_handle: u16) -> i32 {
        let id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1);
        self.pending_requests.insert(id, attribute_handle);
        id
    }

    fn dispatch_read(&mut self, connection_handle: u16, attribute_handle: u16, offset: u16) {
        let Some(device) = self.device_for(connection_handle) else {
            return;
        };
        let request_id = self.allocate_request_id(attribute_handle);
        if let Some(descriptor) = self.descriptors.get(&attribute_handle)
            && let Some(cb) = self.callback.as_deref_mut()
        {
            cb.on_descriptor_read_request(&device, request_id, offset as i32, descriptor);
        } else if let Some(characteristic) = self.characteristics.get(&attribute_handle)
            && let Some(cb) = self.callback.as_deref_mut()
        {
            cb.on_characteristic_read_request(&device, request_id, offset as i32, characteristic);
        }
    }

    fn dispatch_write(
        &mut self,
        connection_handle: u16,
        attribute_handle: u16,
        value: &[u8],
        response_needed: bool,
    ) {
        let Some(device) = self.device_for(connection_handle) else {
            return;
        };
        let request_id = self.allocate_request_id(attribute_handle);
        if let Some(descriptor) = self.descriptors.get_mut(&attribute_handle) {
            descriptor.value = value.to_vec();
            if let Some(cb) = self.callback.as_deref_mut() {
                cb.on_descriptor_write_request(
                    &device,
                    request_id,
                    descriptor,
                    false,
                    response_needed,
                    0,
                    value,
                );
            }
        } else if let Some(characteristic) = self.characteristics.get_mut(&attribute_handle) {
            characteristic.value = value.to_vec();
            if let Some(cb) = self.callback.as_deref_mut() {
                cb.on_characteristic_write_request(
                    &device,
                    request_id,
                    characteristic,
                    false,
                    response_needed,
                    0,
                    value,
                );
            }
        }
    }

    fn dispatch_connection_state(
        &mut self,
        connection_handle: u16,
        peer_address: Address,
        connected: bool,
    ) {
        if connected {
            self.connections.insert(connection_handle, peer_address);
        }
        let device = BluetoothDevice::new(peer_address, AddressType::Public);
        let new_state = if connected {
            BluetoothProfile::STATE_CONNECTED
        } else {
            BluetoothProfile::STATE_DISCONNECTED
        };
        if let Some(cb) = self.callback.as_deref_mut() {
            cb.on_connection_state_change(&device, BluetoothGatt::GATT_SUCCESS, new_state);
        }
        if !connected {
            self.connections.remove(&connection_handle);
        }
    }

    fn dispatch_mtu(&mut self, connection_handle: u16, mtu: u16) {
        let Some(device) = self.device_for(connection_handle) else {
            return;
        };
        if let Some(cb) = self.callback.as_deref_mut() {
            cb.on_mtu_changed(&device, mtu as i32);
        }
    }

    fn dispatch_notification_sent(
        &mut self,
        connection_handle: u16,
        _attribute_handle: u16,
        _indication: bool,
    ) {
        let Some(device) = self.device_for(connection_handle) else {
            return;
        };
        if let Some(cb) = self.callback.as_deref_mut() {
            cb.on_notification_sent(&device, BluetoothGatt::GATT_SUCCESS);
        }
    }
}

// `VirtualDevice::observer` is owned by the `VirtualDevice`, which is itself
// owned by `BluetoothGattServer` — so the observer can't hold a reference or
// owned pointer back to the `BluetoothGattServer` without a cycle. Instead it
// holds a shared handle to just the callback/bookkeeping state
// (`CallbackState`), which never points back to the device or the server.
struct ServerObserverBridge(Arc<Mutex<CallbackState>>);

impl AttServerObserver for ServerObserverBridge {
    fn on_characteristic_read(
        &mut self,
        connection_handle: u16,
        attribute_handle: u16,
        offset: u16,
    ) {
        self.0
            .lock()
            .unwrap()
            .dispatch_read(connection_handle, attribute_handle, offset);
    }

    fn on_characteristic_write(
        &mut self,
        connection_handle: u16,
        attribute_handle: u16,
        value: &[u8],
        response_needed: bool,
    ) {
        self.0.lock().unwrap().dispatch_write(
            connection_handle,
            attribute_handle,
            value,
            response_needed,
        );
    }

    fn on_connection_state_changed(
        &mut self,
        connection_handle: u16,
        peer_address: Address,
        connected: bool,
    ) {
        self.0.lock().unwrap().dispatch_connection_state(
            connection_handle,
            peer_address,
            connected,
        );
    }

    fn on_mtu_changed(&mut self, connection_handle: u16, mtu: u16) {
        self.0.lock().unwrap().dispatch_mtu(connection_handle, mtu);
    }

    fn on_notification_sent(
        &mut self,
        connection_handle: u16,
        attribute_handle: u16,
        indication: bool,
    ) {
        self.0.lock().unwrap().dispatch_notification_sent(
            connection_handle,
            attribute_handle,
            indication,
        );
    }
}

/// Mirrors `android.bluetooth.BluetoothGattServer`: wraps a real
/// `VirtualDevice` and drives its `GattDatabase`/ATT dispatch under an
/// Android-shaped API.
pub struct BluetoothGattServer {
    /// The underlying Simble device this server drives.
    pub device: VirtualDevice,
    state: Arc<Mutex<CallbackState>>,
}

impl BluetoothGattServer {
    /// Wraps a `VirtualDevice` and registers `callback` to receive ATT
    /// events. Mirrors obtaining a `BluetoothGattServer` via
    /// `BluetoothManager.openGattServer`.
    pub fn new(mut device: VirtualDevice, callback: Box<dyn BluetoothGattServerCallback>) -> Self {
        let state = Arc::new(Mutex::new(CallbackState {
            callback: Some(callback),
            services: HashMap::new(),
            characteristics: HashMap::new(),
            descriptors: HashMap::new(),
            connections: HashMap::new(),
            next_request_id: 0,
            pending_requests: HashMap::new(),
        }));
        device.observer = Some(Box::new(ServerObserverBridge(state.clone())));
        Self { device, state }
    }

    /// Registers a service (and its characteristics/descriptors) in the
    /// underlying `GattDatabase`. Mirrors `BluetoothGattServer.addService`.
    pub fn add_service(&mut self, service: BluetoothGattService) -> bool {
        let primary = service.service_type != BluetoothGattService::SERVICE_TYPE_SECONDARY;
        self.device.gatt_db.add_service(service.uuid, primary);

        let mut registered = service;
        for characteristic in registered.characteristics.iter_mut() {
            let properties = properties_from_android(characteristic.properties);
            let permissions = permissions_from_android(characteristic.permissions);
            let (_, value_handle) = self.device.gatt_db.add_characteristic(
                characteristic.uuid,
                properties,
                characteristic.value.clone(),
                permissions,
            );
            characteristic.value_handle = Some(value_handle);

            for descriptor in characteristic.descriptors.iter_mut() {
                let descriptor_permissions = permissions_from_android(descriptor.permissions);
                let handle = self.device.gatt_db.add_descriptor(
                    descriptor.uuid,
                    descriptor.value.clone(),
                    descriptor_permissions,
                );
                descriptor.handle = Some(handle);
            }
        }

        let mut state = self.state.lock().unwrap();
        for characteristic in &registered.characteristics {
            if let Some(handle) = characteristic.value_handle {
                state.characteristics.insert(handle, characteristic.clone());
            }
            for descriptor in &characteristic.descriptors {
                if let Some(handle) = descriptor.handle {
                    state.descriptors.insert(handle, descriptor.clone());
                }
            }
        }
        state.services.insert(registered.uuid, registered.clone());
        if let Some(cb) = state.callback.as_deref_mut() {
            cb.on_service_added(BluetoothGatt::GATT_SUCCESS, &registered);
        }

        true
    }

    /// Unregisters a service's handles from this server's request/response
    /// translation. Simble's `GattDatabase` has no attribute-removal
    /// primitive (add-only, matching the rest of its GATT engine), so the
    /// underlying attributes stay in the database — this only stops routing
    /// read/write requests for them to the app's callback, matching
    /// `BluetoothGattServer.removeService`'s externally-visible effect.
    pub fn remove_service(&mut self, service: &BluetoothGattService) -> bool {
        let mut state = self.state.lock().unwrap();
        let mut removed = state.services.remove(&service.uuid).is_some();
        for characteristic in &service.characteristics {
            if let Some(handle) = characteristic.value_handle {
                removed |= state.characteristics.remove(&handle).is_some();
            }
            for descriptor in &characteristic.descriptors {
                if let Some(handle) = descriptor.handle {
                    state.descriptors.remove(&handle);
                }
            }
        }
        removed
    }

    /// Mirrors `BluetoothGattServer.getService`: returns the registered
    /// (handle-populated) service for `uuid`, if any.
    pub fn get_service(&self, uuid: Uuid) -> Option<BluetoothGattService> {
        self.state.lock().unwrap().services.get(&uuid).cloned()
    }

    /// Mirrors `BluetoothGattServer.getServices`.
    pub fn get_services(&self) -> Vec<BluetoothGattService> {
        self.state
            .lock()
            .unwrap()
            .services
            .values()
            .cloned()
            .collect()
    }

    /// Sends a notification (`confirm = false`) or indication
    /// (`confirm = true`) for `characteristic` to `device`. Mirrors
    /// `BluetoothGattServer.notifyCharacteristicChanged`, which as of API
    /// level 33 returns `int` (a `BluetoothStatusCodes`/`BluetoothGatt`
    /// status) rather than the pre-33 `boolean` — this adapter targets that
    /// current signature.
    pub fn notify_characteristic_changed(
        &mut self,
        device: &BluetoothDevice,
        characteristic: &BluetoothGattCharacteristic,
        confirm: bool,
    ) -> i32 {
        let Some(handle) = characteristic.value_handle else {
            return BluetoothGatt::GATT_FAILURE;
        };
        let connection_handle = self
            .device
            .connections
            .iter()
            .find(|(_, conn)| conn.peer_address == device.address)
            .map(|(&handle, _)| handle);
        let Some(connection_handle) = connection_handle else {
            return BluetoothGatt::GATT_FAILURE;
        };

        if confirm {
            match self
                .device
                .create_indication(connection_handle, handle, &characteristic.value)
            {
                Ok(_) => BluetoothGatt::GATT_SUCCESS,
                Err(_) => BluetoothGatt::GATT_FAILURE,
            }
        } else {
            let _ = self.device.create_notification_for(
                connection_handle,
                handle,
                &characteristic.value,
            );
            BluetoothGatt::GATT_SUCCESS
        }
    }

    /// Answers a pending read/write request. Simble's ATT dispatch is
    /// synchronous (the real ATT-level response was already produced and
    /// returned by the time the app-level callback fired), so this can't
    /// retroactively rewrite the packet already sent; on `GATT_SUCCESS` it
    /// still applies `value` to the attribute, so later reads see whatever
    /// the app decided to answer with.
    pub fn send_response(
        &mut self,
        _device: &BluetoothDevice,
        request_id: i32,
        status: i32,
        _offset: i32,
        value: &[u8],
    ) {
        let mut state = self.state.lock().unwrap();
        if let Some(handle) = state.pending_requests.remove(&request_id)
            && status == BluetoothGatt::GATT_SUCCESS
            && !value.is_empty()
        {
            let _ = self.device.gatt_db.set_value(handle, value);
        }
    }

    /// Mirrors `BluetoothGattServer.close`: stops delivering callbacks.
    pub fn close(&mut self) {
        self.state.lock().unwrap().callback = None;
    }
}
