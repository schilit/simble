// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! The API Explorer, checked against the API it claims to document.
//!
//! `web/explorer/explorer.js` is the inventory of the Rhai surface — every
//! type, method, property, callback and constant family, with prose and a
//! runnable form. It went stale the ordinary way: `catalog::*`,
//! `assert_over`, `android::BluetoothHidHost` and **both Auracast proxies**
//! landed in `src/scripting/`, and nothing anywhere compared the two. The
//! page was still true about what it said; it was silent about a third of the
//! surface, and silence is the failure mode no reader can detect.
//!
//! So this test walks the registration sites and fails when a registered name
//! has no entry on the page. It is the same move `scripts/check_hci_command_answers.py`
//! makes for the HCI command table: the documentation is derived from the
//! code by a machine, on every run, rather than by a person, once.
//!
//! # What it checks
//!
//! Four rules, in increasing strictness:
//!
//! 1. **Every `register_type_with_name` name** appears as `android::<Name>`
//!    or as a bare `<Name>` heading. A whole new script type landing
//!    undocumented is the biggest possible gap and the easiest to catch.
//! 2. **Every module function** — `set_native_fn` into a named static module
//!    — appears fully qualified: `catalog::device`, `uuid::of`,
//!    `android::BluetoothGattServer`. Exact, because the module prefix is
//!    part of how a script spells it.
//! 3. **Every `on_*` callback name** the runtime dispatches appears as
//!    `fn on_<name>`. Exact, because the page documents callbacks with their
//!    signature and the runtime matches on name *and* arity.
//! 4. **Every `register_fn` / `register_get` name** appears somewhere in a
//!    `sig:` entry on the page.
//!
//! # What it cannot cover, and why that is stated rather than hidden
//!
//! Rule 4 is name-based, not overload-aware or receiver-aware. Rhai's registry
//! is keyed by (name, argument types), and the page is organised by receiver
//! type, but a regex over Rust source cannot reliably recover either. Three
//! consequences, all of them real:
//!
//! - **A new overload of a documented name passes.** `assert_over` has four
//!   arities; documenting one satisfies this test for all four.
//! - **A name shared between receivers passes if any one is documented.**
//!   `connect` is registered on `BluetoothGatt` and on `BluetoothHidHost`; had
//!   only the first been on the page, rule 4 would not have noticed. Rule 1
//!   would have — the *type* would have been missing — which is why rule 1
//!   exists and why a new type is the case worth being strict about.
//! - **It checks presence, not truth.** Nothing here can tell whether the
//!   prose is right, whether an argument list still matches, or whether a
//!   member labelled `mode: "ref"` still needs to be. Those stay a reader's
//!   job.
//!
//! Deliberately **not** checked: the count of `mode: "doc"` / `mode: "ref"`
//! members. Pinning it would turn an honest label into a number someone has to
//! bump, and `docs/gaps.md` §4 exists precisely because those labels get
//! "fixed" by deletion. A label is a claim about what this page can host, and
//! a test that made it expensive to add one would push the next person toward
//! the wrong bucket.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The registration sites, and the static module each file's `set_native_fn`
/// calls register into. Derived by reading the `register_static_module` calls:
/// `bindings.rs` builds the `android` module and hands it to `client`,
/// `broadcast` and `hid`; `catalog.rs` and `constants.rs` build their own.
const SITES: &[(&str, &str)] = &[
    ("src/scripting/bindings.rs", "android"),
    ("src/scripting/client.rs", "android"),
    ("src/scripting/broadcast.rs", "android"),
    ("src/scripting/hid.rs", "android"),
    ("src/scripting/catalog.rs", "catalog"),
    ("src/scripting/monitor.rs", "android"),
    ("src/scripting/constants.rs", "uuid"),
    ("src/transport/wasm_ws.rs", "android"),
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(relative: &str) -> String {
    let path = repo_root().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

fn explorer_js() -> String {
    read("web/explorer/explorer.js")
}

/// Strips `//` line comments so a name that only appears in a Rust doc comment
/// is not mistaken for a registration. (`//!` module docs quote script
/// snippets — `catalog::device("hrm")`, `fn on_key_down(host, key)` — which
/// would otherwise register themselves.)
fn without_line_comments(source: &str) -> String {
    source
        .lines()
        .map(|line| match line.find("//") {
            // Only strip a `//` that is not inside a string literal. Counting
            // unescaped quotes before it is enough here: no registration line
            // in these files puts a `//` inside a string.
            Some(at) if line[..at].matches('"').count() % 2 == 0 => &line[..at],
            _ => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every name passed to `call` in `source`: the first string literal after
/// each occurrence, comments already stripped so a wrapped call whose name
/// sits on the next line still resolves.
fn registered_names(source: &str, call: &str) -> BTreeSet<String> {
    let cleaned = without_line_comments(source);
    let mut names = BTreeSet::new();
    let mut rest = cleaned.as_str();
    while let Some(at) = rest.find(call) {
        rest = &rest[at + call.len()..];
        // `register_fn::<A, B>("name", …)` — skip a turbofish, then find the
        // opening quote of the first argument.
        let Some(quote) = rest.find('"') else { break };
        let after = &rest[quote + 1..];
        let Some(end) = after.find('"') else { break };
        names.insert(after[..end].to_string());
    }
    names
}

/// True if `needle` appears in `haystack` bounded by non-identifier characters
/// on both sides — so `connect` does not match inside `disconnect`.
fn contains_token(haystack: &str, needle: &str) -> bool {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut from = 0;
    while let Some(at) = haystack[from..].find(needle) {
        let start = from + at;
        let end = start + needle.len();
        let before_ok = haystack[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !ident(c));
        let after_ok = haystack[end..].chars().next().is_none_or(|c| !ident(c));
        if before_ok && after_ok {
            return true;
        }
        from = end;
    }
    false
}

/// Every `sig:` value on the page, concatenated. This is the page's contract
/// with a reader — a name mentioned only in passing prose does not count as
/// documented, because a reader searching for a call looks at signatures.
fn documented_signatures(js: &str) -> String {
    let mut out = String::new();
    let mut rest = js;
    while let Some(at) = rest.find("sig: ") {
        rest = &rest[at + "sig: ".len()..];
        let Some(quote) = rest.chars().next() else {
            break;
        };
        if quote != '"' && quote != '\'' && quote != '`' {
            continue;
        }
        let body = &rest[1..];
        // No `sig:` literal contains an escaped quote of its own delimiter.
        let Some(end) = body.find(quote) else { break };
        out.push_str(&body[..end]);
        out.push('\n');
    }
    out
}

/// Rule 1: a script type with no section on the page is the largest gap the
/// Explorer can have, and the one that actually happened three times.
#[test]
fn every_script_type_is_documented() {
    let js = explorer_js();
    let mut missing = Vec::new();
    for (file, _) in SITES {
        for name in registered_names(&read(file), "register_type_with_name") {
            // `android::Foo` is how a script spells the constructor; a plain
            // value type (`Uuid`, `BluetoothDevice`) is a section heading.
            if !contains_token(&js, &name) {
                missing.push(format!("{name} (registered in {file})"));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "web/explorer/explorer.js documents no section for {} script type(s):\n  {}\n\
         Add a TYPES entry and its members, and classify each one exec / ref / doc.",
        missing.len(),
        missing.join("\n  ")
    );
}

/// Rule 2: module functions, fully qualified. The prefix is part of the
/// spelling, so `catalog::names` documented as a bare `names()` is not
/// documented.
#[test]
fn every_module_function_is_documented_fully_qualified() {
    let js = explorer_js();
    let mut missing = Vec::new();
    for (file, module) in SITES {
        for name in registered_names(&read(file), "set_native_fn") {
            let qualified = format!("{module}::{name}");
            if !js.contains(&qualified) {
                missing.push(format!("{qualified} (registered in {file})"));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "web/explorer/explorer.js never spells {} module function(s):\n  {}",
        missing.len(),
        missing.join("\n  ")
    );
}

/// Rule 3: callbacks, by the `fn name` a script writes. Dispatch is by name
/// and arity, so the page documents the whole signature; this checks the half
/// a regex can check.
#[test]
fn every_dispatched_callback_is_documented() {
    let js = explorer_js();
    let mut names = BTreeSet::new();
    for (file, _) in SITES {
        let source = without_line_comments(&read(file));
        let mut rest = source.as_str();
        while let Some(at) = rest.find("\"on_") {
            rest = &rest[at + 1..];
            let Some(end) = rest[1..].find('"') else {
                break;
            };
            let name = &rest[..end + 1];
            // A dispatched callback name is an identifier and nothing else;
            // this excludes prose and JSON keys that merely start with `on_`.
            if name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            {
                names.insert(name.to_string());
            }
        }
    }
    assert!(
        names.len() > 30,
        "extraction broke: found only {} callback names, expected the whole dispatch table",
        names.len()
    );
    let missing: Vec<_> = names
        .iter()
        .filter(|name| !js.contains(&format!("fn {name}")))
        .cloned()
        .collect();
    assert!(
        missing.is_empty(),
        "web/explorer/explorer.js documents no `fn …` signature for {} callback(s):\n  {}\n\
         These are mode 'doc': a callback is defined, not called, so it gets no form.",
        missing.len(),
        missing.join("\n  ")
    );
}

/// Rule 4: methods and properties, by name, against the page's signatures.
/// The weakest of the four — see the module docs for the three things it
/// cannot see — but it is the one that covers the bulk of the surface.
#[test]
fn every_registered_function_appears_in_a_signature() {
    let js = explorer_js();
    let sigs = documented_signatures(&js);
    // A floor for "the regex stopped matching", not for "the page shrank" —
    // deliberately far below any plausible page, so this guard never fires
    // ahead of the real assertion and hides which names are missing.
    assert!(
        sigs.lines().count() > 40,
        "extraction broke: found only {} sig: entries on the page",
        sigs.lines().count()
    );

    let mut missing = Vec::new();
    for (file, _) in SITES {
        let source = read(file);
        for call in ["register_fn", "register_get"] {
            for name in registered_names(&source, call) {
                // Operators are documented as `a == b` / `a != b`, which the
                // token rule cannot match — `=` is not an identifier
                // character, so both sides of `==` are already boundaries.
                let found = if name.chars().any(|c| c.is_ascii_alphanumeric()) {
                    contains_token(&sigs, &name)
                } else {
                    sigs.contains(&name)
                };
                if !found {
                    missing.push(format!("{name} ({call} in {file})"));
                }
            }
        }
    }
    assert!(
        missing.is_empty(),
        "web/explorer/explorer.js has no signature mentioning {} registered name(s):\n  {}\n\
         Add a METHODS entry. If the call cannot run in the Explorer's session — it pumps only \n\
         ScriptGattServers — mark it mode 'ref' and say why, rather than leaving it off the page.",
        missing.len(),
        missing.join("\n  ")
    );
}

/// The custom syntax is neither a function nor a type, so it needs its own
/// line. There is exactly one; if a second is ever added this fails until the
/// page grows an entry for it.
#[test]
fn every_custom_syntax_is_documented() {
    let js = explorer_js();
    for (file, _) in SITES {
        for name in registered_names(&read(file), "register_custom_syntax") {
            assert!(
                contains_token(&js, &name),
                "custom syntax `{name}` (registered in {file}) has no entry in \
                 web/explorer/explorer.js"
            );
        }
    }
}

/// The registration sites this test walks must exist. A file renamed or split
/// would otherwise make every rule above pass vacuously — the exact way a
/// derived check rots.
#[test]
fn every_registration_site_still_exists() {
    for (file, _) in SITES {
        let path = repo_root().join(file);
        assert!(
            Path::new(&path).is_file(),
            "SITES lists {file}, which does not exist. If the scripting layer moved, update \
             tests/explorer_surface_test.rs — otherwise these checks silently pass on nothing."
        );
    }
    // And each must actually register something, so a file that survives as an
    // empty shell cannot pass either.
    let total: usize = SITES
        .iter()
        .map(|(file, _)| {
            let source = read(file);
            registered_names(&source, "register_fn").len()
                + registered_names(&source, "register_get").len()
                + registered_names(&source, "set_native_fn").len()
        })
        .sum();
    assert!(
        total > 100,
        "extraction found only {total} distinct registered names across {} sites; the regexes \
         have stopped matching the code they read",
        SITES.len()
    );
}
