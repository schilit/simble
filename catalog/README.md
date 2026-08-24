# The device catalog

Every runnable Bluetooth device SimBLE ships, as Rhai scripts, in one place.

This directory is deliberately at the repo root rather than under `src/` or
`web/`, because **three surfaces read the same files**. A device fixed here is
fixed everywhere at once; a device that only worked in one place was the
problem this layout removed.

| Directory | What it holds |
|---|---|
| `devices/` | 28 device scripts — the peripherals and centrals a scene can host |
| `scenes/` | 5 scene files (JSON), naming devices by their catalog name |
| `tests/` | 5 test scripts, whose filename declares the expected verdict |

## Who reads these files

| Surface | How it gets them |
|---|---|
| **Rust / CLI / tests** | `include_str!` at build time, via `src/devices/catalog.rs` |
| **MCP** (agents) | the `example` tool serves them by name |
| **Web pages** | the wasm `catalog_script(name)` binding, or a plain `fetch` of the `.rhai` file |
| **Rhai scripts** | `catalog::device("hrm")` — the loaded device *is* the peripheral |

Because they are compiled in with `include_str!`, a syntax error in any file
here is a **build** failure, not a runtime surprise. `tests/catalog_test.rs`
goes further: it builds and ticks every entry in `devices/`, and rejects one
that registers no services.

## Adding a device

Write the `.rhai` file, then add an entry to `EXAMPLES` (or `CENTRAL_EXAMPLES`)
in `src/devices/catalog.rs` with its name, a one-line summary, and an
`include_str!` of the file. The name is the identifier every surface uses.

A device script builds a GATT server and, optionally, defines `fn tick(server,
t)` to change its values over time:

```rhai
let server = android::BluetoothGattServer("Thermometer");
// ... add services and characteristics ...
fn tick(server, t) { /* update a value */ }
```

The bindings are Android-shaped on purpose — `android::BluetoothGattServer`,
`android::BluetoothGatt` — so a script reads like the API an app developer
already knows. See [`docs/scripting-profile-apis.md`](../docs/scripting-profile-apis.md).

## The `tests/` naming contract

A filename ending `.pass.rhai` **must** pass; `.fail.rhai` **must** fail. That
is not a convention — a Rust test walks this directory and enforces it, so a
`.fail` script that starts passing breaks the build. The `.fail` cases are the
interesting ones: `notify-required.fail.rhai` asserts that a mistake is still
caught, and `monitor.fail.rhai` is built around a trap where the first samples
pass and only a *temporal* assertion (`assert_over`) catches the violation.
