// Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
//
// The LE Audio sink device, as a Rhai script.
//
// The script itself lives in `catalog/le-audio-sink.rhai` — a real file at the
// repository root, not under any one surface's directory,
// not a template literal in here. It was 57 of this module's 66 lines, which
// meant the device that the Audio page runs, that the Broadcast page's sink
// mirrors, and that `tests/interop/lea_source.py` aims at, existed only as a
// JavaScript string. Nothing in Rust could see it, so a renamed binding broke
// it in a browser at runtime rather than at `cargo build`.
//
// As a file it is three things at once: served to this page, `include_str!`d
// into `src/devices/catalog.rs` so the MCP `example` tool and the Rust tests
// run the same bytes, and checked by CI through the `simble` CLI. That is the
// same arrangement `web/hrm/heart_rate.rhai` has always had.
//
// Fetched with top-level await so the export stays a plain string and callers
// are unchanged — `buildHeaders()` needs the text before `init()` has run, so
// going through the wasm `catalog_script` binding was not an option here.

export const LE_AUDIO_SINK_SCRIPT = await fetch(
  new URL("../../catalog/le-audio-sink.rhai", import.meta.url),
).then((response) => {
  if (!response.ok) {
    throw new Error(`le-audio-sink.rhai: ${response.status} ${response.statusText}`);
  }
  return response.text();
});
