// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! `simble-test` — run a SimBLE Rhai test script and report pass/fail.
//!
//! A device is a Rhai script; add `assert(...)` and the same script is a test.
//! This runs it in a fresh SimBLE scripting engine (no netsim, no connection),
//! prints the result, and exits `0` on pass or `1` on a failed assertion — so a
//! `.rhai` fixture drops straight into CI.
//!
//! ```text
//! simble-test path/to/test.rhai     # run a file
//! simble-test < test.rhai           # or read the script from stdin
//! ```

use simble::transport::wasm_ws::run_test_script;
use std::io::Read;
use std::process::ExitCode;

fn main() -> ExitCode {
    let arg = std::env::args().nth(1);
    let script = match arg.as_deref() {
        Some("-h") | Some("--help") => {
            eprintln!("usage: simble-test [FILE]   (reads stdin if FILE is omitted)");
            return ExitCode::SUCCESS;
        }
        Some(path) => match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("simble-test: cannot read {path}: {e}");
                return ExitCode::from(2);
            }
        },
        None => {
            let mut s = String::new();
            if let Err(e) = std::io::stdin().read_to_string(&mut s) {
                eprintln!("simble-test: cannot read stdin: {e}");
                return ExitCode::from(2);
            }
            s
        }
    };

    match run_test_script(&script) {
        Ok(()) => {
            println!("PASS — all assertions held");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("FAIL — {message}");
            ExitCode::FAILURE
        }
    }
}
