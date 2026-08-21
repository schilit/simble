// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Bluetooth Direction Finding (AoA/AoD): Constant Tone Extension (CTE) HCI
//! packets and host-side IQ-sample angle-of-arrival/departure estimation —
//! the angular sibling to `cs` (Channel Sounding), which does distance.

pub mod packets;
pub mod procedures;
