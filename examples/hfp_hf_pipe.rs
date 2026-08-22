// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Simble's Hands-Free side on a pipe, so a *foreign* Audio Gateway can
//! drive it.
//!
//! Simble's own HFP tests answer one half of the profile with the other
//! half, which proves the two agree and nothing about the wire. This
//! example exposes [`HfProtocol`] over stdin/stdout so Bumble's
//! `AgProtocol` — an independent implementation of the same spec — can be
//! the peer instead. See `tests/interop/hfp_oracle.py`.
//!
//! Line protocol, one line per message, both directions:
//!
//! ```text
//! < <hex>          bytes from the AG, to be fed to HfProtocol::receive
//! ! <command>      make the head unit do something (answer, hangup, ...)
//! > <hex>          bytes the HF wants written to the AG
//! # <event>        an HfpEvent the HF raised
//! .                end of turn
//! ```
//!
//! The feature set is the head unit's from
//! [`simble::device::car_kit`], so the oracle checks the same
//! configuration the Car page puts on the air.

use std::io::{BufRead, Write};

use simble::classic::hfp::{HfProtocol, HfpEvent, VoiceRecognitionState};
use simble::device::car_kit::head_unit_configuration;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len() / 2)
        .map(|i| u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).expect("hex digits"))
        .collect()
}

/// Renders an event as one stable line, so the Python side can assert on it
/// without knowing Rust's `Debug` formatting.
fn describe(event: &HfpEvent) -> String {
    match event {
        HfpEvent::SlcComplete => "slc-complete".into(),
        HfpEvent::CommandCompleted { command, ok, .. } => {
            format!("command-completed {command} {ok}")
        }
        HfpEvent::AgIndicatorUpdated(state) => {
            format!("indicator {:?} {}", state.indicator, state.current_status)
        }
        HfpEvent::CodecNegotiated(codec) => format!("codec {codec:?}"),
        HfpEvent::SpeakerVolume(level) => format!("speaker {level}"),
        HfpEvent::MicrophoneVolume(level) => format!("microphone {level}"),
        HfpEvent::Ring => "ring".into(),
        HfpEvent::CliNotification(cli) => format!("clip {}", cli.number),
        HfpEvent::VoiceRecognition(state) => format!("voice-recognition {state:?}"),
        other => format!("other {other:?}"),
    }
}

fn main() {
    let mut hf = HfProtocol::new(head_unit_configuration());
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    // The HF opens the Service Level Connection, so speak first.
    let opening = hf.start_slc();
    let _ = writeln!(stdout, "> {}", hex(&opening));
    let _ = writeln!(stdout, ".");
    let _ = stdout.flush();

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        let (outgoing, events) = match line.split_once(' ') {
            Some(("<", payload)) => hf.receive(&unhex(payload)),
            Some(("!", command)) => (vec![act(&mut hf, command)], Vec::new()),
            _ if line == "!" => break,
            _ => continue,
        };
        for blob in outgoing {
            if !blob.is_empty() {
                let _ = writeln!(stdout, "> {}", hex(&blob));
            }
        }
        for event in &events {
            let _ = writeln!(stdout, "# {}", describe(event));
        }
        let _ = writeln!(stdout, ".");
        let _ = stdout.flush();
    }
}

fn act(hf: &mut HfProtocol, command: &str) -> Vec<u8> {
    match command.split_once(' ') {
        Some(("dial", number)) => hf.dial(number),
        Some(("raw", text)) => hf.send_command(text),
        _ => match command {
            "answer" => hf.answer_incoming_call(),
            "hangup" => hf.terminate_call(),
            "calls" => hf.query_current_calls(),
            "operator-format" => hf.select_operator_format(),
            "operator" => hf.query_network_operator(),
            "voice-on" => hf.set_voice_recognition(VoiceRecognitionState::Enable),
            "voice-off" => hf.set_voice_recognition(VoiceRecognitionState::Disable),
            _ => Vec::new(),
        },
    }
}
