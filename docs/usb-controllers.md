# USB Bluetooth controllers

> **Living — must match the hardware and the code.** Verified 2026-08-24 against
> the dongles actually plugged into this machine. If a claim here disagrees with
> `examples/usb_list.rs` or `tests/usb_hardware_test.rs`, the code is right.

Real silicon is the only oracle for a whole class of bug. Every hardware
session so far has found something no simulated test could, because simble's
own controller is more permissive than any real one: command credits silently
dropping six of seven commands, `0xFF × 8` event masks rejected as invalid
parameters, `LE_Set_Host_Feature` answered with Command Status rather than
Command Complete, and a USB data-toggle that makes a re-opened dongle deaf.

That is why this file exists: **which controller you own decides which parts of
the stack can be checked at all.**

## What is on the bench now

| # | Part | Bluetooth | Notes |
|---|---|---|---|
| 0, 1 | Cambridge Silicon Radio **CSR8510 A10** (`0a12:0001`) | 4.0 | The cheap grey dongle. Two of them; identical VID:PID and **no serial numbers**, which is why `UsbSelector` exists. |
| 2, 3 | **nRF52840 dongle** (PCA10059), flashed with Zephyr `hci_usb` (`2fe3:000b`) | 5.4 | Extended + periodic advertising, **LE Create BIG / BIG Create Sync**, LE Extended Create Connection, 2M + Coded PHY. |

Run `cargo run --example usb_list` for the live list and every selector each
dongle answers to.

### Why the nRF52840s matter more than the count suggests

**Nothing else can check our BIG code.** Bumble implements no BIG at all, and
upstream rootcanal v1.12.0 answers `LE_Create_BIG` with `Unknown HCI Command` —
measured, not assumed. So `BigBroadcaster`, `BigReceiver`, BASE and BIGInfo had
only ever been tested against simble's own controller, which
`docs/test-strategy.md` calls the configuration that proves nothing. Two
BIG-capable radios is the first independent check that broadcast code has had.

The same goes for `LE Extended Create Connection` — top of `docs/gaps.md` §2's
list of commands worth modelling next, whose completion event (LE *Enhanced*
Connection Complete) `sim.rs` does not emit at all — and for `LE Set PHY`,
which `sim.rs` models with no `LL_PHY_REQ`/`LL_PHY_RSP` exchange.

### Flashing an nRF52840 dongle

The dongle ships running Nordic's Open Bootloader and is **not** a Bluetooth
controller until firmware is on it. Build Zephyr's `hci_usb`, package it, and
push it over DFU:

```sh
west build -p -b nrf52840dongle/nrf52840 zephyr/samples/bluetooth/hci_usb \
  -- -DEXTRA_CONF_FILE=extra.conf          # see below
nrfutil nrf5sdk-tools pkg generate --hw-version 52 --sd-req 0x00 \
  --application build/zephyr/zephyr.hex --application-version 1 fw.zip
nrfutil nrf5sdk-tools dfu usb-serial -pkg fw.zip -p /dev/cu.usbmodemXXXX
```

**The stock sample is not enough.** `hci_usb`'s default config leaves
extended advertising and the whole ISO stack out, so the interesting features
are absent even on 5.4 silicon. The overlay that turns them on:

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

Flash grows 121 KB → 216 KB, which is the quick way to confirm the overlay
took. Verify against the controller rather than the build: read
`Read_Local_Supported_Commands` and check the bits.

**To enter DFU mode**, press the **RESET** button — the small one mounted
*sideways* on the edge, pressed toward the board. It is easy to confuse with
the round user button (SW1) on the top face, which does nothing for DFU. A red
LED fading in and out means it is ready. If a valid application is already
flashed, unplug, hold RESET, plug in, release.

## Selecting one of several

Two dongles of one model share a VID:PID and publish no serial number, so
`0a12:0001` cannot name either. `UsbSelector` accepts four forms:

| Form | Example | Precise | Survives a re-plug |
|---|---|---|---|
| index | `#1` | within a session | no |
| vid:pid | `0a12:0001` | only when unique | yes |
| bus/address | `02/4` | yes | no |
| **bus.port** | `02.3.4` | **yes** | **yes** |

`bus.port` names the *physical socket* and is the form to use in anything
scripted. A `vid:pid` matching more than one device is an **error** listing the
candidates — silently taking the first is the bug this replaced.

## Serving dongles to a browser

A page cannot open a dongle: WebUSB refuses Bluetooth-class devices by design,
so the browser can never claim the radio. The bridge does it instead — one
process, one port, every dongle, mirroring how netsim serves devices:

```sh
simble --usb --ws 32323
curl http://127.0.0.1:32323/devices        # what is available
# ws://127.0.0.1:32323/?device=02.3.2      # open that one
```

Each client gets its own thread, so several radios can be live at once.

## Buying more

### Seeed Studio XIAO nRF52840 — Arrow/DigiKey part `102010448`

Another nRF52840, in the XIAO form factor with USB-C. Same silicon as the
dongles above, so the same `hci_usb` firmware applies and it lands in the same
capability tier: **Bluetooth 5.x, BIG-capable, no Channel Sounding.** Worth it
only if you want *more* 5.x radios — a third and fourth BIG endpoint, or a
spare. It buys no new protocol coverage over what is already on the bench.

Note it is a development board rather than a dongle: castellated pins, a
different bootloader story (UF2), and no factory HCI firmware.

- [Seeed product page](https://www.seeedstudio.com/Seeed-XIAO-BLE-nRF52840-p-5201.html)
- [DigiKey 102010448](https://www.digikey.com/en/products/detail/seeed-technology-co-ltd/102010448/16652893)

### makerdiary nRF54L15 Connect Kit — the Bluetooth 6.0 option

The nRF54L15 is **Bluetooth LE 6.0** silicon: 128 MHz Cortex-M33, 1.5 MB NVM,
and — the reason to care — **Channel Sounding**.

That matters here more than anywhere else, because **Channel Sounding is
simble's flagship feature and has no foreign oracle of any kind.**
`docs/test-strategy.md` records that neither Bumble nor Zephyr implements RAS,
and the four invented RAS UUIDs were caught only by reading the SIG registry —
no test could have found them. `profiles/ras.rs`, `cs/*`,
`device/channel_sounding.rs` and `ranging_scene.rs` are ~3,200 lines checked
against nothing but themselves.

**The catch, and it is a real one: the nRF54L15 has no USB device peripheral.**
The board's USB-C port goes to a separate nRF52820 interface MCU providing a
USB-UART bridge and CMSIS-DAP debugging. So HCI arrives over a **virtual serial
port**, not as a USB Bluetooth-class device.

`UsbTransport` cannot talk to it. Using one means **writing an HCI-over-UART
transport** (Core Vol 4 Part A — H4 framing over a serial port, which is the
same framing `RootcanalTransport` already does over TCP, so the framing is not
the work; the serial plumbing is). That is a prerequisite, not an afterthought.

- [makerdiary product page](https://makerdiary.com/products/nrf54l15-connectkit)
- [Documentation wiki](https://wiki.makerdiary.com/nrf54l15-connectkit/)
- [GitHub](https://github.com/makerdiary/nrf54l15-connectkit)

### Recommendation

Buy the **nRF54L15 Connect Kit** if the goal is checking Channel Sounding
against real silicon — it is the only listed part that covers ground nothing
else can, and it needs two of them for a ranging pair. Budget the UART
transport as part of the cost.

The **XIAO nRF52840** is a spare, not a capability.

Neither is a plain USB dongle you plug in and use; both are development boards
needing firmware. There are still essentially no consumer USB dongles
advertising Bluetooth 6.0 — the cheap market ships 5.1–5.3 Realtek parts
(RTL8761B, RTL8852BE) under many names, and "6.0 certified" on a listing
usually describes a host stack rather than Channel Sounding in the controller.
Confirm the specific feature before ordering.
