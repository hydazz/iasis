# IasisOS

IasisOS is the firmware on the board's RP2354. It controls port power, USB-C attachment, Apple DFU signalling, the button, LEDs and optional OLED. Restore data passes directly through the USB hub between the host and targets.

## Build and flash

From the repository root:

```sh
mise install
mise run firmware:build
mise run host:build
```

`mise install` provides Raspberry Pi's [picotool](https://github.com/raspberrypi/picotool). Use it to flash the ELF through the host USB connection:

```sh
target/release/iasis --serial <board-serial> bootloader
picotool info
picotool load -v -x target/thumbv8m.main-none-eabihf/release/iasis-firmware -t elf
```

`mise run firmware:build` converts the ELF to `IasisOS.uf2` beside it, the form each release carries; `picotool load` takes either.

Check that `picotool info` finds the intended board before loading. With multiple ROM devices attached, select it using picotool's bus/address options.

A board with blank flash enumerates as the ROM updater when an external host is connected: skip the `bootloader` command and load the ELF. For a failed application, connect an external host, hold BOOTSEL, press and release RESET, then release BOOTSEL. Both buttons are under the lid. Flashing replaces the application; an interrupted write can be recovered with the same sequence. SWD provides independent access.

Development USB IDs are `1209:0001`. Set `IASIS_USB_VID` and `IASIS_USB_PID` as hexadecimal build variables to override them, and use matching CLI `--vid`/`--pid` arguments.

## Port power

Ports start enabled and reserve power only while attached. Disabling a port removes Rp and discharges VBUS. PD-capable targets negotiate fixed supplies over USB PD revision 2.0.

A port whose voltage or negotiation fails switches its output off, discharges VBUS and attaches again. Three failures within 30 seconds, or a protection trip, hold the port off until the target is unplugged or the port is enabled again.

| Supply path     | Target offer                               | Budget                                                                                                                                                 |
| --------------- | ------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Host presence   | Fixed 5 V / 0 mA PDO                       | Iasis reserves 800 mA. The remaining current selects the shared limiter below.                                                                         |
| Charging supply | Fixed 5, 9, 15 or 20 V, up to 3 A per port | The brick contract covers input cable and board losses, logic power and a 35 W reserve when the CM5 carrier is powered. The ports share the remainder. |

With host USB alone, charged Apple targets use 5 V for attachment and PD signalling. The 0 mA offer specifies the charging allowance. The port ADC measures connector voltage only. Ordinary USB peripherals draw their operating current from the same 5 V supply. Each port has a nominal 0.51 A current limit. Component tolerance can put the trip point below 500 mA. All five also share a limiter whose upper trip threshold is shown below.

| Upstream allowance | Shared limiter threshold (max) |
| ------------------ | ------------------------------ |
| Default USB | Off |
| 1500 mA            | 550 mA                         |
| 3000 mA            | 1890 mA                        |

Default USB requires the charging input to power targets.

On the charging input, Iasis requests the highest-power fixed profile it supports, up to 20 V. The host USB port stays at 5 V.

Each port starts at 5 V. With a 20 V brick, one port can offer 5, 9, 15 and 20 V at different currents for the same share of power, up to 3 A. The target chooses its profile independently. With a 9 or 12 V input, the ports offer 5 and 9 V.

On brick power, Iasis reserves 5 V / 900 mA to start a port. If that allowance is unavailable, the port waits. A new device can also be offered spare charging capacity. Once a device requests the full current of a highest-power profile, Iasis can redistribute power among charging devices. Existing contracts stay funded until the devices step down. When power becomes available, Iasis updates the charging ports one at a time, so a device can take a higher offer without a replug. An active restore keeps its contract while the supply can cover it.

Changing the charging input can interrupt USB to all targets while the board stays online. Leave the supply connected or disconnected for the duration of a restore.

## Controls and status

`iasis status --watch` shows the board temperature near the port 1 and 2 output switches (TH1191, sensor A) and the shared core regulator (TH1192, sensor B). On macOS it can run during a restore. Use the raw ADC counts in `status --json` for calibration.

The [button and CLI controls](../../README.md#use) operate on ready ports. USB peripherals can use the data link without negotiating Power Delivery. Once a PD contract has settled, Iasis asks the target which Apple debug actions its port offers, and sends DFU or reboot only where the target lists them. DFU also needs the port to list DFU USB, which a Mac does on its [DFU port](https://support.apple.com/en-us/120694) alone: told to enter DFU from another port, some Macs restart into DFU where no host can reach them. Those ports still take reboot. A target that lists neither still charges and can be restored once it is in DFU by other means; a USB-C to Lightning cable does not carry Apple debug. `iasis status` prints each port's list. DFU supports Apple Silicon and T2; the button selects Apple Silicon. Commands use the attachment generation to reject requests for a device that has since disconnected.

A colour is what the port holds, a pulse is work under way or a result awaited, and brightness is the selection.

| Port light      | Meaning                                                      |
| --------------- | ------------------------------------------------------------ |
| Off             | Empty or disabled target port                                |
| Green           | Device with a PD contract, or restore complete               |
| White           | Powered device without a PD contract; dim while one attaches |
| Blue, steady    | A host sees the target in DFU or recovery                    |
| Blue, pulsing   | DFU sent; no host has seen the result                        |
| Amber, pulsing  | Reboot sent                                                  |
| Amber, steady   | Waiting for power                                            |
| Purple, pulsing | Restore image being fetched, or restore preparing or running |
| Purple, steady  | Restore outcome unknown; check the host                      |
| Red             | Port fault or failed restore                                 |
| Dimmed          | Outside the selection                                        |

A fault keeps full brightness whatever is selected. While the button is held, the bright ports are the ones the action will reach.

`iasis --board "Bench A" brightness 35` sets all six lights on Bench A to 35 percent. The range is 10 to 100 percent, with a factory setting of 50. Each board keeps its name and brightness through restarts and firmware updates. Its name appears in the OLED title bar.

Iasis cannot tell a target in DFU from a running one: the target looks the same on its power pins either way. Only a host sees the USB mode, so blue turns steady when [`iasisd`](../../README.md#host-agent) or a restore command reports DFU on that port, and the report is forgotten three seconds after the host stops. Without a host a sent DFU keeps pulsing until the target is unplugged or restarted. With one, a DFU not seen within 15 seconds gives way to the mode the host does see. A green light alone gives no information about the target's USB mode.

| Status light    | Meaning                                                     |
| --------------- | ----------------------------------------------------------- |
| Green           | A host is attached                                          |
| White           | No host; the button still sends DFU and Reboot              |
| Blue            | Releasing the button sends DFU                              |
| Amber           | Releasing sends Reboot                                      |
| Purple          | Releasing asks the host for a restore                       |
| Blinking purple | Erase armed. Hold until confirmed, or tap to cancel.        |
| Red             | Shared power protection or root I²C fault. Targets are off. |

Some cables, including USB-C to Lightning, can keep the port green after the device is unplugged. Use `iasis ports` to list devices on the host.

At the Erase prompt, hold for three seconds to start the restore. You can keep holding once it starts. Letting go earlier cancels.

The status LED can briefly turn red when the charging supply is unplugged. Its input protector signals while the voltage falls.

A restore or revive failure reported by the host lights that port red at full brightness. The OLED labels it `FAIL` and shows `Install failed` when the port is selected. The result belongs to the current USB-C attachment. A detach clears it, including one caused by restarting the target. A manual Configurator restore needs a host process to relay its result to the board.

If updates stop during preparation or restore, the port turns steady purple after five seconds and power stays on. Terminal results have no heartbeat timeout. Command results and feedback fields are defined in the [protocol](../protocol/README.md).

## OLED

Connect an [Adafruit 938](https://www.adafruit.com/product/938) to the display I²C connector. The 128×64 screen shows all five physical ports. Bars beside a port number show its USB link: one for USB 2, two for USB 3. A selected port that a hold would reach is filled; any other selected port is outlined. In the menubar, a plug means brick power. The USB symbol marks a live Iasis host session. A network symbol appears while the host shares Internet access on at least one of the board’s ports.

A port reads `DFU` or `REC` while the host sees it in DFU or recovery, `DFU?` for a DFU sent and not yet seen, `BOOT` after a reboot is sent, and `IMG` with a percentage while the host fetches its restore image.

| Selection | Lower area                                                                                             |
| --------- | ------------------------------------------------------------------------------------------------------ |
| All       | Voltage measured at each occupied connector. With a brick, input voltage and reserved/available watts. |
| One port  | Connector voltage and USB link, then the PD contract or default USB allowance.                         |

A target with a USB link shows `USB2` or `USB3` after its voltage, as the hub reports the port. A target in DFU holds USB 2 whatever its cable carries. Without a link nothing is shown, which a cable without data lines and a device that has not enumerated both look like.

The selected port shows its negotiated voltage and current, for example `PD 20V 780mA (16W)`. A port on the brick’s default USB allowance shows `USB 5V 900mA (5W)`. The display preserves the negotiated current and rounds the derived wattage in parentheses to whole watts. The overview counts the capacity reserved from the brick, including the USB starting allowance and voltage tolerance. Holding the button lists the actions the selected targets take and highlights the one a release will send. A fault, restore progress or button feedback replaces the readings. Holding on an identified Mac connected through a non-DFU port shows `Not in DFU port`. Restore is available for the ports the host can put in DFU or already sees in a supported DFU mode.

`ATCH` means a powered USB-C attachment. `USB` means the hub has a data link or the host sees a USB device. `PD` indicates a power contract. `RDY` means the device offers Apple signalling.

During an Iasis restore, three cycling dots mark a port waiting for its first progress update. The bar under each port follows the whole restore. Select a port to read its percentage. `DONE` means the worker reported completion. `UNKN` means the outcome is unknown. Restores started separately in Finder or Apple Configurator show progress on the host.

The display is optional and works with an external restore host or the CM5. It uses mux channel 7 at `0x3d`, or `0x3c` with the address jumper changed. A bus timeout quarantines channel 7, which also carries the charging-input controller, until reboot.

## Board connections

| Function                | Connection                                            |
| ----------------------- | ----------------------------------------------------- |
| Management USB          | USB7206C internal USB 2 port 6                        |
| Root I²C                | GPIO24 SDA / GPIO25 SCL, 400 kHz                      |
| TCA6408A                | Address `0x20`; GPIO32 reset                          |
| TCA9548A                | Address `0x70`; GPIO1 mux reset, gated by board reset |
| Target FUSB302          | Mux channels 0–4, address `0x22`                      |
| Host FUSB302            | Mux channel 5, address `0x22`                         |
| Hub I²C                 | Mux channel 6, address `0x2d`                         |
| CH224                   | Mux channel 7, address `0x22`                         |
| Port VBUS ADC           | GPIO40, 41, 42, 43, 45 for ports 1–5                  |
| Input bus / thermistors | GPIO44 / GPIO47 and GPIO46                            |
| Button / LEDs           | GPIO21 / GPIO20                                       |
| Debug UART              | 3.3 V TX on GPIO0; header NC/GND/TX                   |

### Reset and recovery

A cooperative ROM update disables target power and retains the CM5 supply, host selection, hub-run state and host orientation. The application restores those outputs before releasing GPIO isolation. Each time it starts, the application resets the hub to turn on its I²C interface, so the board enumerates again on the host. Without firmware the hub attaches on its own. Physical RESET resets the hub and drops CM5 power. Power loss or a failed update on a CM5 host requires recovery through the external host port or SWD; the CM5 service port does not power the board.

I²C transactions time out after 20 ms. Recovery resets the mux and MCU I²C controller, quarantines the failed downstream branch until reboot and preserves the root expander outputs. An unrecoverable root fault disables all targets while leaving USB diagnostics available.

The board supplies VCORE externally: firmware sets the internal regulator to HIZ and avoids POWMAN deep sleep. Boot and retained-output handling live in [`src/board.rs`](src/board.rs); flash and RAM placement is in [`memory.x`](memory.x).

The linker layout derives from [Embassy's RP235x example](https://github.com/embassy-rs/embassy/blob/main/examples/rp235x/memory.x), under Apache-2.0.
