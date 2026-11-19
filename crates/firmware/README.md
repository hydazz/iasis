# Firmware

The RP2354 firmware controls Iasis port power, USB-C attachment, Apple DFU signalling, the button, LEDs and optional OLED. Restore data passes directly through the USB hub between the host and targets.

## Build and flash

From the repository root:

```sh
mise install
mise run firmware:build
mise run host:build
```

Use Raspberry Pi's [picotool](https://github.com/raspberrypi/picotool) to flash the ELF through the host USB connection:

```sh
target/release/iasis --serial <board-serial> bootloader
picotool info
picotool load -v -x target/thumbv8m.main-none-eabihf/release/iasis-firmware -t elf
```

Check that `picotool info` finds the intended board before loading. With multiple ROM devices attached, select it using picotool's bus/address options.

For blank flash or a failed application, connect an external host, hold BOOTSEL, press and release RESET, then release BOOTSEL. Both buttons are under the lid. Flashing replaces the application; an interrupted write can be recovered with the same sequence. SWD provides independent access.

Development USB IDs are `1209:0001`. Set `IASIS_USB_VID` and `IASIS_USB_PID` as hexadecimal build variables to override them, and use matching CLI `--vid`/`--pid` arguments.

## Port power

Ports start enabled and reserve power only while attached. Disabling a port removes Rp and discharges VBUS. Targets negotiate fixed supplies over USB PD revision 2.0.

A port whose voltage or negotiation fails switches its output off, discharges VBUS and attaches again. Three failures within 30 seconds, or a protection trip, hold the port off until the target is unplugged or the port is enabled again.

| Supply path     | Target offer                               | Budget                                                                                                                                                 |
| --------------- | ------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Host presence   | Fixed 5 V / 0 mA PDO                       | Iasis reserves 800 mA. The remaining current selects the shared limiter below.                                                                         |
| Charging supply | Fixed 5, 9, 15 or 20 V, up to 3 A per port | The brick contract covers input cable and board losses, logic power and a 35 W reserve when the CM5 carrier is powered. The ports share the remainder. |

The host path supports attachment and signalling with charged targets. Its current limit is shared across ports; the 0 mA PDO does not prevent pre-contract draw.

| Upstream allowance | Aggregate target allowance |
| ------------------ | -------------------------- |
| 900 mA             | Off                        |
| 1200 mA            | 170 mA                     |
| 1500 mA            | 550 mA                     |
| 3000 mA            | 1890 mA                    |

Native USB-C Rp supplies the allowance. On legacy A-to-C connections, `host-budget <mA>` declares the remaining allowance including Iasis, up to 1600 mA. It clears on USB reset and never overrides native Rp.

On the charging input, Iasis requests the highest-power fixed profile it supports, up to 20 V. The host USB port stays at 5 V.

Each port starts at 5 V. With a 20 V brick, one port can offer 5, 9, 15 and 20 V at different currents for the same share of power, up to 3 A. The target chooses its profile independently. With a 9 or 12 V input, the ports offer 5 and 9 V.

A new device first receives spare capacity. If it reports that its best offer is insufficient, Iasis can renegotiate with other charging devices. Their existing contracts stay funded until they have stepped down. An active restore keeps its contract while the supply can cover it. A port waits if its starting allowance is unavailable.

Leave the power supply connected throughout a restore. Changing the input can interrupt USB to the targets while the controller stays online.

## Controls and status

`iasis status --watch` shows the board temperature near the port 1 and 2 output switches (TH1191, sensor A) and the shared core regulator (TH1192, sensor B). On macOS it can run during a restore. Use the raw ADC counts in `status --json` for calibration.

The [button and CLI controls](../../README.md#use) operate on ready ports. A target is identified by its Power Delivery contract. Once the contract has settled, Iasis asks the target which Apple debug actions its port offers, and sends DFU or reboot only where the target lists them. DFU also needs the port to list DFU USB, which a Mac does on its [DFU port](https://support.apple.com/en-us/120694) alone: told to enter DFU from another port, some Macs restart into DFU where no host can reach them. Those ports still take reboot. A target that lists neither still charges and can be restored once it is in DFU by other means; a USB-C to Lightning cable does not carry Apple debug. `iasis status` prints each port's list. DFU supports Apple Silicon and T2; the button selects Apple Silicon. Commands use the attachment generation to reject requests for a device that has since disconnected.

| Port light | Meaning                                  |
| ---------- | ---------------------------------------- |
| Off        | Empty or disabled target port            |
| Green      | Ready for signalling or restore complete |
| White      | Powered device without a PD contract     |
| Blue       | Activity                                 |
| Amber      | Preparation, waiting or unknown result   |
| Red        | Fault or failure                         |
| Dimmed     | Outside the selection                    |

A fault keeps full brightness whatever is selected. While the button is held, the bright ports are the ones the action will reach.

| Status light    | Meaning                                 |
| --------------- | --------------------------------------- |
| Teal            | Running                                 |
| Blue            | Releasing the button sends DFU          |
| Amber           | Releasing sends Reboot                  |
| Purple          | Releasing asks the host for a restore   |
| Blinking purple | Erase armed: hold 3 s, or tap to cancel |
| Red             | Board fault; targets are off            |

Some cables, including USB-C to Lightning, can keep the port green after the device is unplugged. Use `iasis ports` to list devices on the host.

Host restore feedback expires after five seconds without an update, changing the LED to unconfirmed without cycling power. Command results and feedback fields are defined in the [protocol](../protocol/README.md).

## OLED

Connect an [Adafruit 938](https://www.adafruit.com/product/938) to the display I²C connector. The 128×64 screen shows all five physical ports. A selected port that a hold would reach is filled; any other selected port is outlined. A lightning bolt indicates brick power and a USB symbol a waiting restore host.

| Selection | Lower area                                                                                               |
| --------- | -------------------------------------------------------------------------------------------------------- |
| All       | Voltage measured at each occupied connector, then the input voltage and contract watts out of the supply |
| One port  | Voltage measured at its connector and the plug orientation, then its contract                            |

A contract is the most a target may draw, not what it draws: `9V 3A (27W)` reserves 27 W of the supply whether or not the target uses it, and `50/68W` totals those reservations against the watts available. Iasis measures voltage, not current. On host power the All view shows the host allowance and the shared target limit. Holding the button lists the actions the selected targets take and highlights the one a release will send. A fault, restore progress or button feedback replaces the readings.

`RDY` means ready for signalling. `CHG` marks a target that charges and lists neither DFU nor reboot. `?` marks a powered device without a PD contract, which usually means a charge-only cable or an unsupported device. During an Iasis restore, three cycling dots mark a port waiting for stage progress. The bar under each port fills with the current stage and can start over at the next stage. Select a port to read its stage percentage. `DONE` means the worker reported completion. `UNKN` means the outcome is unknown. Restores started separately in Finder or Apple Configurator show progress on the host.

The display is optional and works with an external restore host or the CM5. It uses mux channel 7 at `0x3d`, or `0x3c` with the address jumper changed. A bus timeout quarantines channel 7, which also carries the charging-input controller, until reboot.

## Board connections

| Function                | Connection                                              |
| ----------------------- | ------------------------------------------------------- |
| Management USB          | USB7206C internal USB 2 port 6                          |
| Root I²C                | GPIO24 SDA / GPIO25 SCL, 400 kHz                        |
| TCA6408A                | Address `0x20`; GPIO32 reset                            |
| TCA9548A                | Address `0x70`; GPIO1 mux reset, gated by board reset   |
| Target FUSB302          | Mux channels 0–4, address `0x22`                        |
| Host FUSB302            | Mux channel 5, address `0x22`                           |
| Hub configuration       | Mux channel 6; leave deselected for default USB startup |
| CH224                   | Mux channel 7, address `0x22`                           |
| Port VBUS ADC           | GPIO40, 41, 42, 43, 45 for ports 1–5                    |
| Input bus / thermistors | GPIO44 / GPIO47 and GPIO46                              |
| Button / LEDs           | GPIO21 / GPIO20                                         |
| Debug UART              | 3.3 V TX on GPIO0; header NC/GND/TX                     |

### Reset and recovery

A cooperative ROM update disables target power and retains the CM5 supply, host selection, hub-run state and host orientation. The application restores those outputs before releasing GPIO isolation. Physical RESET resets the hub and drops CM5 power. Power loss or a failed update on a CM5 host requires recovery through the external host port or SWD; the CM5 service port does not power the board.

I²C transactions time out after 20 ms. Recovery resets the mux and MCU I²C controller, quarantines the failed downstream branch until reboot and preserves the root expander outputs. An unrecoverable root fault disables all targets while leaving USB diagnostics available.

The board supplies VCORE externally: firmware sets the internal regulator to HIZ and avoids POWMAN deep sleep. Boot and retained-output handling live in [`src/board.rs`](src/board.rs); flash and RAM placement is in [`memory.x`](memory.x).

The linker layout derives from [Embassy's RP235x example](https://github.com/embassy-rs/embassy/blob/main/examples/rp235x/memory.x), under Apache-2.0.
