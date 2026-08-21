// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Simulated Bluetooth *controller*-layer behavior — protocols that run
//! between two controllers (chips), below HCI, as opposed to the host-stack
//! layers under `classic`/`gatt`/`smp`/etc. Real controllers (or Rootcanal,
//! reached via `transport::rootcanal`/`transport::netsim`) already implement
//! this; this module exists as a self-contained mock controller for
//! simulating two Bluetooth devices connecting with no real/virtual
//! controller present at all.

pub mod lmp;
pub mod sim;
