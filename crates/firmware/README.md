# Firmware

The RP2354 firmware controls Iasis port power, USB-C attachment, Apple DFU signalling, the button and LEDs. Restore data passes directly through the USB hub between the host and targets.

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
picotool load -v -x target/thumbv8m.main-none-eabihf/release/iasis-firmware
```

Check that `picotool info` finds the intended board before loading. With multiple ROM devices attached, select it using picotool's bus/address options.

For blank flash or a failed application, connect an external host, hold BOOTSEL, press and release RESET, then release BOOTSEL. Both buttons are under the lid. Flashing replaces the application; an interrupted write can be recovered with the same sequence. SWD provides independent access.

Development USB IDs are `1209:0001`. Set `IASIS_USB_VID` and `IASIS_USB_PID` as hexadecimal build variables to override them, and use matching CLI `--vid`/`--pid` arguments.

## Port power

Ports start enabled and reserve power only while attached. Disabling a port opens CC and discharges VBUS. Re-enabling it also retries a fault. ADC, protection, I²C or voltage faults turn off the affected source requests.

| Supply path     | Target offer           | Budget                                                                                               |
| --------------- | ---------------------- | ---------------------------------------------------------------------------------------------------- |
| Host presence   | Fixed 5 V / 0 mA PDO   | 800 mA reserved for Iasis; remaining current selects the aggregate limit below                       |
| Charging supply | Fixed 5 V / 500 mA PDO | 80% of input power, less 3.5 W for logic, 35 W for a powered CM5 carrier and 6 W per attached target |

The host path supports attachment and signalling with charged targets. Its current limit is shared across ports; the 0 mA PDO does not prevent pre-contract draw.

| Upstream allowance | Aggregate target limit |
| ------------------ | ---------------------- |
| 900 mA             | Off                    |
| 1200 mA            | 170 mA                 |
| 1500 mA            | 550 mA                 |
| 3000 mA            | 1890 mA                |

Native USB-C Rp supplies the allowance. On legacy A-to-C connections, `host-budget <mA>` declares the remaining allowance including Iasis, up to 1600 mA. It clears on USB reset and never overrides native Rp.

The charging path requires a confirmed CH224 contract, power-good and 7–21 V on the input bus. It configures each converter for 5.10 V with a 1 A limit, checks safe-zero before connecting and checks output voltage before starting PD.

When a brick is inserted, attached ports keep their output path, while the shared `5V_LOGIC` rail changes source. The presence limit must fit both the host allowance and the brick's remaining budget. Lost or reduced supply capacity can interrupt a restore.

## Controls and status

The [button gestures and CLI commands](../../README.md#use) operate on ready ports. DFU supports Apple Silicon and T2; the button selects Apple Silicon. Commands use the attachment generation to reject requests for a device that has since disconnected.

| LED colour | Meaning                            |
| ---------- | ---------------------------------- |
| Green      | Ready or restore reported complete |
| Blue       | Preparation or activity            |
| Amber      | Waiting or unconfirmed result      |
| Red        | Fault or failure                   |

Host restore feedback expires after five seconds without an update, changing the LED to unconfirmed without cycling power. Command results and feedback fields are defined in the [protocol](../protocol/README.md).

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
