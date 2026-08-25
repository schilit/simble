// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! A2DP against real consumer kit: what a dual-mode USB dongle can actually
//! reach, checked before anything claims to have streamed to a speaker.
//!
//! `tests/usb_hardware_test.rs` puts two dongles on the air against each
//! other, which proves the LE paths over a real radio but still has simble on
//! both ends. A2DP's peer is a speaker somebody bought, and the only honest
//! way to test the source is to find one. That cannot happen in CI, so these
//! skip loudly rather than pretend.
//!
//! # Running them
//!
//! ```sh
//! cargo run --example usb_list                       # what is plugged in
//! cargo test --test a2dp_hardware_test -- --nocapture
//! cargo run --example a2dp_source                    # the full ladder
//! ```
//!
//! `SIMBLE_USB_A2DP` names the dongle, in any form
//! [`UsbSelector::parse`](simble::transport::usb::UsbSelector::parse) takes
//! (`#0`, `0a12:0001`, `02/4`, `02.3.4`). The default is to try each
//! Bluetooth-class dongle in turn and take the first that both opens *and*
//! supports BR/EDR.
//!
//! # Skipping, and the three different reasons for it
//!
//! Following `usb_hardware_test`'s rule — absence is a skip, malfunction is a
//! failure — but A2DP has one more way to be absent than LE does, and
//! conflating them sends the reader to the wrong problem:
//!
//! 1. **No dongle at all.** CI. A skip.
//! 2. **A dongle that will not open.** On macOS the system Bluetooth stack
//!    claims USB dongles it recognises (a CSR8510 is one), and there is no
//!    userspace way to take one back — no `libusb` detach-kernel-driver
//!    equivalent, and the relevant kexts are behind SIP. Not a simble bug,
//!    so a skip, but the message says which dongle and why.
//! 3. **A dongle that opens but has no BR/EDR.** A Zephyr `hci_usb`
//!    nRF52840 built without Classic enumerates and answers HCI perfectly
//!    and *cannot inquire*, so an A2DP run against one stalls at rung 1 for
//!    a reason that has nothing to do with A2DP. Detected from the LMP
//!    feature bit rather than inferred from a silent timeout.
//!
//! Only the last of those is easy to mistake for a stack bug, which is why
//! it is checked explicitly and named in the skip line.

use simble::transport::usb::{UsbSelector, list_bluetooth_dongles};
use simble::transport::{HciChannel, UsbTransport};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// The dongles are a process-wide resource, and cargo runs the tests in a
/// binary concurrently. Without this the tests race for the same radio and
/// the loser reports "will not open — held by the OS Bluetooth stack", which
/// is indistinguishable from the real macOS problem and sends the reader
/// chasing a kext that is not involved. Found exactly that way.
static RADIO: Mutex<()> = Mutex::new(());

fn radio_lock() -> MutexGuard<'static, ()> {
    RADIO.lock().unwrap_or_else(|e| e.into_inner())
}

const RESET: [u8; 2] = [0x03, 0x0C];
const READ_LOCAL_SUPPORTED_FEATURES: [u8; 2] = [0x03, 0x10];

const EVT_COMMAND_COMPLETE: u8 = 0x0E;

/// LMP feature bit 37, "BR/EDR Not Supported" (Vol 2, Part C, Table 3.3).
/// Bit 37 is byte 4, bit 5. A controller that sets it is LE-only, and A2DP
/// — which is Classic — can never run on it.
const BR_EDR_NOT_SUPPORTED_BYTE: usize = 4;
const BR_EDR_NOT_SUPPORTED_MASK: u8 = 0x20;

/// How long to let a dongle answer two commands before giving up on it.
const BRING_UP_BUDGET: Duration = Duration::from_secs(3);

/// What a dongle turned out to be. The three failure shapes are separate
/// because they send the reader to three different places.
enum Probe {
    /// It opened and speaks BR/EDR: usable for A2DP.
    DualMode(UsbSelector),
    /// It opened, and said it has no BR/EDR.
    LeOnly(UsbSelector),
    /// It would not open, for this reason.
    WillNotOpen(UsbSelector, String),
    /// It opened but never answered `Read_Local_Supported_Features`.
    Mute(UsbSelector),
}

/// Opens `selector` and asks the controller what it supports.
///
/// Deliberately does not go through `ClassicHost`: the question is whether
/// this silicon can do Classic *at all*, and asking it with the stack under
/// test would confuse a controller that cannot answer with a host that asked
/// wrongly.
fn probe(selector: &UsbSelector) -> Probe {
    let mut transport = match UsbTransport::open_selected(selector) {
        Ok(transport) => transport,
        Err(e) => return Probe::WillNotOpen(selector.clone(), e.to_string()),
    };
    let channel = HciChannel::new();
    for opcode in [RESET, READ_LOCAL_SUPPORTED_FEATURES] {
        channel
            .send_command(&[opcode[0], opcode[1], 0x00])
            .expect("queue command");
    }
    let deadline = Instant::now() + BRING_UP_BUDGET;
    while Instant::now() < deadline {
        if transport.pump(&channel).is_err() {
            return Probe::Mute(selector.clone());
        }
        while let Some(packet) = channel.poll_controller_packet() {
            // H4 event: 0x04, code, length, then parameters. A Command
            // Complete carries credits(1), opcode(2), status(1), then the
            // eight feature octets.
            let [
                0x04,
                EVT_COMMAND_COMPLETE,
                _,
                _,
                lo,
                hi,
                status,
                features @ ..,
            ] = &packet[..]
            else {
                continue;
            };
            if [*lo, *hi] != READ_LOCAL_SUPPORTED_FEATURES || *status != 0x00 {
                continue;
            }
            let Some(byte) = features.get(BR_EDR_NOT_SUPPORTED_BYTE) else {
                continue;
            };
            return if byte & BR_EDR_NOT_SUPPORTED_MASK == 0 {
                Probe::DualMode(selector.clone())
            } else {
                Probe::LeOnly(selector.clone())
            };
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Probe::Mute(selector.clone())
}

/// Every dongle worth trying, in the order to try them.
fn candidates() -> Vec<UsbSelector> {
    if let Ok(spec) = std::env::var("SIMBLE_USB_A2DP") {
        return match UsbSelector::parse(&spec) {
            Ok(selector) => vec![selector],
            Err(e) => {
                println!("SIMBLE_USB_A2DP is not a usable selector: {e}");
                Vec::new()
            }
        };
    }
    let Ok(dongles) = list_bluetooth_dongles() else {
        return Vec::new();
    };
    (0..dongles.len()).map(UsbSelector::Index).collect()
}

/// Finds a dongle that can carry A2DP, or prints why none can and returns
/// `None`. Every caller treats `None` as a skip.
fn dual_mode_dongle() -> Option<UsbSelector> {
    let candidates = candidates();
    if candidates.is_empty() {
        println!(
            "SKIP: no Bluetooth-class USB dongle is plugged in. A2DP needs a real \
             radio and a real speaker; CI has neither."
        );
        return None;
    }
    let mut reasons = Vec::new();
    for selector in &candidates {
        match probe(selector) {
            Probe::DualMode(selector) => return Some(selector),
            Probe::LeOnly(selector) => reasons.push(format!(
                "{} opened but reports BR/EDR Not Supported (LMP bit 37) — an LE-only \
                 controller cannot inquire, page, or carry A2DP",
                selector.describe()
            )),
            Probe::WillNotOpen(selector, why) => reasons.push(format!(
                "{} will not open: {why} — held by the OS Bluetooth stack (macOS claims \
                 dongles it recognises, and SIP prevents taking one back), short of usbfs \
                 permissions on Linux, or already in use by another process",
                selector.describe()
            )),
            Probe::Mute(selector) => reasons.push(format!(
                "{} opened but never answered Read_Local_Supported_Features within \
                 {BRING_UP_BUDGET:?} — suspect a wedged controller needing a re-plug",
                selector.describe()
            )),
        }
    }
    println!(
        "SKIP: no dual-mode dongle available for A2DP. Tried {}:",
        candidates.len()
    );
    for reason in &reasons {
        println!("  - {reason}");
    }
    None
}

/// The precondition every A2DP hardware claim rests on: a dongle that opens
/// and speaks Classic. Run on its own so the report says whether the radio
/// or the speaker was the missing half — those are different problems and a
/// single combined test cannot tell them apart.
#[test]
fn a2dp_needs_a_dual_mode_dongle_that_this_machine_will_let_us_open() {
    let _radio = radio_lock();
    let Some(selector) = dual_mode_dongle() else {
        return;
    };
    println!(
        "ok: {} opens and reports BR/EDR support — usable for A2DP",
        selector.describe()
    );
}

/// A2DP's peer cannot be mocked: the whole point is a speaker whose firmware
/// nobody here wrote. This looks for one and skips when the room has none,
/// because "no speaker in pairing mode" is absence, not malfunction.
///
/// It deliberately stops at the inquiry. Pairing with a speaker leaves a
/// bond on a device somebody owns, and a test that silently re-pairs the
/// user's headphones every `cargo test` is a bad neighbour — the full ladder
/// is `cargo run --example a2dp_source`, which a person runs on purpose.
#[test]
fn an_inquiry_from_a_real_dongle_finds_a_speaker_in_pairing_mode() {
    use simble::device::ClassicHost;
    use simble::device::classic_host::{inquiry_mode, scan_enable};

    /// Class of Device major class Audio/Video, from the middle octet.
    const AUDIO_VIDEO_MAJOR_CLASS: u8 = 0x04;
    /// 8 × 1.28 s. Long enough for a speaker that answers slowly.
    const INQUIRY_LENGTH: u8 = 8;

    let _radio = radio_lock();
    let Some(selector) = dual_mode_dongle() else {
        return;
    };
    let mut transport = match UsbTransport::open_selected(&selector) {
        Ok(transport) => transport,
        Err(e) => {
            println!(
                "SKIP: {} stopped opening between probes: {e}",
                selector.describe()
            );
            return;
        }
    };

    let mut host = ClassicHost::new("simble-a2dp-probe", [0x0C, 0x02, 0x5A]);
    let channel = HciChannel::new();
    for packet in host.start_commands() {
        channel.inject_host_packet(packet).expect("queue bring-up");
    }
    for packet in host.set_scan_enable(scan_enable::NONE) {
        channel
            .inject_host_packet(packet)
            .expect("queue scan enable");
    }
    for packet in host.set_inquiry_mode(inquiry_mode::WITH_EXTENDED) {
        channel
            .inject_host_packet(packet)
            .expect("queue inquiry mode");
    }
    for packet in host.start_inquiry(INQUIRY_LENGTH) {
        channel.inject_host_packet(packet).expect("queue inquiry");
    }

    let deadline = Instant::now() + Duration::from_secs(INQUIRY_LENGTH as u64 * 2 + 5);
    while Instant::now() < deadline && !host.inquiry_finished() {
        if let Err(e) = transport.pump(&channel) {
            panic!("the dongle stopped answering mid-inquiry: {e}");
        }
        while let Some(packet) = channel.poll_controller_packet() {
            match host.handle_packet(&packet) {
                Ok(outgoing) => {
                    for out in outgoing {
                        let _ = channel.inject_host_packet(out);
                    }
                }
                Err(e) => eprintln!("host: {e}"),
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let found = host.discovered();
    for device in found {
        let cod = device.class_of_device;
        println!(
            "  saw {} class {:#08x}{}",
            device.address,
            u32::from_le_bytes([cod[0], cod[1], cod[2], 0]),
            match &device.name {
                Some(name) => format!(" {name:?}"),
                None => String::new(),
            }
        );
    }
    let speakers: Vec<_> = found
        .iter()
        .filter(|d| (d.class_of_device[1] & 0x1F) == AUDIO_VIDEO_MAJOR_CLASS)
        .collect();
    if speakers.is_empty() {
        println!(
            "SKIP: the inquiry ran on {} and found {} device(s), none of them \
             Audio/Video. Put a speaker in pairing mode and run \
             `cargo run --example a2dp_source` for the full ladder.",
            selector.describe(),
            found.len()
        );
        return;
    }
    for speaker in &speakers {
        println!(
            "ok: found an Audio/Video device at {} — the A2DP source example can target it",
            speaker.address
        );
    }
}
