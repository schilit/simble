// Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
//
// Stamps a build identity into the binary, so a stale one can say so.
//
// The MCP registration recipe pins a client to `target/release/simble`, and
// nothing rebuilds it. That is not hypothetical: the registered binary was
// found a day stale, missing `add_central` and still serving the four
// *invented* RAS characteristic UUIDs whose source had been corrected hours
// earlier. `serverInfo.version` was `CARGO_PKG_VERSION` — a hardcoded 0.1.0 —
// so an agent talking to it had no way to tell, and a `tools/list` that
// disagreed with the docs looked like a documentation bug.
//
// A git description costs nothing to produce and makes the mismatch visible
// in the handshake.

use std::process::Command;

fn main() {
    // Rebuild the stamp when HEAD moves, not just when sources change.
    for path in [".git/HEAD", ".git/refs/heads/main"] {
        if std::path::Path::new(path).exists() {
            println!("cargo:rerun-if-changed={path}");
        }
    }

    let describe = Command::new("git")
        .args(["describe", "--always", "--dirty", "--tags"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        // A source tarball has no git metadata; that is not an error, it just
        // means the stamp cannot be more specific than the crate version.
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=SIMBLE_BUILD={describe}");
}
