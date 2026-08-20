// Copyright 2026 The Android Open Source Project
// SPDX-License-Identifier: Apache-2.0

pub mod hid_reports;

pub use hid_reports::{KEYBOARD_REPORT_MAP, MOUSE_REPORT_MAP, ascii_to_hid, keycode, modifier};
