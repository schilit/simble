# Simble browser demos

Simble compiled to WebAssembly, running in the page — its real HCI bring-up,
GAP/GATT/L2CAP and scripting stack, in wasm. Each page picks a controller from
the bar at the top: the in-page **SimBLE** stack (nothing installed), **netsim**
(a local `netsimd` over the browser's native WebSocket, sharing a scene with the
Android emulator), or **Real radio** (a dongle or a phone through a local
`simble --usb` bridge).

There are ~19 pages under `web/`, sharing the common chrome and controller code
in `web/common/`. Among them:

- **`scanner/`** — renders live decoded advertisements off the chosen scene.
- **`hrm/`** — a peripheral defined by an editable Rhai script; edit, press Run,
  and the device on the air changes.
- **`testing/`** — asserts and throughput measurements (including phone-to-phone).
- **`playground/`, `explorer/`, `scene/`, `devices/`** — author and inspect
  devices and whole scenes; **`audio/`, `car/`, `hid/`, `ranging/`,
  `broadcast/`, `speaker/`, `lightbulb/`, `dual/`, `source/`, `emulator/`,
  `controllers/`, `shared/`** cover specific profiles and topologies.

The hosted pages are at <https://schilit.github.io/simble/> — the cloud hosts
the page, and netsim (if used) runs on **your** machine. A good first demo: open
`scanner/` and `hrm/` side by side against one netsimd, rename the device in the
HRM's script, press Run, and watch the scanner pick up the new name.

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

The wasm artifacts (`web/pkg/`) are not committed; build them for
`wasm32-unknown-unknown` — Rust's `arch-vendor-os` target name, where the two
`unknown`s just mean no vendor and no host OS, because the code runs in a
browser sandbox rather than on an operating system:

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
python3 web/serve.py 8000
```

and open <http://localhost:8000/scanner/> and <http://localhost:8000/hrm/>.

Use `serve.py` rather than `python3 -m http.server`: it sends `no-store`, so an
edited module actually reloads instead of Chrome silently re-running the
cached copy. It also serves `/catalog/` from the repository root, which the
pages expect.

## Deployment

`.github/workflows/pages.yml` runs the same build on every push to `main` and
publishes `web/` via GitHub Pages (Settings → Pages → Source: GitHub Actions).
