// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Generic (Android-agnostic) observer hook for real ATT server dispatch
//! events. `device/` stays free of any dependency on `crate::android`; the
//! `android` adapter layer implements this trait to translate real events
//! into `android.bluetooth`-shaped callback calls.

use crate::types::Address;

/// Why a subscription state change is being reported, mirroring the
/// `cur_notify`/`prev_notify` + reason shape of NimBLE's
/// `BLE_GAP_EVENT_SUBSCRIBE` (`BLE_GAP_SUBSCRIBE_REASON_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscriptionReason {
    /// The peer wrote the CCCD over ATT.
    Write,
    /// The subscribed peer disconnected; per Core Spec Vol 3, Part G,
    /// Section 3.3.3.3 CCCD state is per-client, so the live value resets.
    Disconnect,
    /// A bonded peer reconnected and its persisted CCCD state was restored
    /// from the bond store.
    BondRestore,
}

/// Observes real events dispatched by a [`crate::device::VirtualDevice`]'s
/// ATT server engine. Every method has a default no-op body so implementors
/// only override what they care about.
///
/// Requires `Send + Sync` so `VirtualDevice` (which holds
/// `Option<Box<dyn AttServerObserver>>`) stays `Send + Sync` itself —
/// several callers (e.g. `service::manager`) put `VirtualDevice` behind
/// `Arc<Mutex<_>>`.
pub trait AttServerObserver: Send + Sync {
    /// A peer requested to read `attribute_handle` (via Read Request or Read
    /// Blob Request, the latter carrying a non-zero `offset`).
    fn on_characteristic_read(
        &mut self,
        _connection_handle: u16,
        _attribute_handle: u16,
        _offset: u16,
    ) {
    }

    /// A peer requested to write `value` to `attribute_handle`.
    /// `response_needed` is true for Write Request (which expects a Write
    /// Response) and false for Write Command (fire-and-forget).
    fn on_characteristic_write(
        &mut self,
        _connection_handle: u16,
        _attribute_handle: u16,
        _value: &[u8],
        _response_needed: bool,
    ) {
    }

    /// The connection to `peer_address` was established (`connected = true`)
    /// or torn down (`connected = false`).
    fn on_connection_state_changed(
        &mut self,
        _connection_handle: u16,
        _peer_address: Address,
        _connected: bool,
    ) {
    }

    /// The ATT MTU for a connection was negotiated/changed.
    fn on_mtu_changed(&mut self, _connection_handle: u16, _mtu: u16) {}

    /// A CCCD's notify/indicate subscription bits changed for a connection —
    /// via a peer write, a disconnect, or bond restore on reconnect (see
    /// [`SubscriptionReason`]). Fired only on an actual bit transition, so
    /// rewriting an identical CCCD value is silent.
    ///
    /// NimBLE's subscribe event carries exactly these prev/cur fields; a
    /// params struct would be ceremony for a default-no-op hook, hence the
    /// lint allowance.
    #[allow(clippy::too_many_arguments)]
    fn on_subscription_changed(
        &mut self,
        _connection_handle: u16,
        _cccd_handle: u16,
        _prev_notify: bool,
        _cur_notify: bool,
        _prev_indicate: bool,
        _cur_indicate: bool,
        _reason: SubscriptionReason,
    ) {
    }

    /// A Handle Value Notification (`indication = false`) or Indication
    /// (`indication = true`) was sent for `attribute_handle`.
    fn on_notification_sent(
        &mut self,
        _connection_handle: u16,
        _attribute_handle: u16,
        _indication: bool,
    ) {
    }
}
