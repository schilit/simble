# examples/

> **Reference, kept current.** If an example listed here no longer builds or
> runs as described, this file is a bug.

Seventeen programs that are really **two different things**, and it matters
which one you are looking at: four are self-contained demos you can run right
now, and seven are the simble half of an interop test — a binary that only
makes sense when a Python script on the other side is driving it. The
remaining six need a controller or a radio that is not in this repo.

## Self-contained — `cargo run --example NAME`, nothing else needed

| Example | What it shows |
|---|---|
| `heart_rate_monitor` | The smallest useful device: a GATT server notifying a value. |
| `ble_keyboard` | HOGP — a report map and input reports. |
| `channel_sounding` | Bluetooth 6.0 distance ranging, both ends in one process. |
| `in_process_scene` | Two devices meeting over the simulated controller, no external anything. |

## Interop harnesses — driven by `tests/interop/*.py`

These connect to a controller and then wait to be driven. Run them **through
their script**, not directly — on their own they will sit there.

| Example | Driven by |
|---|---|
| `scripted_central` | `gatt_client.py` |
| `a2dp_sink` | `a2dp_peer.py` |
| `avrcp_remote` | `avrcp_peer.py` |
| `classic_initiator` | `classic_peer.py` |
| `hfp_hf_pipe` | `hfp_oracle.py` |
| `auracast_source`, `auracast_sink` | `auracast_*.py` |

See [`tests/interop/README.md`](../tests/interop/README.md) for how to run
them, which need a live `netsimd`, and which run against Bumble alone.

## Need external hardware or a running controller

| Example | Needs |
|---|---|
| `netsim_smoke`, `netsim_two_devices` | a running `netsimd` |
| `classic_discoverable` | `netsimd` plus an Android emulator to page it |
| `usb_hrm` | a real USB Bluetooth dongle |
| `a2dp_source` | a **dual-mode** dongle and a real Bluetooth speaker in pairing mode |
| `usb_list` | nothing — it says what dongles are plugged in, and names each |

`a2dp_source` is the only example whose peer is consumer kit rather than
software: it inquires, pairs, reads the speaker's Audio Sink record for the
AVDTP PSM, negotiates SBC against what the speaker actually offers, and
streams a short melody. It reports which rung of that ladder it reached, and
prints the peer's SDP and AVDTP capability bytes whether or not it got past
them — a stall with the speaker's real bytes in hand is the useful outcome.
A2DP is Classic, so an LE-only controller (a Zephyr `hci_usb` nRF52840)
cannot run it at all; `cargo test --test a2dp_hardware_test -- --nocapture`
says whether this machine has a dongle that can, and why not when it cannot.

With two dongles of the same model plugged in, a `vid:pid` names **both** and
is refused rather than guessed at. `usb_list` prints every selector each one
answers to: `#0` (index), `0a12:0001` (vid:pid), `02/4` (bus/address), and
`02.3.4` (the socket it is in — the only form that survives a re-plug).
`tests/usb_hardware_test.rs` takes two of them, as `SIMBLE_USB_A` and
`SIMBLE_USB_B`, and runs simble against itself over real RF.

## Choosing a controller

Most examples take `$SIMBLE_HCI` to pick their transport (unset means netsim).
That indirection lives in `LiveTransport::open_from_env`, so an example does
not hard-code a URL and the same binary can be driven by netsim, by a
Bumble-hosted controller, or by whatever comes next.

Only four examples are declared as `[[example]]` in `Cargo.toml` (those needing
explicit `required-features` or a non-default name); the rest cargo discovers
automatically.
