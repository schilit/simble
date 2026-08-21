# Simble browser demos

Simble compiled to WebAssembly, running in the page, connected to a local
`netsimd` over the browser's native WebSocket. Two demos:

- **`scanner/`** — joins the netsim scene as `web-scanner` and renders live
  decoded advertisements (Simble's real HCI bring-up and GAP parsing, in wasm).
- **`hrm/`** — a running Simble whose peripheral is defined by an editable
  Rhai script; edit the script, press Run, and the device on the air changes.

The hosted pages are at <https://schilit.github.io/simble/scanner/> and
<https://schilit.github.io/simble/hrm/> — the cloud hosts the page, netsim
runs on **your** machine. Best demo: open both pages side by side against one
netsimd, rename the device in the HRM's script, press Run, and watch the
scanner pick up the new name.

## Prerequisite (both hosted and local): a local netsimd

The WebSocket endpoint needs the canary-channel emulator's netsimd
(37.2.5+ — the stable channel's daemon doesn't start the WebSocket frontend):

```bash
# One-time: install the canary-channel emulator package
~/Library/Android/sdk/cmdline-tools/latest/bin/sdkmanager --channel=3 emulator

# Start netsim with the WebSocket endpoint on
# (--test-beacons puts two beacons on the air for the scanner immediately)
~/Library/Android/sdk/emulator/netsimd --logtostderr --no-shutdown --ws-port 7681 --test-beacons
```

Browsers allow `ws://localhost` connections even from `https://` pages
(secure-context localhost exemption), which is what makes the GitHub-Pages
pages work against your local daemon.

## Building and serving locally

The wasm artifacts (`web/pkg/`) are not committed; build them with:

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.127 --locked   # must match the wasm-bindgen crate

RUSTFLAGS='--cfg getrandom_backend="wasm_js"' \
    cargo build --release --target wasm32-unknown-unknown --lib
wasm-bindgen --target web --no-typescript --out-dir web/pkg \
    target/wasm32-unknown-unknown/release/simble.wasm
```

(The `RUSTFLAGS` cfg selects getrandom's browser backend, needed by a
transitive dependency of the scripting engine.)

Then serve this directory — ES modules don't load from `file://`:

```bash
cd web && python3 -m http.server 8000
```

and open <http://localhost:8000/scanner/> and <http://localhost:8000/hrm/>.

## Deployment

`.github/workflows/pages.yml` runs the same build on every push to `main` and
publishes `web/` via GitHub Pages (Settings → Pages → Source: GitHub Actions).
