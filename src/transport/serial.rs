// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! H4 over a serial device — the transport for a controller that presents as
//! a tty, which on this project means a Zephyr `hci_uart` build exposing
//! CDC-ACM over USB.
//!
//! This exists because the USB *Bluetooth class* path could not carry ISO
//! data (Zephyr's class has no host-to-controller ISO path; upstream issue
//! #44013) and proved unstable under sustained traffic — which matches
//! Nordic's own guidance that HCI-over-USB is "slightly unstable by design"
//! and that stressed transports should run H4 over CDC-ACM instead. H4
//! framing carries the packet type in-band, so commands, events, ACL and ISO
//! all ride one byte stream with nothing invented.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::sync::mpsc;

use super::hci_adapter::HciChannel;
use super::rootcanal::H4FrameReader;
use crate::types::SimbleError;

/// How long a write waits out backpressure before calling the controller
/// wedged. Generous next to any real drain, short next to a person waiting.
const WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// H4 over a tty. Writes go straight to the device; reads arrive via a
/// blocking reader thread, because a portable non-blocking tty read needs
/// termios flags std does not expose.
pub struct SerialTransport {
    writer: std::fs::File,
    inbound: mpsc::Receiver<Vec<u8>>,
    /// Reader-thread failure, delivered on the next pump rather than lost.
    errors: mpsc::Receiver<String>,
}

impl SerialTransport {
    /// Opens `path` as a raw byte pipe.
    ///
    /// The tty is put into raw mode with `stty`, shelling out rather than
    /// binding termios: this crate keeps its no-FFI rule, and one exec at
    /// open time against a tool present on every Unix is the cheapest
    /// honest way to stop the line discipline from cooking binary H4
    /// (echo, CR/LF mapping, ^C — any of which corrupts the stream).
    pub fn open(path: &str) -> Result<Self, SimbleError> {
        // macOS gives every serial device two nodes: /dev/tty.* is the
        // dial-IN device and blocks in open(2) until carrier detect — which
        // for a CDC-ACM gadget never comes. /dev/cu.* is the call-out node.
        // The two are the same device, and a caller naming the tty node
        // means the device, not the dial-in semantics.
        let path = &if cfg!(target_os = "macos") {
            path.replace("/dev/tty.", "/dev/cu.")
        } else {
            path.to_string()
        };

        // O_NONBLOCK at open, for two reasons. It sidesteps the carrier wait
        // that hangs open(2) on some CDC gadgets even via the call-out node,
        // and it turns a controller that has stopped draining into a
        // reported error rather than a process parked forever in write(2) —
        // which is exactly how an over-run ISO stream first presented.
        const O_NONBLOCK: i32 = if cfg!(target_os = "macos") {
            0x0004
        } else {
            0o4000
        };
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(O_NONBLOCK)
            .open(path)
            .map_err(|e| SimbleError::Transport(format!("opening {path}: {e}")))?;

        // Raw mode, or the line discipline cooks binary H4: echo alone would
        // write every received byte back at the controller. `stty` is shelled
        // out rather than binding termios because this crate takes no FFI —
        // and it is handed the ALREADY-OPEN descriptor as its stdin, because
        // `stty -f <path>` opens the device itself and blocks doing so.
        let stty_stdin = file
            .try_clone()
            .map_err(|e| SimbleError::Transport(format!("cloning {path}: {e}")))?;
        let stty = std::process::Command::new("stty")
            .args(["raw", "-echo"])
            .stdin(std::process::Stdio::from(stty_stdin))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|e| SimbleError::Transport(format!("running stty for {path}: {e}")))?;
        if !stty.success() {
            return Err(SimbleError::Transport(format!(
                "stty could not put {path} into raw mode (exit {stty})"
            )));
        }

        let writer = file;
        let mut reader = writer
            .try_clone()
            .map_err(|e| SimbleError::Transport(format!("cloning {path}: {e}")))?;

        let (tx, inbound) = mpsc::channel();
        let (err_tx, errors) = mpsc::channel();
        std::thread::Builder::new()
            .name("simble-serial-rx".into())
            .spawn(move || {
                let mut framer = H4FrameReader::default();
                let mut chunk = [0u8; 4096];
                loop {
                    match reader.read(&mut chunk) {
                        Ok(0) => {
                            let _ = err_tx.send("serial device closed".to_string());
                            return;
                        }
                        Ok(n) => {
                            framer.feed(&chunk[..n]);
                            loop {
                                match framer.next_packet() {
                                    Ok(Some(packet)) => {
                                        if tx.send(packet).is_err() {
                                            return; // transport dropped; done
                                        }
                                    }
                                    Ok(None) => break,
                                    Err(e) => {
                                        let _ = err_tx.send(format!("H4 framing: {e}"));
                                        return;
                                    }
                                }
                            }
                        }
                        // Nothing to read yet: the descriptor is non-blocking,
                        // so this is the idle case, not a failure.
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(std::time::Duration::from_millis(1));
                        }
                        Err(e) => {
                            let _ = err_tx.send(format!("serial read: {e}"));
                            return;
                        }
                    }
                }
            })
            .map_err(|e| SimbleError::Transport(format!("spawning the reader thread: {e}")))?;

        Ok(Self {
            writer,
            inbound,
            errors,
        })
    }

    /// Writes one whole packet, waiting out short-term backpressure but
    /// giving up rather than parking forever. A controller with its buffers
    /// full stops reading the line; a host that blocks there stops being
    /// able to say so.
    fn write_packet(&mut self, packet: &[u8]) -> Result<(), SimbleError> {
        let deadline = std::time::Instant::now() + WRITE_TIMEOUT;
        let mut written = 0;
        while written < packet.len() {
            match self.writer.write(&packet[written..]) {
                Ok(0) => {
                    return Err(SimbleError::Transport(
                        "serial write accepted nothing — the controller is not draining".into(),
                    ));
                }
                Ok(n) => written += n,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if std::time::Instant::now() > deadline {
                        return Err(SimbleError::Transport(format!(
                            "serial write stalled for {WRITE_TIMEOUT:?} with {} of {} bytes out — \
                             the controller has stopped draining (its buffers are full, or it \
                             is wedged)",
                            written,
                            packet.len()
                        )));
                    }
                    std::thread::sleep(std::time::Duration::from_micros(200));
                }
                Err(e) => return Err(SimbleError::Transport(format!("serial write: {e}"))),
            }
        }
        Ok(())
    }
}

impl super::HciTransport for SerialTransport {
    fn pump(&mut self, channel: &HciChannel) -> Result<(), SimbleError> {
        if let Ok(error) = self.errors.try_recv() {
            return Err(SimbleError::Transport(error));
        }
        let trace = std::env::var_os("SIMBLE_HCI_LOG").is_some();
        while let Some(packet) = channel.poll_host_packet() {
            if trace {
                eprintln!("host -> ctlr  {:02X?}", packet);
            }
            self.write_packet(&packet)?;
        }
        while let Ok(packet) = self.inbound.try_recv() {
            if trace {
                eprintln!("ctlr -> host  {:02X?}", &packet[..packet.len().min(24)]);
            }
            channel.receive_from_controller(packet)?;
        }
        Ok(())
    }
}
