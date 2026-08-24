// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! SMP pairing session state machine (Bluetooth Core Spec Vol 3, Part H,
//! Sections 2 and 3): drives Pairing Request/Response, Confirm/Random (LE
//! Legacy via `c1`/`s1`, LE Secure Connections via `f4`/`f5`/`f6`/`g2`),
//! DHKey Check, and bonding key distribution (LTK/EDIV/RAND, IRK, identity
//! address) through to completion, plus the `h6`/`h7`-based LTK<->link-key
//! conversion used for BR/EDR<->LE cross-transport key derivation.
//!
//! LE Secure Connections runs real P-256 ECDH (`crypto::ecdh`), with peer
//! public keys validated to be on the curve — required to pair against
//! real stacks, which reject invalid points (CVE-2018-5383). Only Just
//! Works / Numeric Comparison association (auto-confirmed, no user prompt)
//! is implemented; OOB and Passkey Entry are out of scope for a headless
//! simulator with no display/keyboard to model.
//!
//! # Failure paths
//!
//! Three rules from Section 3.4 and Section 3.5.5 govern what a session does
//! when things go wrong, and all three are load-bearing rather than cosmetic:
//!
//! - **The Security Manager Timer** (Section 3.4). "If the Security Manager
//!   Timer reaches 30 seconds, the procedure shall be considered to have
//!   failed... No further SMP commands shall be sent over the L2CAP Security
//!   Manager Channel. A new Pairing process shall only be performed when a new
//!   physical link has been established." A `PairingSession` has no clock of
//!   its own — see [`PairingSession::tick`] for how simulated seconds reach it.
//! - **Nothing is processed after a failure.** A Pairing Failed "reports that
//!   the pairing procedure has been stopped and no further communication for
//!   the current pairing procedure is to occur" (Section 3.5.5), and after a
//!   timeout no SMP command may be *sent* at all (Section 3.4) — so a failed
//!   session drops later PDUs silently rather than answering each with another
//!   Pairing Failed.
//! - **Per-opcode parameter validation.** Reason code 0x0A "indicates that the
//!   command length is invalid or that a parameter is outside of the specified
//!   range" (Table 3.7), which is exactly the two checks every opcode arm
//!   needs: an exact PDU length, and fields inside the ranges Tables 3.4–3.6
//!   define.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};

use zerocopy::IntoBytes;

use crate::crypto::{P256Keypair, c1, f4, f5, f6, h6, h7, s1};
use crate::packets::{
    smp_auth_req as auth_req, smp_error_code as error_code,
    smp_key_distribution as key_distribution,
};
use crate::smp::keystore::{PairingKey, PairingKeys};
use crate::smp::{SmpPairingPacket, io_capability, opcode};
use crate::types::{Address, AddressType, SimbleError};

/// Diffie-Hellman private/public key pair used in Debug Mode (Bluetooth Core
/// Spec Vol 3, Part H) — fixed, publicly-known values so pairing traces are
/// reproducible in tests, matching Bumble's `smp_debug_mode` behavior.
pub const SMP_DEBUG_KEY_PUBLIC_X: [u8; 32] = [
    0x20, 0xB0, 0x03, 0xD2, 0xF2, 0x97, 0xBE, 0x2C, 0x5E, 0x2C, 0x83, 0xA7, 0xE9, 0xF9, 0xA5, 0xB9,
    0xEF, 0xF4, 0x91, 0x11, 0xAC, 0xF4, 0xFD, 0xDB, 0xCC, 0x03, 0x01, 0x48, 0x0E, 0x35, 0x9D, 0xE6,
];
/// Y coordinate of the fixed Debug Mode P-256 public key (see
/// [`SMP_DEBUG_KEY_PUBLIC_X`]).
pub const SMP_DEBUG_KEY_PUBLIC_Y: [u8; 32] = [
    0xDC, 0x80, 0x9C, 0x49, 0x65, 0x2A, 0xEB, 0x6D, 0x63, 0x32, 0x9A, 0xBF, 0x5A, 0x52, 0x15, 0x5C,
    0x76, 0x63, 0x45, 0xC2, 0x8F, 0xED, 0x30, 0x24, 0x74, 0x1C, 0x8E, 0xD0, 0x15, 0x89, 0xD2, 0x8B,
];
/// The Debug Mode private scalar (big-endian, as the spec prints it) that
/// [`SMP_DEBUG_KEY_PUBLIC_X`]/`_Y` derive from — real ECDH runs with it in
/// debug mode so traces are reproducible AND cryptographically valid.
pub const SMP_DEBUG_KEY_PRIVATE_BE: [u8; 32] = [
    0x3F, 0x49, 0xF6, 0xD4, 0xA3, 0xC5, 0x5F, 0x38, 0x74, 0xC9, 0xB3, 0xE3, 0xD2, 0x10, 0x3F, 0x50,
    0x4A, 0xFF, 0x60, 0x7B, 0xEB, 0x40, 0xB7, 0x99, 0x58, 0x99, 0xB8, 0xA6, 0xCD, 0x3C, 0x1A, 0xBD,
];

/// The Security Manager Timeout, Bluetooth Core Spec Vol 3, Part H, Section
/// 3.4: "If the Security Manager Timer reaches 30 seconds, the procedure shall
/// be considered to have failed."
pub const SMP_TIMEOUT_SECONDS: f64 = 30.0;

/// SMP Pairing Keypress Notification (Core Spec Vol 3, Part H, Table 3.3, code
/// 0x0E). Not in `packets::smp::opcode` because nothing in this port builds
/// one — it exists here so the dispatch can tell a *defined* command it does
/// not implement (answer "Command Not Supported") from a code that is reserved
/// for future use (Section 3.3: "it shall be ignored").
const OPCODE_KEYPRESS_NOTIFICATION: u8 = 0x0E;

/// The highest defined OOB data flag value, Table 3.5: 0x00 is "not present",
/// 0x01 is "present", 0x02 to 0xFF are reserved for future use.
const OOB_DATA_PRESENT: u8 = 0x01;

/// AuthReq bits that carry no defined meaning (Figure 3.3 and Table 3.6):
/// bits 6-7 are reserved for future use, and bit 1 is the high bit of the
/// 2-bit Bonding_Flags field, whose values 0b10 and 0b11 are both reserved.
/// A peer that sets any of them has put a parameter outside its specified
/// range, which is what reason code 0x0A is for.
const AUTH_REQ_RESERVED: u8 = 0b1100_0010;

/// Maximum Encryption Key Size bounds, Section 2.3.4 / Section 3.5.1: "The
/// maximum key size shall be in the range 7 to 16 octets."
const MIN_ENCRYPTION_KEY_SIZE: u8 = 7;
/// Upper bound of the range described on [`MIN_ENCRYPTION_KEY_SIZE`].
const MAX_ENCRYPTION_KEY_SIZE: u8 = 16;

/// Exact wire lengths, in octets, of the fixed-size SMP PDUs this session
/// accepts — Code octet included (Section 3.3, Figure 3.1). Reason code 0x0A
/// covers "the command length is invalid", so each arm checks its own.
const LEN_PAIRING_PARAMETERS: usize = 7;
/// Length of a Pairing Failed PDU: Code + Reason (Figure 3.7).
const LEN_PAIRING_FAILED: usize = 2;

/// `h7` salts for cross-transport key derivation (CTKD), Bluetooth Core Spec
/// Vol 3, Part H, Section 2.4.2.4.
const SMP_CTKD_H7_LEBR_SALT: [u8; 16] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x74, 0x6D, 0x70, 0x31,
];
const SMP_CTKD_H7_BRLE_SALT: [u8; 16] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x74, 0x6D, 0x70, 0x32,
];

/// Which side of the pairing exchange this session represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The side that sends the Pairing Request (LE central).
    Initiator,
    /// The side that sends the Pairing Response (LE peripheral).
    Responder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    WaitPairingResponse,
    WaitPeerConfirm,
    WaitPeerRandom,
    WaitPeerPublicKey,
    WaitPeerDhKeyCheck,
    Complete,
    Failed,
}

/// Local device identity-address preference for the Identity Address
/// Information PDU (Bluetooth Core Spec Vol 3, Part H, Section 3.6.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityAddressPreference {
    /// Prefer distributing the public identity address.
    Public,
    /// Prefer distributing the random static identity address.
    Random,
}

/// Local configuration for a [`PairingSession`], analogous to Bumble's
/// `PairingConfig` + `SecurityManager` delegate defaults.
#[derive(Debug, Clone, Copy)]
pub struct PairingConfig {
    /// Local IO capability advertised in the Pairing Request/Response.
    pub io_capability: u8,
    /// Whether to request bonding (persist keys after pairing).
    pub bonding: bool,
    /// Whether to request MITM (man-in-the-middle) protection.
    pub mitm: bool,
    /// Whether to request LE Secure Connections (vs. LE Legacy).
    pub sc: bool,
    /// Whether to use the fixed Debug Mode key pair for reproducible traces.
    pub debug_mode: bool,
    /// Whether to hold phase-3 key distribution until the link is actually
    /// encrypted ([`PairingSession::on_link_encrypted`]). The spec requires
    /// it, and real stacks (Bumble: "received key distribution on a
    /// non-encrypted connection") reject early keys — but only runtimes
    /// with a real controller see an Encryption Change event, so the scene
    /// arms this and bare in-process sessions keep distributing eagerly.
    pub defer_key_distribution: bool,
    /// Key-distribution flags this side offers as initiator.
    pub initiator_key_distribution: u8,
    /// Key-distribution flags this side offers as responder.
    pub responder_key_distribution: u8,
    /// Which identity address to distribute, if a preference is set.
    pub identity_address_preference: Option<IdentityAddressPreference>,
}

impl Default for PairingConfig {
    fn default() -> Self {
        Self {
            io_capability: io_capability::NO_INPUT_NO_OUTPUT,
            bonding: true,
            mitm: false,
            sc: true,
            debug_mode: false,
            defer_key_distribution: false,
            initiator_key_distribution: key_distribution::ENC_KEY | key_distribution::ID_KEY,
            responder_key_distribution: key_distribution::ENC_KEY | key_distribution::ID_KEY,
            identity_address_preference: None,
        }
    }
}

/// Resolves which identity address to distribute (Identity Address
/// Information PDU), matching Bumble's `Session.send_identity_address_command`
/// decision matrix: an explicit preference wins; otherwise a public address
/// is preferred over a random static one when both are available.
pub fn resolve_identity_address(
    preference: Option<IdentityAddressPreference>,
    public_address: Address,
    static_address: Address,
) -> (u8, Address) {
    match preference {
        Some(IdentityAddressPreference::Public) => (0, public_address),
        Some(IdentityAddressPreference::Random) => (1, static_address),
        None => {
            if public_address != Address::ANY {
                (0, public_address)
            } else {
                (1, static_address)
            }
        }
    }
}

static RANDOM_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Minimal, non-cryptographic PRNG for simulated SMP nonces and keys.
/// Simble is a protocol simulator, not a security boundary, so this only
/// needs to avoid handing out identical values across rapid successive
/// calls (e.g. two `VirtualDevice`s pairing within the same test process).
fn random_bytes<const N: usize>() -> [u8; N] {
    let counter = RANDOM_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut out = [0u8; N];
    crate::types::rng::fill_pseudo_random(counter.wrapping_mul(0x2545_F491_4F6C_DD1D), &mut out);
    out
}

fn build_16(op: u8, val: &[u8; 16]) -> Vec<u8> {
    let mut v = Vec::with_capacity(17);
    v.push(op);
    v.extend_from_slice(val);
    v
}

fn parse_16(pdu: &[u8], op: u8) -> Option<[u8; 16]> {
    if pdu.len() != 17 || pdu[0] != op {
        return None;
    }
    pdu[1..17].try_into().ok()
}

fn build_public_key(x: &[u8; 32], y: &[u8; 32]) -> Vec<u8> {
    let mut v = Vec::with_capacity(65);
    v.push(opcode::PAIRING_PUBLIC_KEY);
    v.extend_from_slice(x);
    v.extend_from_slice(y);
    v
}

fn parse_public_key(pdu: &[u8]) -> Option<([u8; 32], [u8; 32])> {
    if pdu.len() != 65 || pdu[0] != opcode::PAIRING_PUBLIC_KEY {
        return None;
    }
    Some((pdu[1..33].try_into().ok()?, pdu[33..65].try_into().ok()?))
}

fn build_master_id(ediv: u16, rand: &[u8; 8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(11);
    v.push(opcode::MASTER_IDENTIFICATION);
    v.extend_from_slice(&ediv.to_le_bytes());
    v.extend_from_slice(rand);
    v
}

fn parse_master_id(pdu: &[u8]) -> Option<(u16, [u8; 8])> {
    if pdu.len() != 11 || pdu[0] != opcode::MASTER_IDENTIFICATION {
        return None;
    }
    let ediv = u16::from_le_bytes(pdu[1..3].try_into().ok()?);
    let rand = pdu[3..11].try_into().ok()?;
    Some((ediv, rand))
}

fn build_identity_addr_info(addr_type: u8, addr: &Address) -> Vec<u8> {
    let mut v = Vec::with_capacity(8);
    v.push(opcode::IDENTITY_ADDR_INFO);
    v.push(addr_type);
    v.extend_from_slice(addr.as_slice());
    v
}

fn parse_identity_addr_info(pdu: &[u8]) -> Option<(u8, Address)> {
    if pdu.len() != 8 || pdu[0] != opcode::IDENTITY_ADDR_INFO {
        return None;
    }
    // Section 3.6.5 defines exactly two AddrType values: 0x00 for a public
    // device address and 0x01 for a static random one. Anything else is a
    // parameter outside its specified range, and this is the field a bond
    // record gets keyed by — accepting a nonsense type would file the peer's
    // identity under an address type that means nothing.
    if pdu[1] > 1 {
        return None;
    }
    let bytes: [u8; 6] = pdu[2..8].try_into().ok()?;
    Some((pdu[1], Address::new(bytes)))
}

/// Parses and range-checks a Pairing Request or Pairing Response (Sections
/// 3.5.1 and 3.5.2).
///
/// [`SmpPairingPacket::parse`] is a zero-copy prefix parse: it is happy with a
/// PDU *longer* than the seven octets the figure defines, and it range-checks
/// nothing. Both matter here. `Preq`/`Pres` are fed verbatim into the confirm
/// value, so the seven octets are the ones that have to be there and no more;
/// and every field in them has a defined range that Tables 3.4, 3.5 and 3.6
/// close off. A `None` here becomes Pairing Failed 0x0A, whose definition
/// (Table 3.7) is precisely "the command length is invalid or... a parameter
/// is outside of the specified range".
fn parse_pairing_parameters(pdu: &[u8]) -> Option<SmpPairingPacket> {
    if pdu.len() != LEN_PAIRING_PARAMETERS {
        return None;
    }
    let (packet, _) = SmpPairingPacket::parse(pdu)?;
    let packet = *packet;
    // Table 3.4: 0x00-0x04 are the IO capabilities; 0x05 to 0xFF are reserved.
    if packet.io_capability > io_capability::KEYBOARD_DISPLAY {
        return None;
    }
    // Table 3.5: 0x00 and 0x01 only.
    if packet.oob_data_flag > OOB_DATA_PRESENT {
        return None;
    }
    // Figure 3.3 / Table 3.6, see [`AUTH_REQ_RESERVED`].
    if packet.auth_req & AUTH_REQ_RESERVED != 0 {
        return None;
    }
    // Section 2.3.4: "between 7 octets (56 bits) and 16 octets (128 bits)".
    // Worth its own check rather than folding into the negotiated minimum:
    // `encryption_key_size()` is `preq[4].min(pres[4])`, so a peer claiming a
    // 1-octet maximum would negotiate a 1-octet key with no field left to
    // catch it, and a peer claiming 255 would over-report the bond's strength.
    if !(MIN_ENCRYPTION_KEY_SIZE..=MAX_ENCRYPTION_KEY_SIZE)
        .contains(&packet.max_encryption_key_size)
    {
        return None;
    }
    Some(packet)
}

/// A single SMP pairing session, one per connection side. Drives Pairing
/// Request/Response, Confirm/Random, Public Key / DHKey Check (Secure
/// Connections only), and bonding key distribution to completion.
#[derive(Debug, Clone)]
pub struct PairingSession {
    role: Role,
    phase: Phase,
    io_capability: u8,
    peer_io_capability: u8,
    bonding: bool,
    mitm: bool,
    sc: bool,
    defer_key_distribution: bool,
    link_encrypted: bool,
    initiator_key_distribution: u8,
    responder_key_distribution: u8,
    preq: [u8; 7],
    pres: [u8; 7],
    ia: [u8; 6],
    iat: u8,
    ra: [u8; 6],
    rat: u8,
    tk: [u8; 16],
    r_local: [u8; 16],
    peer_random: Option<[u8; 16]>,
    peer_confirm: Option<[u8; 16]>,
    stk: Option<[u8; 16]>,
    local_bonded_ltk: [u8; 16],
    local_bonded_ediv: u16,
    local_bonded_rand: [u8; 8],
    peer_ltk: Option<[u8; 16]>,
    peer_ediv: Option<u16>,
    peer_rand: Option<[u8; 8]>,
    ecdh: P256Keypair,
    local_pub_x: [u8; 32],
    local_pub_y: [u8; 32],
    peer_pub_x: [u8; 32],
    peer_pub_y: [u8; 32],
    dh_key: Option<[u8; 32]>,
    ltk: Option<[u8; 16]>,
    ea: Option<[u8; 16]>,
    eb: Option<[u8; 16]>,
    local_irk: [u8; 16],
    peer_irk: Option<[u8; 16]>,
    peer_identity_address: Option<(u8, Address)>,
    peer_csrk: Option<[u8; 16]>,
    peer_expected: Vec<u8>,
    own_keys_sent: bool,
    complete: bool,
    failed: bool,
    timed_out: bool,
    /// Seconds left on the Security Manager Timer, or `None` while the timer
    /// is stopped — before pairing starts, and once it completes "whether
    /// successfully or not" (Section 3.4).
    timer_remaining: Option<f64>,
    /// The `t_seconds` of the most recent [`PairingSession::tick`], so the
    /// next one can subtract a delta. `None` until the first tick, which
    /// therefore only establishes the baseline and consumes no time: a
    /// session created at scene second 900 must get its full 30 seconds, not
    /// be timed out by its own first tick.
    last_tick: Option<f64>,
    is_encrypted: bool,
    identity_address_preference: Option<IdentityAddressPreference>,
    local_public_address: Address,
    local_static_address: Address,
    pending: VecDeque<Vec<u8>>,
}

impl PairingSession {
    /// Creates a pairing session for `role` with the given config and the
    /// local/peer addresses used in confirm-value computation.
    pub fn new(
        role: Role,
        config: PairingConfig,
        local_address: Address,
        local_address_type: AddressType,
        peer_address: Address,
        peer_address_type: AddressType,
    ) -> Self {
        fn is_random(t: AddressType) -> u8 {
            matches!(t, AddressType::Random | AddressType::RandomIdentity) as u8
        }

        let (ia, iat, ra, rat) = if role == Role::Initiator {
            (
                local_address.bytes,
                is_random(local_address_type),
                peer_address.bytes,
                is_random(peer_address_type),
            )
        } else {
            (
                peer_address.bytes,
                is_random(peer_address_type),
                local_address.bytes,
                is_random(local_address_type),
            )
        };

        let (local_public_address, local_static_address) = match local_address_type {
            AddressType::Public | AddressType::PublicIdentity => (local_address, Address::ANY),
            AddressType::Random | AddressType::RandomIdentity => (Address::ANY, local_address),
        };

        let ecdh = if config.debug_mode {
            P256Keypair::from_private_be(&SMP_DEBUG_KEY_PRIVATE_BE)
                .expect("the spec debug scalar is valid")
        } else {
            P256Keypair::generate()
        };
        let (local_pub_x, local_pub_y) = (ecdh.x_le, ecdh.y_le);

        Self {
            role,
            phase: Phase::Idle,
            io_capability: config.io_capability,
            peer_io_capability: io_capability::NO_INPUT_NO_OUTPUT,
            bonding: config.bonding,
            mitm: config.mitm,
            sc: config.sc,
            defer_key_distribution: config.defer_key_distribution,
            link_encrypted: false,
            initiator_key_distribution: config.initiator_key_distribution,
            responder_key_distribution: config.responder_key_distribution,
            preq: [0; 7],
            pres: [0; 7],
            ia,
            iat,
            ra,
            rat,
            tk: [0; 16],
            r_local: [0; 16],
            peer_random: None,
            peer_confirm: None,
            stk: None,
            local_bonded_ltk: [0; 16],
            local_bonded_ediv: 0,
            local_bonded_rand: [0; 8],
            peer_ltk: None,
            peer_ediv: None,
            peer_rand: None,
            ecdh,
            local_pub_x,
            local_pub_y,
            peer_pub_x: [0; 32],
            peer_pub_y: [0; 32],
            dh_key: None,
            ltk: None,
            ea: None,
            eb: None,
            local_irk: random_bytes::<16>(),
            peer_irk: None,
            peer_identity_address: None,
            peer_csrk: None,
            peer_expected: Vec::new(),
            own_keys_sent: false,
            complete: false,
            failed: false,
            timed_out: false,
            timer_remaining: None,
            last_tick: None,
            is_encrypted: false,
            identity_address_preference: config.identity_address_preference,
            local_public_address,
            local_static_address,
            pending: VecDeque::new(),
        }
    }

    fn auth_req_byte(&self) -> u8 {
        let mut v = 0u8;
        if self.bonding {
            v |= auth_req::BONDING;
        }
        if self.mitm {
            v |= auth_req::MITM;
        }
        if self.sc {
            v |= auth_req::SC;
        }
        v
    }

    /// Queues one PDU for transmission, restarting the Security Manager Timer.
    ///
    /// Every outgoing PDU goes through here rather than touching `pending`
    /// directly, because Section 3.4 words the timer rule around exactly this
    /// moment: "The Security Manager Timer shall be reset when an L2CAP SMP
    /// command is queued for transmission." Sprinkling resets next to a dozen
    /// `push_back` calls is how one gets missed.
    fn queue(&mut self, pdu: Vec<u8>) {
        self.pending.push_back(pdu);
        self.timer_remaining = Some(SMP_TIMEOUT_SECONDS);
    }

    /// Stops the Security Manager Timer — Section 3.4: "When a Pairing process
    /// completes (whether successfully or not), the Security Manager Timer
    /// shall be stopped."
    fn stop_timer(&mut self) {
        self.timer_remaining = None;
    }

    /// Advances the Security Manager Timer to simulated time `t_seconds`,
    /// returning `true` on the tick that expires it (Bluetooth Core Spec Vol 3,
    /// Part H, Section 3.4).
    ///
    /// This is the only clock a `PairingSession` has. It takes the same
    /// monotonic `t_seconds` the rest of the simulator ticks on — the scene's
    /// `tick(t_seconds)`, a script's `fn tick(server, t)` — rather than reading
    /// a wall clock, so a test can jump 31 seconds in one call and a session
    /// that is never ticked simply never times out. Only the *delta* between
    /// consecutive ticks is consumed, so the session need not have been alive
    /// since `t_seconds == 0.0`, and a clock that stalls or steps backwards
    /// costs it nothing.
    ///
    /// On expiry the pairing has failed: the key material is discarded, and
    /// anything already queued for transmission is dropped unsent, because
    /// "No further SMP commands shall be sent over the L2CAP Security Manager
    /// Channel" — the caller should treat the link as unkeyed.
    pub fn tick(&mut self, t_seconds: f64) -> bool {
        let elapsed = match self.last_tick {
            Some(previous) if t_seconds > previous => t_seconds - previous,
            _ => 0.0,
        };
        self.last_tick = Some(t_seconds);
        let Some(remaining) = self.timer_remaining else {
            return false;
        };
        let remaining = remaining - elapsed;
        if remaining > 0.0 {
            self.timer_remaining = Some(remaining);
            return false;
        }
        self.stop_timer();
        self.timed_out = true;
        self.phase = Phase::Failed;
        self.failed = true;
        self.discard_key_material();
        self.pending.clear();
        true
    }

    /// Whether this session ended because the Security Manager Timer reached
    /// `SMP_TIMEOUT_SECONDS`, as opposed to an ordinary Pairing Failed.
    ///
    /// The two are not interchangeable. After a Pairing Failed, "any
    /// subsequent pairing procedure shall restart from the Pairing Feature
    /// Exchange phase" (Section 3.5.5) — re-pairing on the same link is
    /// allowed. After a timeout it is not: "A new Pairing process shall only
    /// be performed when a new physical link has been established"
    /// (Section 3.4).
    pub fn is_timed_out(&self) -> bool {
        self.timed_out
    }

    /// Seconds left before the Security Manager Timer expires, or `None` while
    /// it is stopped. Exposed so a caller (or a test) can see that queuing a
    /// PDU restarted it, which is the half of Section 3.4 that has no
    /// observable effect until 30 seconds later.
    pub fn seconds_until_timeout(&self) -> Option<f64> {
        self.timer_remaining
    }

    /// Builds and returns the initial Pairing Request PDU. Initiator-only.
    pub fn start(&mut self) -> Vec<u8> {
        let req = SmpPairingPacket {
            opcode: opcode::PAIRING_REQUEST,
            io_capability: self.io_capability,
            oob_data_flag: 0,
            auth_req: self.auth_req_byte(),
            max_encryption_key_size: MAX_ENCRYPTION_KEY_SIZE,
            initiator_key_distribution: self.initiator_key_distribution,
            responder_key_distribution: self.responder_key_distribution,
        };
        self.preq = req.as_bytes().try_into().unwrap();
        self.phase = Phase::WaitPairingResponse;
        // Section 3.4: "Upon transmission of the Pairing Request command...
        // the Security Manager Timer shall be reset and started." The request
        // is returned rather than queued, so start the timer by hand.
        self.timer_remaining = Some(SMP_TIMEOUT_SECONDS);
        req.as_bytes().to_vec()
    }

    /// Processes one incoming SMP PDU, returning the immediate outgoing reply
    /// (if any). Additional PDUs queued as a side effect (e.g. multi-PDU key
    /// distribution) are retrieved via [`Self::poll_pending`].
    pub fn handle_pdu(&mut self, pdu: &[u8]) -> Result<Option<Vec<u8>>, SimbleError> {
        self.step(pdu)?;
        Ok(self.pending.pop_front())
    }

    /// Drains one more queued outgoing PDU, if any remain after
    /// [`Self::handle_pdu`] returned its immediate reply.
    pub fn poll_pending(&mut self) -> Option<Vec<u8>> {
        self.pending.pop_front()
    }

    /// Returns `true` once pairing (including key distribution) has finished.
    pub fn is_complete(&self) -> bool {
        self.complete
    }

    /// Returns `true` if the session ended in a Pairing Failed state.
    pub fn is_failed(&self) -> bool {
        self.failed
    }

    /// Returns `true` once the link key has been established and encryption enabled.
    pub fn is_encrypted(&self) -> bool {
        self.is_encrypted
    }

    /// The key that secures the link: the SC LTK for Secure Connections, or
    /// the legacy STK otherwise (the bonded legacy LTK is a separate,
    /// per-role value distributed via [`Self::pairing_keys`]).
    pub fn ltk(&self) -> Option<[u8; 16]> {
        if self.sc { self.ltk } else { self.stk }
    }

    /// The peer's distributed Identity Resolving Key, if any.
    pub fn peer_irk(&self) -> Option<[u8; 16]> {
        self.peer_irk
    }

    /// Whether both sides requested bonding (Core Spec Vol 3, Part H,
    /// Section 3.5.1 AuthReq) — only then is the pairing worth persisting
    /// in a bond store.
    pub fn is_bonding(&self) -> bool {
        self.bonding
    }

    /// Whether LE Secure Connections was negotiated (vs. LE Legacy).
    pub fn is_secure_connections(&self) -> bool {
        self.sc
    }

    /// Whether the pairing used a MITM-protected association model: both
    /// sides must have set the MITM AuthReq bit (Core Spec Vol 3, Part H,
    /// Section 2.3.1) — `preq`/`pres` byte 3 is AuthReq.
    pub fn is_authenticated(&self) -> bool {
        (self.preq[3] & auth_req::MITM != 0) && (self.pres[3] & auth_req::MITM != 0)
    }

    /// The negotiated encryption key size: the minimum of both sides'
    /// Maximum Encryption Key Size fields (Core Spec Vol 3, Part H,
    /// Section 2.3.4) — `preq`/`pres` byte 4. Meaningful once the
    /// Request/Response exchange has happened.
    pub fn encryption_key_size(&self) -> u8 {
        self.preq[4].min(self.pres[4])
    }

    /// The peer's distributed identity address `(address_type, address)`, if any.
    pub fn peer_identity_address(&self) -> Option<(u8, Address)> {
        self.peer_identity_address
    }

    /// This session's local public key `(x, y)` for Secure Connections.
    pub fn local_public_key(&self) -> ([u8; 32], [u8; 32]) {
        (self.local_pub_x, self.local_pub_y)
    }

    /// Builds a [`PairingKeys`] record suitable for [`crate::smp::KeyStore`],
    /// validating this port's claimed Bumble `JsonKeyStore` compatibility.
    pub fn pairing_keys(&self) -> PairingKeys {
        let mut keys = PairingKeys::default();
        if self.sc {
            if let Some(ltk) = self.ltk {
                keys.ltk = Some(PairingKey::new(ltk));
            }
        } else {
            let mut local_key = PairingKey::new(self.local_bonded_ltk);
            local_key.ediv = Some(self.local_bonded_ediv);
            local_key.rand = Some(self.local_bonded_rand);
            let peer_key = self.peer_ltk.map(|v| {
                let mut k = PairingKey::new(v);
                k.ediv = self.peer_ediv;
                k.rand = self.peer_rand;
                k
            });
            if self.role == Role::Initiator {
                keys.ltk_peripheral = Some(local_key);
                keys.ltk_central = peer_key;
            } else {
                keys.ltk_central = Some(local_key);
                keys.ltk_peripheral = peer_key;
            }
        }
        if let Some(irk) = self.peer_irk {
            keys.irk = Some(PairingKey::new(irk));
        }
        if let Some((addr_type, _)) = self.peer_identity_address {
            keys.address_type = Some(addr_type);
        }
        keys
    }

    /// Derives an LE Long Term Key from a BR/EDR Link Key (cross-transport
    /// key derivation, Bluetooth Core Spec Vol 3, Part H, Section 2.4.2.4).
    pub fn derive_ltk(link_key: &[u8; 16], ct2: bool) -> [u8; 16] {
        let ilk = if ct2 {
            h7(&SMP_CTKD_H7_BRLE_SALT, link_key)
        } else {
            h6(link_key, b"tmp2")
        };
        h6(&ilk, b"brle")
    }

    /// Derives a BR/EDR Link Key from an LE Long Term Key (the inverse of
    /// [`Self::derive_ltk`]).
    pub fn derive_link_key(ltk: &[u8; 16], ct2: bool) -> [u8; 16] {
        let ilk = if ct2 {
            h7(&SMP_CTKD_H7_LEBR_SALT, ltk)
        } else {
            h6(ltk, b"tmp1")
        };
        h6(&ilk, b"lebr")
    }

    fn fail(&mut self, reason: u8) -> Result<(), SimbleError> {
        self.phase = Phase::Failed;
        self.failed = true;
        self.discard_key_material();
        self.queue(vec![opcode::PAIRING_FAILED, reason]);
        // `queue` restarted the timer, as Section 3.4 says queuing a command
        // must. But this particular command ends the procedure, and the same
        // section stops the timer "when a Pairing process completes (whether
        // successfully or not)" — so it stops here, after the PDU is queued.
        self.stop_timer();
        Ok(())
    }

    /// Drops every secret derived so far.
    ///
    /// Secure Connections computes the LTK with `f5` during the random
    /// exchange, *before* the DH Key Check proves the peer derived the same
    /// one — it has to, because Ea/Eb are computed from it. So a session that
    /// fails after that point was holding an LTK that nothing ever
    /// authenticated, and was keeping it.
    ///
    /// Nothing used it: `is_encrypted` stays false and the link never starts
    /// encryption, which is the property that actually matters. But retaining
    /// unauthenticated key material past the failure that rejected it is the
    /// kind of thing that becomes exploitable the moment some later code path
    /// reads `ltk` without re-checking `failed`. Discard it at the point of
    /// failure instead of trusting every future reader to be careful.
    fn discard_key_material(&mut self) {
        self.tk = [0; 16];
        self.stk = None;
        self.dh_key = None;
        self.ltk = None;
        self.ea = None;
        self.eb = None;
        self.peer_confirm = None;
    }

    fn step(&mut self, pdu: &[u8]) -> Result<(), SimbleError> {
        // Once the procedure has failed, nothing more on this channel is
        // processed and nothing more is sent. Two sentences say so, and the
        // second is the one that decides the *shape* of this guard: Section
        // 3.5.5 says a Pairing Failed "reports that the pairing procedure has
        // been stopped and no further communication for the current pairing
        // procedure is to occur", and Section 3.4 says that after the timer
        // expires "No further SMP commands shall be sent over the L2CAP
        // Security Manager Channel". A later PDU is therefore *dropped* — not
        // answered with a second Pairing Failed, which would itself be a
        // further SMP command, and which two failed peers would trade forever.
        if self.failed {
            return Ok(());
        }
        if pdu.is_empty() {
            return self.fail(error_code::INVALID_PARAMETERS);
        }
        match pdu[0] {
            opcode::PAIRING_REQUEST => self.on_pairing_request(pdu),
            opcode::PAIRING_RESPONSE => self.on_pairing_response(pdu),
            opcode::PAIRING_CONFIRM => self.on_pairing_confirm(pdu),
            opcode::PAIRING_RANDOM => self.on_pairing_random(pdu),
            opcode::PAIRING_FAILED => self.on_pairing_failed(pdu),
            opcode::PAIRING_PUBLIC_KEY => self.on_public_key(pdu),
            opcode::PAIRING_DHKEY_CHECK => self.on_dhkey_check(pdu),
            opcode::ENCRYPTION_INFO => self.on_encryption_info(pdu),
            opcode::MASTER_IDENTIFICATION => self.on_master_identification(pdu),
            opcode::IDENTITY_INFO => self.on_identity_info(pdu),
            opcode::IDENTITY_ADDR_INFO => self.on_identity_addr_info(pdu),
            opcode::SIGNING_INFO => self.on_signing_info(pdu),
            // Defined commands this port does not implement. Section 3.3: "If
            // pairing is supported then all commands shall be supported" — we
            // do not support these two, so the honest answer is reason 0x07,
            // "The SMP command received is not supported on this device".
            opcode::SECURITY_REQUEST | OPCODE_KEYPRESS_NOTIFICATION => {
                self.fail(error_code::COMMAND_NOT_SUPPORTED)
            }
            // Everything above 0x0E is reserved for future use, and Section
            // 3.3 is explicit about those: "If a packet is received with a
            // Code that is reserved for future use it shall be ignored."
            // Failing the pairing instead would let any future spec revision
            // break this stack by defining one more command.
            _ => Ok(()),
        }
    }

    fn on_pairing_failed(&mut self, pdu: &[u8]) -> Result<(), SimbleError> {
        // Figure 3.7: Code octet then Reason octet, and nothing else.
        if pdu.len() != LEN_PAIRING_FAILED {
            return self.fail(error_code::INVALID_PARAMETERS);
        }
        // The peer rejected us. Same reasoning as `fail()`: whatever we had
        // derived was never authenticated, so it goes too.
        self.phase = Phase::Failed;
        self.failed = true;
        self.discard_key_material();
        self.stop_timer();
        Ok(())
    }

    fn on_pairing_request(&mut self, pdu: &[u8]) -> Result<(), SimbleError> {
        if self.role != Role::Responder {
            return self.fail(error_code::COMMAND_NOT_SUPPORTED);
        }
        let Some(req) = parse_pairing_parameters(pdu) else {
            return self.fail(error_code::INVALID_PARAMETERS);
        };
        // Section 3.4: "Upon... reception of the Pairing Request command, the
        // Security Manager Timer shall be reset and started." The responder's
        // clock starts here, not when it gets around to answering.
        self.timer_remaining = Some(SMP_TIMEOUT_SECONDS);
        self.preq = req.as_bytes().try_into().unwrap();
        self.peer_io_capability = req.io_capability;
        self.sc = self.sc && (req.auth_req & auth_req::SC != 0);
        self.bonding = self.bonding && (req.auth_req & auth_req::BONDING != 0);
        self.initiator_key_distribution &= req.initiator_key_distribution;
        self.responder_key_distribution &= req.responder_key_distribution;
        self.compute_peer_expected(self.initiator_key_distribution);

        let resp = SmpPairingPacket::new_response(
            self.io_capability,
            self.auth_req_byte(),
            self.initiator_key_distribution,
            self.responder_key_distribution,
        );
        self.pres = resp.as_bytes().try_into().unwrap();
        self.queue(resp.as_bytes().to_vec());
        self.phase = if self.sc {
            Phase::WaitPeerPublicKey
        } else {
            Phase::WaitPeerConfirm
        };
        Ok(())
    }

    fn on_pairing_response(&mut self, pdu: &[u8]) -> Result<(), SimbleError> {
        if self.role != Role::Initiator {
            return self.fail(error_code::COMMAND_NOT_SUPPORTED);
        }
        let Some(resp) = parse_pairing_parameters(pdu) else {
            return self.fail(error_code::INVALID_PARAMETERS);
        };
        self.pres = resp.as_bytes().try_into().unwrap();
        self.peer_io_capability = resp.io_capability;
        self.sc = self.sc && (resp.auth_req & auth_req::SC != 0);
        self.bonding = self.bonding && (resp.auth_req & auth_req::BONDING != 0);

        if (resp.initiator_key_distribution & !self.initiator_key_distribution) != 0
            || (resp.responder_key_distribution & !self.responder_key_distribution) != 0
        {
            return self.fail(error_code::INVALID_PARAMETERS);
        }
        self.initiator_key_distribution = resp.initiator_key_distribution;
        self.responder_key_distribution = resp.responder_key_distribution;
        self.compute_peer_expected(self.responder_key_distribution);

        if self.sc {
            self.queue(build_public_key(&self.local_pub_x, &self.local_pub_y));
            self.phase = Phase::WaitPeerPublicKey;
        } else {
            self.send_confirm_legacy();
            self.phase = Phase::WaitPeerConfirm;
        }
        Ok(())
    }

    fn send_confirm_legacy(&mut self) {
        self.r_local = random_bytes::<16>();
        let confirm = c1(
            &self.tk,
            &self.r_local,
            &self.preq,
            &self.pres,
            self.iat,
            self.rat,
            &self.ia,
            &self.ra,
        );
        self.queue(build_16(opcode::PAIRING_CONFIRM, &confirm));
    }

    fn on_pairing_confirm(&mut self, pdu: &[u8]) -> Result<(), SimbleError> {
        let Some(value) = parse_16(pdu, opcode::PAIRING_CONFIRM) else {
            return self.fail(error_code::INVALID_PARAMETERS);
        };
        self.peer_confirm = Some(value);
        if self.sc {
            if self.role != Role::Initiator {
                return self.fail(error_code::COMMAND_NOT_SUPPORTED);
            }
            self.r_local = random_bytes::<16>();
            self.queue(build_16(opcode::PAIRING_RANDOM, &self.r_local));
            self.phase = Phase::WaitPeerRandom;
        } else if self.role == Role::Initiator {
            self.queue(build_16(opcode::PAIRING_RANDOM, &self.r_local));
            self.phase = Phase::WaitPeerRandom;
        } else {
            self.send_confirm_legacy();
            self.phase = Phase::WaitPeerRandom;
        }
        Ok(())
    }

    fn on_pairing_random(&mut self, pdu: &[u8]) -> Result<(), SimbleError> {
        let Some(value) = parse_16(pdu, opcode::PAIRING_RANDOM) else {
            return self.fail(error_code::INVALID_PARAMETERS);
        };
        self.peer_random = Some(value);
        if self.sc {
            self.on_random_sc(value)
        } else {
            self.on_random_legacy(value)
        }
    }

    fn on_random_legacy(&mut self, peer_random: [u8; 16]) -> Result<(), SimbleError> {
        let Some(expected) = self.peer_confirm else {
            return self.fail(error_code::UNSPECIFIED_REASON);
        };
        let check = c1(
            &self.tk,
            &peer_random,
            &self.preq,
            &self.pres,
            self.iat,
            self.rat,
            &self.ia,
            &self.ra,
        );
        if check != expected {
            return self.fail(error_code::CONFIRM_VALUE_FAILED);
        }

        let (mrand, srand) = if self.role == Role::Initiator {
            (self.r_local, peer_random)
        } else {
            (peer_random, self.r_local)
        };
        self.stk = Some(s1(&self.tk, &srand, &mrand));
        self.local_bonded_ltk = random_bytes::<16>();
        self.local_bonded_ediv = u16::from_le_bytes(random_bytes::<2>());
        self.local_bonded_rand = random_bytes::<8>();

        if self.role == Role::Responder {
            self.queue(build_16(opcode::PAIRING_RANDOM, &self.r_local));
        }
        self.on_encrypted();
        Ok(())
    }

    fn on_random_sc(&mut self, peer_random: [u8; 16]) -> Result<(), SimbleError> {
        // `finish_sc_random_exchange` needs the DHKey, and the DHKey only
        // exists once the public keys have been exchanged (Section 2.3.5.6.1:
        // "After the public keys have been exchanged, the device can then
        // start computing the Diffie-Hellman Key"). A peer that skips straight
        // to Pairing Random has sent a well-formed PDU in the wrong order —
        // which used to reach an `expect` on the responder path and panic the
        // whole process from one remote packet.
        if self.dh_key.is_none() {
            return self.fail(error_code::UNSPECIFIED_REASON);
        }
        match self.role {
            Role::Responder => {
                // `r_local` was already committed to (as Nb) when the
                // confirm value was computed in `on_public_key` — it must
                // not be regenerated here, or the random value sent
                // wouldn't match what the confirm committed to.
                self.queue(build_16(opcode::PAIRING_RANDOM, &self.r_local));
                self.finish_sc_random_exchange();
                self.phase = Phase::WaitPeerDhKeyCheck;
            }
            Role::Initiator => {
                let Some(expected) = self.peer_confirm else {
                    return self.fail(error_code::UNSPECIFIED_REASON);
                };
                let check = f4(&self.peer_pub_x, &self.local_pub_x, &peer_random, 0);
                if check != expected {
                    return self.fail(error_code::CONFIRM_VALUE_FAILED);
                }
                self.finish_sc_random_exchange();
                let ea = self.ea.expect("computed by finish_sc_random_exchange");
                self.queue(build_16(opcode::PAIRING_DHKEY_CHECK, &ea));
                self.phase = Phase::WaitPeerDhKeyCheck;
            }
        }
        Ok(())
    }

    fn finish_sc_random_exchange(&mut self) {
        let peer_random = self.peer_random.expect("set by on_pairing_random");
        let (na, nb) = if self.role == Role::Initiator {
            (self.r_local, peer_random)
        } else {
            (peer_random, self.r_local)
        };

        let mut a = [0u8; 7];
        a[..6].copy_from_slice(&self.ia);
        a[6] = self.iat;
        let mut b = [0u8; 7];
        b[..6].copy_from_slice(&self.ra);
        b[6] = self.rat;

        let dh_key = self.dh_key.expect("set by on_public_key");
        let (mac_key, ltk) = f5(&dh_key, &na, &nb, &a, &b);
        self.ltk = Some(ltk);

        let io_cap_a: [u8; 3] = self.preq[1..4].try_into().unwrap();
        let io_cap_b: [u8; 3] = self.pres[1..4].try_into().unwrap();
        let zero = [0u8; 16];
        self.ea = Some(f6(&mac_key, &na, &nb, &zero, &io_cap_a, &a, &b));
        self.eb = Some(f6(&mac_key, &nb, &na, &zero, &io_cap_b, &b, &a));
    }

    fn on_public_key(&mut self, pdu: &[u8]) -> Result<(), SimbleError> {
        let Some((x, y)) = parse_public_key(pdu) else {
            return self.fail(error_code::INVALID_PARAMETERS);
        };
        self.peer_pub_x = x;
        self.peer_pub_y = y;
        // Real ECDH — and real point validation: a peer key that is not on
        // the P-256 curve fails the pairing (mandatory since CVE-2018-5383;
        // Android enforces the same on our key). The reason code is the one
        // Section 3.5.5 names for it — "During LE Secure Connections pairing,
        // this command should be sent if the remote device's public key is
        // invalid... The Reason field should be set to 'DHKey Check Failed'" —
        // not Invalid Parameters, which is for a malformed PDU. The PDU here
        // is well-formed; it is the point that is not on the curve.
        let Some(dh_key) = self.ecdh.shared_x_le(&x, &y) else {
            return self.fail(error_code::DHKEY_CHECK_FAILED);
        };
        self.dh_key = Some(dh_key);
        match self.role {
            Role::Initiator => {
                self.phase = Phase::WaitPeerConfirm;
            }
            Role::Responder => {
                self.queue(build_public_key(&self.local_pub_x, &self.local_pub_y));
                self.r_local = random_bytes::<16>();
                let confirm = f4(&self.local_pub_x, &x, &self.r_local, 0);
                self.queue(build_16(opcode::PAIRING_CONFIRM, &confirm));
                self.phase = Phase::WaitPeerRandom;
            }
        }
        Ok(())
    }

    fn on_dhkey_check(&mut self, pdu: &[u8]) -> Result<(), SimbleError> {
        let Some(value) = parse_16(pdu, opcode::PAIRING_DHKEY_CHECK) else {
            return self.fail(error_code::INVALID_PARAMETERS);
        };
        let expected = if self.role == Role::Initiator {
            self.eb
        } else {
            self.ea
        };
        let Some(expected) = expected else {
            return self.fail(error_code::UNSPECIFIED_REASON);
        };
        if value != expected {
            return self.fail(error_code::DHKEY_CHECK_FAILED);
        }
        if self.role == Role::Responder {
            let eb = self.eb.expect("computed by finish_sc_random_exchange");
            self.queue(build_16(opcode::PAIRING_DHKEY_CHECK, &eb));
        }
        self.on_encrypted();
        Ok(())
    }

    fn on_encrypted(&mut self) {
        self.is_encrypted = true;
        self.phase = Phase::Complete;
        // Phase-3 keys belong on the encrypted link. With a real controller
        // the Encryption Change event triggers them (on_link_encrypted);
        // bare in-process sessions have no such event and distribute now.
        if self.role == Role::Responder && (!self.defer_key_distribution || self.link_encrypted) {
            self.distribute_keys();
        }
        if self.peer_expected.is_empty() {
            self.on_peer_distribution_complete();
        }
    }

    /// Tells the session the link is now actually encrypted (HCI Encryption
    /// Change). If key distribution was deferred (see
    /// [`PairingConfig::defer_key_distribution`]), it starts here.
    pub fn on_link_encrypted(&mut self) {
        self.link_encrypted = true;
        if self.phase == Phase::Complete && self.role == Role::Responder {
            self.distribute_keys();
        }
    }

    fn on_peer_distribution_complete(&mut self) {
        if self.role == Role::Initiator {
            self.distribute_keys();
        }
        self.complete = true;
        // Section 3.4: "When a Pairing process completes (whether successfully
        // or not), the Security Manager Timer shall be stopped." Key
        // distribution is part of the pairing process, so this — not
        // `on_encrypted` — is where it finishes. Stopping it here is what
        // keeps a long-lived bonded connection from timing out its own
        // completed session 30 seconds later and discarding the live LTK.
        self.stop_timer();
    }

    fn distribute_keys(&mut self) {
        if self.own_keys_sent {
            return;
        }
        self.own_keys_sent = true;
        let my_flags = if self.role == Role::Initiator {
            self.initiator_key_distribution
        } else {
            self.responder_key_distribution
        };
        if !self.sc && (my_flags & key_distribution::ENC_KEY != 0) {
            self.queue(build_16(opcode::ENCRYPTION_INFO, &self.local_bonded_ltk));
            self.queue(build_master_id(
                self.local_bonded_ediv,
                &self.local_bonded_rand,
            ));
        }
        if my_flags & key_distribution::ID_KEY != 0 {
            self.queue(build_16(opcode::IDENTITY_INFO, &self.local_irk));
            let (addr_type, addr) = resolve_identity_address(
                self.identity_address_preference,
                self.local_public_address,
                self.local_static_address,
            );
            self.queue(build_identity_addr_info(addr_type, &addr));
        }
        if my_flags & key_distribution::SIGN_KEY != 0 {
            self.queue(build_16(opcode::SIGNING_INFO, &[0u8; 16]));
        }
    }

    fn compute_peer_expected(&mut self, flags: u8) {
        self.peer_expected.clear();
        if !self.sc && (flags & key_distribution::ENC_KEY != 0) {
            self.peer_expected.push(opcode::ENCRYPTION_INFO);
            self.peer_expected.push(opcode::MASTER_IDENTIFICATION);
        }
        if flags & key_distribution::ID_KEY != 0 {
            self.peer_expected.push(opcode::IDENTITY_INFO);
            self.peer_expected.push(opcode::IDENTITY_ADDR_INFO);
        }
        if flags & key_distribution::SIGN_KEY != 0 {
            self.peer_expected.push(opcode::SIGNING_INFO);
        }
    }

    fn check_distribution(&mut self, op: u8) -> Result<(), SimbleError> {
        if !self.is_encrypted {
            return self.fail(error_code::UNSPECIFIED_REASON);
        }
        let Some(pos) = self.peer_expected.iter().position(|&o| o == op) else {
            return self.fail(error_code::UNSPECIFIED_REASON);
        };
        self.peer_expected.remove(pos);
        if self.peer_expected.is_empty() {
            self.on_peer_distribution_complete();
        }
        Ok(())
    }

    fn on_encryption_info(&mut self, pdu: &[u8]) -> Result<(), SimbleError> {
        let Some(v) = parse_16(pdu, opcode::ENCRYPTION_INFO) else {
            return self.fail(error_code::INVALID_PARAMETERS);
        };
        self.peer_ltk = Some(v);
        self.check_distribution(opcode::ENCRYPTION_INFO)
    }

    fn on_master_identification(&mut self, pdu: &[u8]) -> Result<(), SimbleError> {
        let Some((ediv, rand)) = parse_master_id(pdu) else {
            return self.fail(error_code::INVALID_PARAMETERS);
        };
        self.peer_ediv = Some(ediv);
        self.peer_rand = Some(rand);
        self.check_distribution(opcode::MASTER_IDENTIFICATION)
    }

    fn on_identity_info(&mut self, pdu: &[u8]) -> Result<(), SimbleError> {
        let Some(v) = parse_16(pdu, opcode::IDENTITY_INFO) else {
            return self.fail(error_code::INVALID_PARAMETERS);
        };
        self.peer_irk = Some(v);
        self.check_distribution(opcode::IDENTITY_INFO)
    }

    fn on_identity_addr_info(&mut self, pdu: &[u8]) -> Result<(), SimbleError> {
        let Some((addr_type, addr)) = parse_identity_addr_info(pdu) else {
            return self.fail(error_code::INVALID_PARAMETERS);
        };
        self.peer_identity_address = Some((addr_type, addr));
        self.check_distribution(opcode::IDENTITY_ADDR_INFO)
    }

    fn on_signing_info(&mut self, pdu: &[u8]) -> Result<(), SimbleError> {
        let Some(v) = parse_16(pdu, opcode::SIGNING_INFO) else {
            return self.fail(error_code::INVALID_PARAMETERS);
        };
        self.peer_csrk = Some(v);
        self.check_distribution(opcode::SIGNING_INFO)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_pairing(sc: bool, debug_mode: bool) -> (PairingSession, PairingSession) {
        let central_addr = Address::from_be_bytes([0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
        let peripheral_addr = Address::from_be_bytes([0x66, 0x55, 0x44, 0x33, 0x22, 0x11]);

        let config = PairingConfig {
            sc,
            debug_mode,
            ..PairingConfig::default()
        };
        let mut initiator = PairingSession::new(
            Role::Initiator,
            config,
            central_addr,
            AddressType::Random,
            peripheral_addr,
            AddressType::Random,
        );
        let mut responder = PairingSession::new(
            Role::Responder,
            config,
            peripheral_addr,
            AddressType::Random,
            central_addr,
            AddressType::Random,
        );

        // Shuttle PDUs between the two sessions until both queues drain,
        // following the standard request -> response -> (public key ->)
        // confirm -> random(x2) -> (dhkey check x2 ->) key distribution
        // sequence.
        let mut to_responder = vec![initiator.start()];
        let mut to_initiator: Vec<Vec<u8>> = Vec::new();
        for _ in 0..64 {
            if to_responder.is_empty() && to_initiator.is_empty() {
                break;
            }
            let mut next_to_initiator = Vec::new();
            for pdu in to_responder.drain(..) {
                if let Some(reply) = responder.handle_pdu(&pdu).unwrap() {
                    next_to_initiator.push(reply);
                }
                while let Some(more) = responder.poll_pending() {
                    next_to_initiator.push(more);
                }
            }
            let mut next_to_responder = Vec::new();
            for pdu in to_initiator.drain(..) {
                if let Some(reply) = initiator.handle_pdu(&pdu).unwrap() {
                    next_to_responder.push(reply);
                }
                while let Some(more) = initiator.poll_pending() {
                    next_to_responder.push(more);
                }
            }
            to_initiator = next_to_initiator;
            to_responder = next_to_responder;
        }

        (initiator, responder)
    }

    #[test]
    fn test_le_legacy_pairing_reaches_matching_stk() {
        let (initiator, responder) = run_pairing(false, false);
        assert!(initiator.is_complete());
        assert!(responder.is_complete());
        assert!(!initiator.is_failed());
        assert!(!responder.is_failed());
        assert!(initiator.is_encrypted());
        assert!(responder.is_encrypted());
        assert_eq!(initiator.ltk(), responder.ltk());
        assert!(initiator.ltk().is_some());
    }

    #[test]
    fn test_le_secure_connections_pairing_reaches_matching_ltk() {
        let (initiator, responder) = run_pairing(true, false);
        assert!(initiator.is_complete());
        assert!(responder.is_complete());
        assert!(!initiator.is_failed());
        assert!(!responder.is_failed());
        assert_eq!(initiator.ltk(), responder.ltk());
        assert!(initiator.ltk().is_some());
        assert!(initiator.peer_irk().is_some());
        assert!(responder.peer_irk().is_some());
    }

    #[test]
    fn test_smp_debug_mode_uses_fixed_key() {
        let debug = PairingSession::new(
            Role::Initiator,
            PairingConfig {
                debug_mode: true,
                ..PairingConfig::default()
            },
            Address::ANY,
            AddressType::Random,
            Address::ANY,
            AddressType::Random,
        );
        // The consts are the spec's big-endian print; the session stores
        // wire order (little-endian) — the reverse.
        assert_eq!(
            debug.local_public_key(),
            (
                crate::crypto::smp_crypto::rev(&SMP_DEBUG_KEY_PUBLIC_X),
                crate::crypto::smp_crypto::rev(&SMP_DEBUG_KEY_PUBLIC_Y)
            )
        );

        let non_debug = PairingSession::new(
            Role::Initiator,
            PairingConfig {
                debug_mode: false,
                ..PairingConfig::default()
            },
            Address::ANY,
            AddressType::Random,
            Address::ANY,
            AddressType::Random,
        );
        assert_ne!(
            non_debug.local_public_key(),
            (SMP_DEBUG_KEY_PUBLIC_X, SMP_DEBUG_KEY_PUBLIC_Y)
        );
    }

    #[test]
    fn test_resolve_identity_address_matrix() {
        let public = Address::from_be_bytes([0x00, 0x11, 0x22, 0x33, 0x44, 0x55]);
        let random = Address::from_be_bytes([0xEE, 0xEE, 0xEE, 0xEE, 0xEE, 0xEE]);

        assert_eq!(resolve_identity_address(None, public, random), (0, public));
        assert_eq!(
            resolve_identity_address(None, Address::ANY, random),
            (1, random)
        );
        assert_eq!(
            resolve_identity_address(Some(IdentityAddressPreference::Public), public, random),
            (0, public)
        );
        assert_eq!(
            resolve_identity_address(Some(IdentityAddressPreference::Random), public, random),
            (1, random)
        );
    }
}
