// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Common HOGP (HID Over GATT Profile) device setup and report dispatch helper.

use crate::device::VirtualDevice;
use crate::gap::{AdvertisingData, flags};
use crate::gatt::{AttributePermissions, CharacteristicProperties};
use crate::profiles::{BatteryService, DeviceInformationService};
use crate::types::{Address, AddressType, Uuid};

/// Standard HOGP characteristic UUIDs.
pub mod hogp_uuid {
    /// HID Service (0x1812).
    pub const HID_SERVICE: u16 = 0x1812;
    /// Protocol Mode characteristic (0x2A4E).
    pub const PROTOCOL_MODE: u16 = 0x2A4E;
    /// Report characteristic (0x2A4D).
    pub const REPORT: u16 = 0x2A4D;
    /// Report Map characteristic (0x2A4B).
    pub const REPORT_MAP: u16 = 0x2A4B;
    /// HID Information characteristic (0x2A4A).
    pub const HID_INFORMATION: u16 = 0x2A4A;
    /// HID Control Point characteristic (0x2A4C).
    pub const HID_CONTROL_POINT: u16 = 0x2A4C;
}

/// Configuration parameters for initializing a standard HID device.
pub(crate) struct HidDeviceConfig<'a> {
    /// Device name advertised over GAP.
    pub name: String,
    /// Model number reported by the Device Information Service.
    pub model_number: &'static str,
    /// The device's Bluetooth address.
    pub address: Address,
    /// The USB HID report descriptor exposed as the Report Map.
    pub report_map: &'a [u8],
    /// Initial length (in bytes) of the input report value.
    pub initial_report_len: usize,
}

/// Initialized common HID components.
pub(crate) struct CommonHidComponents {
    /// The fully configured virtual device.
    pub device: VirtualDevice,
    /// Attribute handle of the HID input report characteristic value.
    pub input_report_val_handle: u16,
    /// Attribute handle of the input report's Client Characteristic Configuration descriptor.
    pub input_report_cccd_handle: u16,
    /// The Battery Service registered on the device.
    pub bas: BatteryService,
}

/// Constructs and registers all standard HOGP services, characteristics, and advertising data.
pub(crate) fn setup_common_hid_device(config: HidDeviceConfig<'_>) -> CommonHidComponents {
    let mut dev = VirtualDevice::new(&config.name, config.address, AddressType::Random);

    // 1. Device Information Service
    DeviceInformationService::new()
        .with_manufacturer_name("Google Simulated")
        .with_model_number(config.model_number)
        .with_firmware_revision("1.0.0")
        .register(&mut dev.gatt_db);

    // 2. Battery Service
    let bas = BatteryService::register(&mut dev.gatt_db, 100);

    // 3. Human Interface Device Service (0x1812)
    dev.gatt_db
        .add_service(Uuid::from_u16(hogp_uuid::HID_SERVICE), true);

    // Protocol Mode (0x2A4E): 0x01 = Report Protocol Mode
    dev.gatt_db.add_characteristic(
        Uuid::from_u16(hogp_uuid::PROTOCOL_MODE),
        CharacteristicProperties(
            CharacteristicProperties::READ | CharacteristicProperties::WRITE_WITHOUT_RESPONSE,
        ),
        vec![0x01],
        AttributePermissions::default(),
    );

    // HID Information (0x2A4A): bcdHID (0x0111), bCountryCode (0x00), Flags (0x02 = RemoteWake)
    dev.gatt_db.add_characteristic(
        Uuid::from_u16(hogp_uuid::HID_INFORMATION),
        CharacteristicProperties(CharacteristicProperties::READ),
        vec![0x11, 0x01, 0x00, 0x02],
        AttributePermissions::default(),
    );

    // HID Control Point (0x2A4C)
    dev.gatt_db.add_characteristic(
        Uuid::from_u16(hogp_uuid::HID_CONTROL_POINT),
        CharacteristicProperties(CharacteristicProperties::WRITE_WITHOUT_RESPONSE),
        vec![0x00],
        AttributePermissions::default(),
    );

    // Report Map (0x2A4B): USB HID Report Descriptor
    dev.gatt_db.add_characteristic(
        Uuid::from_u16(hogp_uuid::REPORT_MAP),
        CharacteristicProperties(CharacteristicProperties::READ),
        config.report_map.to_vec(),
        AttributePermissions::default(),
    );

    // Input Report (0x2A4D)
    let (_, input_report_val_handle) = dev.gatt_db.add_characteristic(
        Uuid::from_u16(hogp_uuid::REPORT),
        CharacteristicProperties(CharacteristicProperties::READ | CharacteristicProperties::NOTIFY),
        vec![0u8; config.initial_report_len],
        AttributePermissions::default(),
    );
    let input_report_cccd_handle = dev.gatt_db.add_cccd();

    // 4. Advertising Data with General Discoverable and HID/Battery UUIDs
    let ad = AdvertisingData::new()
        .with_flags(flags::LE_GENERAL_DISCOVERABLE | flags::BR_EDR_NOT_SUPPORTED)
        .with_name(&config.name)
        .with_service_uuid_16(hogp_uuid::HID_SERVICE)
        .with_service_uuid_16(0x180F);

    dev.advertising_data = Some(ad);
    dev.is_advertising = true;

    CommonHidComponents {
        device: dev,
        input_report_val_handle,
        input_report_cccd_handle,
        bas,
    }
}
