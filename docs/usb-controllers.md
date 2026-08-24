# Running SimBLE on real hardware

> **Reference, kept current.** Verified 2026-08-24 against the dongles it
> describes. If something here disagrees with `cargo run --example usb_list`,
> the code is right and this file is a bug.

SimBLE's simulated controller is deliberately permissive, and real silicon is
not. That difference is the whole reason to plug in a dongle: it is the only
way to find a class of bug that no simulated test can reach — a controller
that silently discards commands you sent too fast, or rejects a parameter your
simulator waved through.

This page is about choosing a controller, setting it up, and knowing what each
one can actually prove.

## Which controller do you need?

Work down until a row covers what you want to test. Each tier includes
everything above it.

| You want to test | You need | Why |
|---|---|---|
| Protocol logic, CI, anything deterministic | **No hardware.** The built-in controller | Real RF is non-deterministic; that is wrong for CI. |
| Against a real phone or another stack | **Any Bluetooth 4.0 dongle** — e.g. a CSR8510 (`0a12:0001`), a few pounds | Real RF, real timing, a real peer. |
| Extended advertising, periodic advertising, **LE Audio broadcast (BIG)**, 2M/Coded PHY | **A Bluetooth 5.x controller** — an nRF52840 dongle, flashed (below) | A 4.0 controller answers these with *Unknown HCI Command*. There is no software workaround. |
| **Channel Sounding** (distance ranging) | **Bluetooth 6.0 silicon**, and two of them | Not available as a USB dongle today — see [Channel Sounding](#channel-sounding-hardware). |

A note worth internalising before spending anything: **BIG and Channel
Sounding cannot be checked against software.** Bumble implements no BIG, and
Rootcanal answers `LE_Create_BIG` with `Unknown HCI Command`. Neither
implements the Ranging Service. If you are working on broadcast audio or
ranging, hardware is not a nice-to-have — it is the only oracle that exists.

## Plugging in more than one

Two dongles of the same model share a `vid:pid` and usually publish no serial
number, so `0a12:0001` cannot name either of them. SimBLE refuses an ambiguous
selector rather than guessing, and offers four ways to be specific:

| Form | Example | Precise | Survives a re-plug |
|---|---|---|---|
| index | `#1` | within a session | no |
| vid:pid | `0a12:0001` | only when unique | yes |
| bus/address | `02/4` | yes | no |
| **bus.port** | `02.3.4` | **yes** | **yes** |

`bus.port` names the physical socket. Use it for anything scripted; the others
are for typing once.

```sh
cargo run --example usb_list    # every dongle, and every name it answers to
```

## Reaching a dongle from a browser page

A web page cannot open a dongle: WebUSB refuses Bluetooth-class devices by
design, so the browser can never claim a radio. Run the bridge instead — one
process serves every dongle on one port, the way netsim serves every device:

```sh
simble --usb --ws 32323
curl http://127.0.0.1:32323/devices        # what is plugged in, as JSON
# ws://127.0.0.1:32323/?device=02.3.2      # open that one
```

Each client gets its own connection, so several radios can be live at once.

## Turning an nRF52840 dongle into a controller

An nRF52840 dongle ships running a bootloader, **not** Bluetooth firmware — it
is a blank board, and will not appear as a controller until you flash it. The
firmware to use is Zephyr's `hci_usb` sample.

You need a Zephyr workspace, the ARM toolchain, and `nrfutil`. Then:

```sh
west build -p -b nrf52840dongle/nrf52840 zephyr/samples/bluetooth/hci_usb \
  -- -DEXTRA_CONF_FILE=extra.conf
nrfutil nrf5sdk-tools pkg generate --hw-version 52 --sd-req 0x00 \
  --application build/zephyr/zephyr.hex --application-version 1 fw.zip
nrfutil nrf5sdk-tools dfu usb-serial -pkg fw.zip -p /dev/cu.usbmodemXXXX
```

**The stock sample is not what you want.** Its default configuration leaves
out extended advertising and the entire ISO stack, so a 5.4 chip comes up
without the features you bought it for. Add this overlay as `extra.conf`:

```
CONFIG_BT_EXT_ADV=y
CONFIG_BT_CTLR_ADV_EXT=y
CONFIG_BT_PER_ADV=y
CONFIG_BT_PER_ADV_SYNC=y
CONFIG_BT_CTLR_ADV_PERIODIC=y
CONFIG_BT_CTLR_SYNC_PERIODIC=y
CONFIG_BT_ISO_BROADCASTER=y
CONFIG_BT_ISO_SYNC_RECEIVER=y
CONFIG_BT_CTLR_ADV_ISO=y
CONFIG_BT_CTLR_SYNC_ISO=y
```

Flash usage roughly doubles (121 KB → 216 KB), which is a quick way to confirm
the overlay was picked up. To be certain, ask the controller: read
`Read_Local_Supported_Commands` and check the bits, rather than trusting the
build.

### Getting into DFU mode

Press **RESET** — the small button mounted *sideways* on the edge, pressed
toward the board. It is easy to confuse with the round user button on the top
face, which does nothing for DFU. A red LED **fading in and out** means it is
ready.

If the dongle already has working firmware, a plain reset will boot straight
into it. Unplug, hold RESET, plug in, then release.

### Two gotchas

**The address is not what you set.** An nRF52840 has no public address in ROM;
Zephyr uses a random static address, and it reads back as
`00:00:00:00:00:00`. SimBLE advertises with `own_address_type = public`, so a
peer sees the controller's address rather than one you configured. Read the
address from the controller rather than assuming.

**Bus power.** Several radios behind one unpowered hub is a real limit. A
dongle that drops out intermittently is more often power than software.

## Channel Sounding hardware

Channel Sounding is the Bluetooth 6.0 distance-ranging feature, and it needs
new silicon. **There is essentially no consumer USB dongle that supports it**
— the cheap market still ships 5.1–5.3 Realtek parts (RTL8761B, RTL8852BE)
under many names, and "6.0" on a product listing usually describes a host
stack rather than Channel Sounding in the controller. Confirm the specific
feature before ordering.

What exists today is development kits, and as of August 2026 they are all the
same Nordic nRF54L15 silicon — so the choice is form factor, not capability:

| Board | Notable |
|---|---|
| Nordic **nRF54L15-DK** | The reference kit. |
| **makerdiary nRF54L15 Connect Kit** ([product](https://makerdiary.com/products/nrf54l15-connectkit), [wiki](https://wiki.makerdiary.com/nrf54l15-connectkit/)) | Compact, castellated pins, onboard debugger. |
| Nordic **nRF54L15 Tag** | Coin-cell, and **two antennas** — the only listed board that can exercise multiple antenna paths. |

Three things to know before buying:

1. **You need two.** Channel Sounding measures distance *between* two devices.
   One kit cannot range against anything.
2. **The nRF54L15 has no USB device peripheral.** On these boards the USB-C
   port goes to a separate interface MCU providing a USB-UART bridge, so HCI
   arrives on a **virtual serial port**. SimBLE's `UsbTransport` cannot talk to
   it; an HCI-over-UART transport is a prerequisite, not an afterthought.
3. Silicon Labs (EFR32BG26) and TI (CC2340) have announced Channel Sounding
   parts. Neither surfaced as a shipping kit in an August 2026 search.

## Also worth knowing

**Seeed XIAO nRF52840** (Arrow/DigiKey `102010448`,
[product page](https://www.seeedstudio.com/Seeed-XIAO-BLE-nRF52840-p-5201.html))
is the same nRF52840 silicon as the dongles above, in a XIAO board with USB-C.
The same `hci_usb` firmware applies and it lands in the same tier. Useful as an
extra 5.x radio; it adds no capability you would not already have.

**Homebrew's `nrfutil` cask fails macOS Gatekeeper** and was flagged for
removal. If it disappears, Nordic distributes the binary directly.
