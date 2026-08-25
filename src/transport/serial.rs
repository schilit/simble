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
use std::sync::mpsc;

use super::hci_adapter::HciChannel;
use super::rootcanal::H4FrameReader;
use crate::types::SimbleError;

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
        // for a CDC-ACM gadget never comes, so the process simply hangs with
        // no error to report. /dev/cu.* is the call-out node and opens at
        // once. Taking the cu form silently is right because they are the
        // same device, and a caller who names the tty node means the device,
        // not the dial-in semantics.
        let path = &if cfg!(target_os = "macos") {
            path.replace("/dev/tty.", "/dev/cu.")
        } else {
            path.to_string()
        };
        let stty = std::process::Command::new("stty")
            .args(if cfg!(target_os = "macos") {
                ["-f", path, "raw", "-echo"]
            } else {
                ["-F", path, "raw", "-echo"]
            })
            .status()
            .map_err(|e| SimbleError::Transport(format!("running stty for {path}: {e}")))?;
        if !stty.success() {
            return Err(SimbleError::Transport(format!(
                "stty could not put {path} into raw mode (exit {stty})"
            )));
        }

        let writer = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| SimbleError::Transport(format!("opening {path}: {e}")))?;
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
            self.writer
                .write_all(&packet)
                .map_err(|e| SimbleError::Transport(format!("serial write: {e}")))?;
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
