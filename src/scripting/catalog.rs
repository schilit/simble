// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! `catalog::*` — loading a shipped device by name, from a script.
//!
//! [`crate::devices::catalog`] is one registry with, until now, one consumer
//! per surface: the MCP `example` tool serves it to an agent, a scene file's
//! `"device": "hrm"` resolves against it, and the wasm export hands it to a
//! page. The one caller that could never reach it was the surface *written in
//! the same language as the catalog itself* — a Rhai script had no way to say
//! `catalog::device("hrm")` and had to re-type the device inline.
//!
//! # How a name becomes a device
//!
//! The entry is compiled and run **in the caller's own engine**, through
//! [`rhai::NativeCallContext::engine`]. That one decision is what makes the
//! rest fall out:
//!
//! - Every binding the entry needs is already there. Catalog devices call
//!   `server.update_value`, `add_pacs`, `add_ras`, `f32_le` — the scene
//!   extensions. A sub-engine would have to be rebuilt to match, and would
//!   drift the first time an extension was added.
//! - Events land in the **same session queue**, so `wait_for "service_added"`
//!   in the calling script sees the services the loaded device just published.
//!   A separate engine would have a separate queue and `wait_for` would sit
//!   there insisting nothing had happened.
//! - The value that comes back is a plain [`ScriptGattServer`], which is
//!   exactly what the scene host scans top-level variables for. So
//!   `let hrm = catalog::device("hrm");` in a scene script *is* the peripheral
//!   being added to that scene — no new host plumbing, and no change to how
//!   the scene collects its servers.
//!
//! What the entry's own `fn tick` needs is kept with it — see
//! [`CarriedScript`] — so `assert_over` and `server.advance(t)` can run the
//! loaded device's physics rather than staring at a frozen value.

use std::cell::Cell;
use std::rc::Rc;

use rhai::{Array, Dynamic, Engine, EvalAltResult, Module, NativeCallContext, Scope};

use crate::devices::catalog;
use crate::scripting::bindings::{CarriedScript, ScriptGattServer, runtime_error};

/// How deep `catalog::device` may nest. An entry that loads another entry is
/// legitimate composition; an entry that loads *itself* is a stack overflow,
/// and `Engine::set_max_call_levels` does not see it because the recursion
/// runs through native frames.
const MAX_DEPTH: u32 = 4;

thread_local! {
    static DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// Registers the `catalog` module: `device`, `names`, `source`.
pub fn register(engine: &mut Engine) {
    let mut module = Module::new();

    module.set_native_fn(
        "device",
        |context: NativeCallContext, name: &str| -> Result<ScriptGattServer, Box<EvalAltResult>> {
            device(&context, name)
        },
    );
    // What `device` would have accepted, so an error message is never the
    // only way to find out. Catalog order, peripherals then clients — the
    // same order the listings and the docs quote.
    module.set_native_fn("names", || -> Result<Array, Box<EvalAltResult>> {
        Ok(catalog::names().into_iter().map(Dynamic::from).collect())
    });
    // The entry's source, unrun. A test that wants to assert *about* a
    // catalog entry — that it still sets a CCCD, that it still defines a tick
    // — reads it here instead of duplicating the file.
    module.set_native_fn(
        "source",
        |name: &str| -> Result<String, Box<EvalAltResult>> {
            catalog::script(name)
                .map(str::to_string)
                .ok_or_else(|| unknown_name("catalog::source", name))
        },
    );

    engine.register_static_module("catalog", module.into());
}

/// Loads the catalog entry `name` and returns the server it built.
fn device(context: &NativeCallContext, name: &str) -> Result<ScriptGattServer, Box<EvalAltResult>> {
    let script = catalog::script(name).ok_or_else(|| unknown_name("catalog::device", name))?;

    let depth = DEPTH.with(|d| {
        let next = d.get() + 1;
        d.set(next);
        next
    });
    let result = (|| {
        if depth > MAX_DEPTH {
            return Err(runtime_error(format!(
                "catalog::device(\"{name}\"): nested {MAX_DEPTH} deep — a catalog entry is \
                 loading itself, directly or in a cycle"
            )));
        }
        run_entry(context, name, script)
    })();
    DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    result
}

/// Compiles and runs one entry in the caller's engine, then picks the server
/// out of the scope it left behind.
fn run_entry(
    context: &NativeCallContext,
    name: &str,
    script: &'static str,
) -> Result<ScriptGattServer, Box<EvalAltResult>> {
    let engine = context.engine();
    let ast = engine.compile(script).map_err(|e| {
        runtime_error(format!(
            "catalog::device(\"{name}\"): the catalog entry does not compile: {e}"
        ))
    })?;
    let mut scope = Scope::new();
    engine
        .run_ast_with_scope(&mut scope, &ast)
        .map_err(|e| runtime_error(format!("catalog::device(\"{name}\"): {e}")))?;

    // The first server the entry bound, matching how the scene host picks a
    // peripheral's primary server out of its top-level variables.
    let server = scope
        .iter()
        .find_map(|(_, _, value)| value.try_cast::<ScriptGattServer>())
        .ok_or_else(|| {
            runtime_error(format!(
                "catalog::device(\"{name}\"): that entry builds no \
                 android::BluetoothGattServer{}",
                if catalog::central(name).is_some() {
                    " — it is a central (client) script, which drives a device \
                     instead of being one"
                } else {
                    ""
                }
            ))
        })?;

    let has_tick = ast
        .iter_functions()
        .any(|f| f.name == "tick" && f.params.len() == 2);
    server.attach_script(CarriedScript {
        name: name.to_string(),
        has_tick,
        ast: Rc::new(ast),
        scope: Rc::new(std::cell::RefCell::new(scope)),
        state: Rc::new(std::cell::RefCell::new(Dynamic::from_map(rhai::Map::new()))),
    });
    Ok(server)
}

/// The "no such entry" error, with the names the caller most likely meant.
///
/// A bare "unknown device" makes the reader go and find the catalog. A
/// misspelling is by far the common case, so lead with the near-misses and
/// keep the full list one call away.
fn unknown_name(function: &str, name: &str) -> Box<EvalAltResult> {
    let suggestions = near_misses(name);
    let tail = if suggestions.is_empty() {
        format!("known names: {}", catalog::names_joined())
    } else {
        format!(
            "did you mean {}? (catalog::names() lists all {})",
            suggestions.join(", "),
            catalog::names().len()
        )
    };
    runtime_error(format!(
        "{function}(\"{name}\"): no such catalog entry — {tail}"
    ))
}

/// The catalog names closest to `name`, best first, at most three.
///
/// Two kinds of near-miss, because they are two different mistakes: a
/// *substring* match is someone who remembered part of the name
/// (`"keyboard"` -> `hid_keyboard`), and a small *edit distance* is someone
/// who typed it slightly wrong (`"hrmm"` -> `hrm`). Separators are neutral,
/// so `hid-keyboard` finds `hid_keyboard`.
fn near_misses(name: &str) -> Vec<&'static str> {
    let needle = normalize(name);
    let mut scored: Vec<(u8, u32, &'static str)> = catalog::names()
        .into_iter()
        .filter_map(|candidate| {
            let hay = normalize(candidate);
            let distance = edit_distance(&needle, &hay);
            // Allow a slack of one edit per four characters, so short names
            // do not match everything and long ones are not held to perfection.
            let budget = (needle.len().max(hay.len()) / 4 + 1) as u32;
            let rank = if hay == needle {
                0
            } else if hay.contains(&needle) || needle.contains(&hay) {
                1
            } else if distance <= budget {
                2
            } else {
                return None;
            };
            Some((rank, distance, candidate))
        })
        .collect();
    scored.sort_by_key(|&(rank, distance, candidate)| (rank, distance, candidate));
    scored.truncate(3);
    scored.into_iter().map(|(_, _, name)| name).collect()
}

/// Lowercases and drops `_`/`-`/space, so the separator a caller guessed does
/// not decide whether they get a suggestion.
fn normalize(name: &str) -> String {
    name.chars()
        .filter(|c| !matches!(c, '_' | '-' | ' '))
        .flat_map(char::to_lowercase)
        .collect()
}

/// Levenshtein distance, two rows at a time.
fn edit_distance(a: &str, b: &str) -> u32 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len() as u32;
    }
    let mut previous: Vec<u32> = (0..=b.len() as u32).collect();
    let mut current = vec![0u32; b.len() + 1];
    for (i, &ac) in a.iter().enumerate() {
        current[0] = i as u32 + 1;
        for (j, &bc) in b.iter().enumerate() {
            let substitution = previous[j] + u32::from(ac != bc);
            current[j + 1] = substitution.min(previous[j + 1] + 1).min(current[j] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_misspelling_suggests_the_name_that_was_meant() {
        assert_eq!(near_misses("hrmm").first(), Some(&"hrm"));
        assert_eq!(near_misses("thermomter").first(), Some(&"thermometer"));
        // The separator is not the point.
        assert_eq!(near_misses("hid-keyboard").first(), Some(&"hid_keyboard"));
        // A remembered fragment finds the whole name.
        assert!(near_misses("keyboard").contains(&"hid_keyboard"));
    }

    #[test]
    fn nonsense_suggests_nothing_rather_than_everything() {
        assert!(
            near_misses("qqqqqqqqqqqqqqqq").is_empty(),
            "a name with nothing near it should fall back to the full list"
        );
    }

    #[test]
    fn the_unknown_name_error_names_the_near_misses() {
        let message = unknown_name("catalog::device", "hrmm").to_string();
        assert!(message.contains("hrmm"), "{message}");
        assert!(message.contains("did you mean"), "{message}");
        assert!(message.contains("hrm"), "{message}");
        assert!(message.contains("catalog::names()"), "{message}");

        // With no near-miss the message still has to be actionable.
        let message = unknown_name("catalog::device", "qqqqqqqqqqqqqqqq").to_string();
        assert!(message.contains("known names:"), "{message}");
        assert!(message.contains("hrm"), "{message}");
    }

    #[test]
    fn edit_distance_is_levenshtein() {
        assert_eq!(edit_distance("", "abc"), 3);
        assert_eq!(edit_distance("abc", ""), 3);
        assert_eq!(edit_distance("abc", "abc"), 0);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
    }
}
