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

**No hardware at all** is the right answer for protocol logic and for CI: the
built-in simulated controller is deterministic, and real RF is not. Everything
below is for when you need a real peer, or a feature the simulator cannot
prove by itself.

Work down until a row covers what you want to test; each tier includes
everything above it. **Tested** means someone has run SimBLE against that
board and the hardware tests pass — anything else is an expectation, however
reasonable.

| Silicon | Boards you can buy | BT | Adds | Tested |
|---|---|---|---|---|
| **CSR8510 A10** | **Ezurio (Laird) BT820** — `BT820-ND` ([DigiKey](https://www.digikey.com/en/products/detail/laird-connectivity-inc/BT820/4423863), [Newark](https://www.newark.com/laird-technologies/bt820/usb-dongle-bluetooth-bt800-v4/dp/08X8202), [Ezurio](https://www.ezurio.com/part/bt820)) | 4.0 | Real RF, real timing, a real peer: advertising, scanning, connections, GATT | ✅ two of them link and discover over the air (`tests/usb_hardware_test.rs`) |
| **Realtek RTL8761B / RTL8852BE** | Most "5.x" dongles sold today, many brand names | 5.1–5.3 | Real RF; extended advertising varies by firmware | ⚠️ untested |
| **Nordic nRF52840** | **Dongle** — `NRF52840-DONGLE` / PCA10059 ([Arrow](https://www.arrow.com/en/products/nrf52840-dongle/nordic-semiconductor), [DigiKey](https://www.digikey.com/en/products/detail/nordic-semiconductor-asa/NRF52840-DONGLE/9491124), [Mouser](https://www.mouser.com/ProductDetail/Nordic-Semiconductor/nRF52840-Dongle?qs=gTYE2QTfZfTbdrOaMHWEZg%3D%3D))<br>**XIAO** — Seeed `102010448` ([DigiKey](https://www.digikey.com/en/products/detail/seeed-technology-co-ltd/102010448/16652893), [Seeed](https://www.seeedstudio.com/Seeed-XIAO-BLE-nRF52840-p-5201.html))<br>**MDK USB Dongle** — makerdiary ([product](https://makerdiary.com/products/nrf52840-mdk-usb-dongle), [wiki](https://wiki.makerdiary.com/nrf52840-mdk-usb-dongle/)), USB-A stick with a UF2 bootloader | 5.4 | Extended advertising, periodic advertising, 2M + Coded PHY, LE Extended Create Connection, **BIG at the HCI level only — see the caveat below** | ✅ **dongle** tested, flashed with Zephyr `hci_uart` (H4 over CDC-ACM; `hci_usb` cannot carry ISO)<br>❌ **XIAO** — a bare MCU, not a dongle: no HCI firmware ships with it. Flash Zephyr `hci_uart` by UF2 to use it |
| **Nordic nRF54L15** | **DK** ([Arrow](https://www.arrow.com/en/products/nrf54l15-dk/nordic-semiconductor.html), [Newark](https://www.newark.com/new-products/embedded-computers-education-maker-boards/nordic-nrf54l15-dk))<br>**makerdiary Connect Kit** ([direct](https://makerdiary.com/products/nrf54l15-connectkit))<br>**Nordic Tag** — two antennas | 6.0 | **Channel Sounding** (distance ranging) | ❌ blocked **on SimBLE, not the board** — these expose HCI on a virtual serial port and SimBLE has no UART transport yet |

No nRF board is plug-in-and-go: all need firmware flashed before they are
Bluetooth controllers at all.

**If you are buying an nRF52840 to flash, prefer makerdiary's MDK USB
Dongle.** It has an upstream Zephyr board definition
(`nrf52840_mdk_usb_dongle`), so `hci_usb` builds for it directly — and it
ships a **UF2 bootloader**, meaning you flash it by copying a file to a
mass-storage device. Nordic's own dongle needs `nrfutil`, whose pip package
has no Apple Silicon wheels (`pc_ble_driver_py`) and whose Homebrew cask fails
macOS Gatekeeper, so the flashing route is markedly more awkward on a Mac.

### The nRF52840 and LE Audio: what it does, and what nobody supports

Zephyr's open-source controller implements BIG for the nRF52 series, and it
works as far as HCI can show: on a flashed dongle we created a BIG, opened
ISO data paths on two BIS, and streamed 240 000 SDUs per BIS for forty
minutes without an error, with the whole broadcast — BASE, BIG parameters,
both tones in their right channels — decoded by Bumble as a foreign
receiver.

**Nordic does not support LE Audio on the nRF52840.** Their position is that
LE Audio is an nRF5340 feature: the LE Audio controller stack and the LC3
codec are built for that part, and their Auracast reference designs
(`nRF Auraconfig`, the broadcast audio samples) target the **nRF5340 Audio
DK**. See [Auracast feature on
nRF52840?](https://devzone.nordicsemi.com/f/nordic-q-a/96478/auracast-feature-on-nrf52840).

So: "the controller accepts the commands and the bytes are right" is
established, and "a commercial receiver joins the broadcast off the air" is
**not**. A Pixel 9 with Pixel Buds Pro 2 listed our broadcast and synced to
it but rendered silence, and we could not tell whether the fault lay in the
phone, the buds, or an unsupported radio path. If Auracast is the goal
rather than a bonus, buy the part the vendor supports.

One more thing to know before spending anything: **Channel Sounding cannot be
checked against software at all, and BIG only against netsim.** For BIG the
situation is narrower than it looks: upstream rootcanal *implements* it
(`rust/src/llcp/iso.rs`) but ships the release binary with those commands
left out of its supported-commands table, so it answers `LE_Create_BIG` with
`Unknown HCI Command` — while the rootcanal built into netsim has them
enabled, and our broadcast path is verified against Bumble's `auracast` app
through netsim in both directions. Bumble itself models no BIG, and nothing
in software implements the Ranging Service. So: broadcast audio has exactly
one software oracle (netsim) and ranging has none — for ranging, hardware is
not a nice-to-have but the only oracle that exists.

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
is a blank board, and will not appear as a controller until you flash it.

**Which sample depends on whether you need ISO.** `hci_usb` is the obvious
choice and is fine for LE and Classic. It cannot carry HCI ISO data at all:
Zephyr's USB Bluetooth class hardwires bulk OUT to ACL and its isochronous
endpoints are zero-bandwidth SCO placeholders (upstream
[zephyr#44013](https://github.com/zephyrproject-rtos/zephyr/issues/44013)),
and Nordic's own guidance is that HCI-over-USB is unstable under sustained
traffic. For anything isochronous — LE Audio, Auracast — use **`hci_uart`**
instead: it presents as a CDC-ACM serial port carrying H4, where the packet
type is in-band and ISO rides the same stream as everything else. simble
reaches it with `serial:/dev/cu.usbmodemXXXX`.

You need a Zephyr workspace, the ARM toolchain, and `nrfutil`. Then:

```sh
# hci_usb for LE and Classic; swap in hci_uart if you need ISO (see above).
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
# Without this the periodic-advertising data buffer is the stock 31 bytes
# and a real Auracast BASE (~46 bytes) is refused with Command Disallowed —
# measured on the flashed dongle: 31 accepted, 32 refused. LE_Create_BIG
# itself is present either way; the buffer is the blocker.
CONFIG_BT_CTLR_ADV_DATA_LEN_MAX=191
# Two BIS per BIG (stereo): the controller default is ONE stream, and
# Num_BIS=2 answers Invalid Parameters.
CONFIG_BT_ISO_MAX_CHAN=4
CONFIG_BT_CTLR_ADV_ISO_STREAM_MAX=2
CONFIG_BT_CTLR_ADV_ISO_STREAM_COUNT=4
# The quiet killer: the default ISO TX buffer is NINE bytes, which caps
# every BIS PDU below any usable size — and it does not fail as a length
# error; it surfaces as LE_Create_BIG refusing every parameter set with
# Invalid Parameters, because the scheduler math is downstream of the cap.
CONFIG_BT_CTLR_ISO_TX_BUFFERS=8
CONFIG_BT_CTLR_ISO_TX_BUFFER_SIZE=255
CONFIG_BT_ISO_TX_BUF_COUNT=8
# One ISO-AL source serves one BIS; the second data path answers Command
# Disallowed without more.
CONFIG_BT_CTLR_ISOAL_SOURCES=4
CONFIG_BT_CTLR_ISOAL_SINKS=4

# And one thing no overlay fixes: Zephyr's USB HCI class has **no
# host-to-controller ISO data path at all** — bulk OUT is hardwired to ACL
# and the descriptor's isochronous endpoints are zero-bandwidth SCO
# placeholders. SimBLE's answer is an opt-in framing deviation patched into
# the class (CONFIG_USBD_BT_HCI_H4_BULK): every bulk packet carries its H4
# type byte, the firmware says "H4-bulk" in its USB product string, and
# `UsbTransport` switches dialect on that string. A standard dongle never
# sees a prefixed byte. ADV_RESERVE_MAX must also be off (a local Kconfig
# default flip — the fragment assignment is ignored) or worst-case
# advertising reservations eat the whole ISO interval.
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
new silicon. **No cheap consumer dongle has it** — that market still ships
5.1–5.3 Realtek parts (RTL8761B, RTL8852BE) under many names, and "6.0" on a
product listing usually describes a host stack rather than Channel Sounding in
the controller. Confirm the specific feature before ordering.

What you can buy are development kits and one test device. Be precise about
what "USB" means for each: they all plug into a USB-C port, but that port
leads to an interface MCU offering a **virtual serial port**, not to a USB
Bluetooth-class controller. The distinction decides whether `UsbTransport` can
talk to it (it cannot) or whether you need an HCI-over-UART transport (you
do).

What exists today is development kits, and as of August 2026 they are all the
same Nordic nRF54L15 silicon — so the choice is form factor, not capability:

| Board | Notable |
|---|---|
| Nordic **nRF54L15-DK** ([Arrow](https://www.arrow.com/en/products/nrf54l15-dk/nordic-semiconductor.html), [Newark](https://www.newark.com/new-products/embedded-computers-education-maker-boards/nordic-nrf54l15-dk)) | The reference kit; SEGGER J-Link on board, and it can emulate the nRF54L10 and nRF54L05 too. |
| **makerdiary nRF54L15 Connect Kit** ([product](https://makerdiary.com/products/nrf54l15-connectkit), [wiki](https://wiki.makerdiary.com/nrf54l15-connectkit/)) | Compact, castellated pins, onboard debugger, and a [ready-made Channel Sounding sample](https://wiki.makerdiary.com/nrf54l15-connectkit/guides/ncs/samples/bluetooth/channel_sounding/). |
| Nordic **nRF54L15 Tag** | Coin-cell, and **two antennas** — the only listed board that can exercise multiple antenna paths. |

makerdiary's [Channel Sounding sample](https://wiki.makerdiary.com/nrf54l15-connectkit/guides/ncs/samples/bluetooth/channel_sounding/)
is the most useful starting point, and worth reading before you order because
it shows exactly what the hardware gives you. It runs one board as an
**Initiator with Ranging Requestor** and the other as a **Reflector with
Ranging Responder** — the Ranging Service roles — and logs a distance per
antenna path by three independent methods:

```
Distance estimates on antenna path 0: ifft: 1.610213, phase_slope: 2.249911, rtt: 13.750394
```

Those are the same roles and the same measurement methods SimBLE implements in
`profiles/ras.rs` and `cs/ranging.rs`. Three estimates from one procedure is
also a useful sanity check in itself: they should broadly agree, and the
spread tells you how much to trust any single one.

Three things to know before buying:

1. **You need two.** Channel Sounding measures distance *between* two devices.
   One kit cannot range against anything.
2. **SimBLE needs a UART transport before it can use one.** The boards are
   fine — the nRF54L15 has no USB device peripheral, so the USB-C port goes to
   an interface MCU that bridges to a **virtual serial port**, and HCI is right
   there on it. SimBLE simply has no serial transport: `UsbTransport` speaks to
   USB Bluetooth-class devices only. The framing (H4) is already implemented —
   `RootcanalTransport` does it over TCP — so this is serial plumbing rather
   than protocol work, but budget it as part of the purchase.
3. Silicon Labs (EFR32BG26) and TI (CC2340) have announced Channel Sounding
   parts. Neither surfaced as a shipping kit in an August 2026 search.

## Also worth knowing

**Seeed XIAO nRF52840** ([product page](https://www.seeedstudio.com/Seeed-XIAO-BLE-nRF52840-p-5201.html))
is a development board rather than a dongle — castellated pins, a UF2
bootloader, and no factory HCI firmware. Same silicon as the nRF52840 dongle,
so the same `hci_usb` build applies.

**Homebrew's `nrfutil` cask fails macOS Gatekeeper** and was flagged for
removal. If it disappears, Nordic distributes the binary directly.
