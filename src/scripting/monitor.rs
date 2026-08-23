// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! `assert_over` — the temporal assertion, on the Rhai test surface.
//!
//! `assert(...)` says *this happened*. A monitor says *this stayed true*, and
//! those are different claims: a heart-rate monitor that reads 72 once has
//! proved nothing about the spike at t=1.4s. The MCP tool `assert_over`
//! (`mcp.rs::tool_assert_over`) has said the second thing since the temporal
//! monitor landed; a script author could only ever say the first.
//!
//! # The shape, and where it came from
//!
//! The semantics here are the MCP monitor's, deliberately unchanged, so that
//! "monitor HR < 200 for 3s" means the same thing whichever surface asks:
//!
//! - a window (`seconds`, default 2.0) sampled in 0.1 s steps,
//! - one byte of the characteristic's value (`byte`, default 1 — index 0 is
//!   the flags octet in nearly every SIG measurement format),
//! - an operator that must hold on **every** sample, failing on the first
//!   violation and reporting the offending value,
//! - and, on the way past, the *extreme* — the sample that came closest to
//!   breaking the rule, which is the number that tells you whether a passing
//!   monitor passed comfortably or by one.
//!
//! # What differs, and why it is not a copy

//! The two surfaces sample different worlds. MCP has a `SceneEngine` with a
//! connected central: it subscribes, advances the scene clock, re-reads over
//! the air and parses hex out of the central's status JSON. A test script has
//! no central and no radio — it holds the device itself, so it samples the
//! peripheral's own GATT database through the same `server.value(uuid)`
//! binding a script would use, and advances time by running the device's own
//! `fn tick` (see [`crate::scripting::bindings::CarriedScript`]).
//!
//! What *should* be shared is the middle: [`compare`] and [`extreme_for`],
//! plus the step count. Those three live here now, and `mcp.rs` still holds
//! private copies of the first two. Unifying them is a real refactor — the
//! MCP module is native-only (`#[cfg(not(target_arch = "wasm32"))]`) while
//! `scripting` builds for wasm too, so `mcp.rs` must depend on this module
//! and not the reverse — and it cannot be done without editing `src/mcp.rs`.
//! Until then this module is the *declared* owner of the semantics and the
//! test below pins them against the operator table `mcp.rs` uses.

use rhai::{Blob, Dynamic, Engine, EvalAltResult, NativeCallContext};

use crate::scripting::bindings::{ScriptGattServer, runtime_error};
use crate::types::Uuid;

/// The monitor's sampling interval, in seconds. Matches the MCP monitor: a
/// window is `ceil(seconds / STEP)` samples, at least one.
pub const STEP_SECONDS: f64 = 0.1;

/// The default window when a script names none, in seconds.
pub const DEFAULT_SECONDS: f64 = 2.0;

/// The default byte index: 1, past the flags octet that opens nearly every
/// SIG measurement value (Heart Rate Measurement, Temperature Measurement,
/// PLX Spot-Check, ...).
pub const DEFAULT_BYTE: usize = 1;

/// Evaluates `actual <op> threshold`; `None` for an unrecognized operator.
///
/// The operator table is the monitor's contract with the script author, and
/// it is the same table the MCP tool advertises in its JSON schema.
pub fn compare(actual: i64, op: &str, threshold: i64) -> Option<bool> {
    Some(match op {
        "<" => actual < threshold,
        ">" => actual > threshold,
        "<=" => actual <= threshold,
        ">=" => actual >= threshold,
        "==" => actual == threshold,
        "!=" => actual != threshold,
        _ => return None,
    })
}

/// The sample furthest toward violating `op` — the max for `<`/`<=`, the min
/// for `>`/`>=` — so a passing monitor can report how close it came.
pub fn extreme_for(op: &str, a: i64, b: i64) -> i64 {
    match op {
        ">" | ">=" => a.min(b),
        _ => a.max(b),
    }
}

/// Every operator the monitor understands, in the order a message lists them.
pub const OPERATORS: &[&str] = &["<", ">", "<=", ">=", "==", "!="];

/// How many samples a window of `seconds` takes. Always at least one, so
/// `assert_over(..., 0.0)` still checks the value it starts with.
pub fn steps_for(seconds: f64) -> usize {
    ((seconds / STEP_SECONDS).ceil() as usize).max(1)
}

/// Runs one step of `server`'s own script — the `fn tick(server, t)` of the
/// catalog entry it was loaded from — at time `t`.
///
/// A server with no carried script, or a carried script with no `fn tick`, is
/// a no-op and *not* an error: a static device is a legitimate thing to
/// monitor, it simply never changes.
pub fn advance(
    engine: &Engine,
    server: &ScriptGattServer,
    t: f64,
) -> Result<(), Box<EvalAltResult>> {
    let Some(carried) = server.carried_script() else {
        return Ok(());
    };
    if !carried.has_tick {
        return Ok(());
    }
    // A `fn tick` that calls `advance` on its own device would re-enter these
    // borrows. Fail with a sentence rather than a RefCell panic.
    let (Ok(mut scope), Ok(mut state)) = (
        carried.scope.try_borrow_mut(),
        carried.state.try_borrow_mut(),
    ) else {
        return Err(runtime_error(format!(
            "advance: {} is already ticking — a device's own fn tick cannot advance itself",
            server.label()
        )));
    };
    // eval_ast(false): the entry's body already ran when it was loaded;
    // re-running it here would rebuild the device every step. bind_this_ptr:
    // `this` is how a catalog `fn tick` remembers anything, and the host
    // binds it the same way.
    let options = rhai::CallFnOptions::new()
        .eval_ast(false)
        .bind_this_ptr(&mut state);
    engine
        .call_fn_with_options::<Dynamic>(
            options,
            &mut scope,
            &carried.ast,
            "tick",
            (Dynamic::from(server.clone()), t),
        )
        .map(|_| ())
        .map_err(|e| {
            runtime_error(format!(
                "advance: {} raised an error in its own fn tick: {e}",
                server.label()
            ))
        })
}

/// Reads byte `byte` of `uuid`'s current value on `server`, through the same
/// `server.value(uuid)` binding a script would call.
///
/// Going back through the engine rather than reaching into the database is
/// deliberate: `value` already knows that a service built by a Rust profile
/// registrar exists only in the `GattDatabase` and never reaches the script's
/// own service list, so a monitor over `add_ras`/`add_pacs` state works for
/// free instead of quietly finding nothing.
fn sample(
    context: &NativeCallContext,
    server: &ScriptGattServer,
    uuid: Uuid,
    byte: usize,
) -> Result<i64, Box<EvalAltResult>> {
    let bytes: Blob = context
        .call_native_fn("value", (server.clone(), uuid))
        .map_err(|_| {
            runtime_error(format!(
                "assert_over: {} has no readable characteristic {uuid} \
                 — check the UUID, or that the device published it",
                server.label()
            ))
        })?;
    bytes.get(byte).map(|b| i64::from(*b)).ok_or_else(|| {
        runtime_error(format!(
            "assert_over: {} {uuid} is {} byte(s) long, so there is no byte {byte} to monitor",
            server.label(),
            bytes.len(),
        ))
    })
}

/// The whole assertion: advance, sample, compare, `seconds / 0.1` times.
///
/// Returns the *extreme* — the sample that came closest to violating `op`,
/// which is what the MCP tool reports on a PASS. A script can ignore it (it is
/// an expression statement) or assert on it, which is how you find out that a
/// monitor is passing by one.
fn assert_over(
    context: &NativeCallContext,
    server: &ScriptGattServer,
    uuid: Uuid,
    op: &str,
    threshold: i64,
    seconds: f64,
    byte: usize,
) -> Result<i64, Box<EvalAltResult>> {
    if compare(0, op, 0).is_none() {
        return Err(runtime_error(format!(
            "assert_over: unknown operator {op:?} — use one of {}",
            OPERATORS.join(", ")
        )));
    }
    if !(seconds.is_finite() && seconds >= 0.0) {
        return Err(runtime_error(format!(
            "assert_over: {seconds} is not a monitoring window in seconds"
        )));
    }
    let steps = steps_for(seconds);
    let mut extreme: Option<i64> = None;
    for step in 0..steps {
        let t = step as f64 * STEP_SECONDS;
        advance(context.engine(), server, t)?;
        let actual = sample(context, server, uuid, byte)?;
        extreme = Some(extreme.map_or(actual, |e| extreme_for(op, e, actual)));
        // `compare` cannot be None here — the operator was checked above.
        if compare(actual, op, threshold) == Some(false) {
            return Err(runtime_error(format!(
                "assert_over failed: {} {uuid} byte {byte} = {actual} violated \
                 {op} {threshold} at t={t:.1}s (sample {} of {steps})",
                server.label(),
                step + 1,
            )));
        }
    }
    // A passing monitor is silent, like `assert` — the extreme is the
    // expression's value, not output, so nothing is printed on success.
    Ok(extreme.unwrap_or_default())
}

/// Registers `assert_over` (three arities) and `server.advance(t)`.
///
/// This is a *scene* extension rather than part of [`crate::scripting::new_engine`]:
/// sampling goes through `server.value(uuid)`, which the scene surface
/// registers, so an engine without it could offer `assert_over` only to fail
/// on the first sample.
pub fn register(engine: &mut Engine) {
    // The full form. `seconds` is the window, `byte` the index into the
    // characteristic's value.
    engine.register_fn(
        "assert_over",
        |context: NativeCallContext,
         server: ScriptGattServer,
         uuid: Uuid,
         op: &str,
         threshold: i64,
         seconds: f64,
         byte: i64|
         -> Result<i64, Box<EvalAltResult>> {
            let byte = usize::try_from(byte).map_err(|_| {
                runtime_error(format!("assert_over: byte index {byte} is negative"))
            })?;
            assert_over(&context, &server, uuid, op, threshold, seconds, byte)
        },
    );
    // Window named, byte defaulted.
    engine.register_fn(
        "assert_over",
        |context: NativeCallContext,
         server: ScriptGattServer,
         uuid: Uuid,
         op: &str,
         threshold: i64,
         seconds: f64|
         -> Result<i64, Box<EvalAltResult>> {
            assert_over(
                &context,
                &server,
                uuid,
                op,
                threshold,
                seconds,
                DEFAULT_BYTE,
            )
        },
    );
    // Rhai has one integer type and no coercion at the call site, so
    // `assert_over(hrm, uuid, "<", 200, 3)` — an author writing a whole
    // number of seconds — must resolve too, or it is a baffling "function not
    // found" for the most natural spelling.
    engine.register_fn(
        "assert_over",
        |context: NativeCallContext,
         server: ScriptGattServer,
         uuid: Uuid,
         op: &str,
         threshold: i64,
         seconds: i64|
         -> Result<i64, Box<EvalAltResult>> {
            assert_over(
                &context,
                &server,
                uuid,
                op,
                threshold,
                seconds as f64,
                DEFAULT_BYTE,
            )
        },
    );
    // The shortest form: the default window.
    engine.register_fn(
        "assert_over",
        |context: NativeCallContext,
         server: ScriptGattServer,
         uuid: Uuid,
         op: &str,
         threshold: i64|
         -> Result<i64, Box<EvalAltResult>> {
            assert_over(
                &context,
                &server,
                uuid,
                op,
                threshold,
                DEFAULT_SECONDS,
                DEFAULT_BYTE,
            )
        },
    );
    // The other half of "the device's own physics run under the assertion":
    // a scene script that composed itself out of `catalog::device` forwards
    // its own tick with `server.advance(t)`, so the loaded device keeps
    // moving once the test is over and it is being hosted for real.
    engine.register_fn(
        "advance",
        |context: NativeCallContext,
         server: ScriptGattServer,
         t: f64|
         -> Result<(), Box<EvalAltResult>> { advance(context.engine(), &server, t) },
    );
    engine.register_fn(
        "advance",
        |context: NativeCallContext,
         server: ScriptGattServer,
         t: i64|
         -> Result<(), Box<EvalAltResult>> { advance(context.engine(), &server, t as f64) },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compare_covers_every_advertised_operator() {
        // The table is the contract; an operator the schema advertises but
        // `compare` does not know is a monitor that fails with "unknown op".
        for op in OPERATORS {
            assert!(
                compare(1, op, 2).is_some(),
                "advertised operator {op} is not implemented"
            );
        }
        assert_eq!(compare(1, "<", 2), Some(true));
        assert_eq!(compare(2, "<", 2), Some(false));
        assert_eq!(compare(2, "<=", 2), Some(true));
        assert_eq!(compare(3, ">", 2), Some(true));
        assert_eq!(compare(2, ">=", 3), Some(false));
        assert_eq!(compare(2, "==", 2), Some(true));
        assert_eq!(compare(2, "!=", 2), Some(false));
        assert_eq!(compare(1, "=<", 2), None, "typos are not operators");
    }

    #[test]
    fn extreme_is_the_sample_closest_to_breaking_the_rule() {
        // Under "<", the worst sample is the largest; under ">", the smallest.
        assert_eq!(extreme_for("<", 70, 190), 190);
        assert_eq!(extreme_for("<=", 70, 190), 190);
        assert_eq!(extreme_for(">", 70, 190), 70);
        assert_eq!(extreme_for(">=", 70, 190), 70);
        assert_eq!(extreme_for("==", 70, 190), 190);
    }

    #[test]
    fn a_window_is_always_at_least_one_sample() {
        assert_eq!(steps_for(0.0), 1);
        assert_eq!(steps_for(0.05), 1);
        assert_eq!(steps_for(1.0), 10);
        assert_eq!(steps_for(2.0), 20);
        assert_eq!(steps_for(3.0), 30);
    }
}
