# IasisOS

IasisOS is the firmware on the hub's RP2354. It controls port power, USB-C attachment, Apple DFU signalling, the button, LEDs and optional OLED. Restore data passes directly through the USB hub between the host and targets.

## Build and update

From the repository root:

```sh
mise install
mise run host:build
target/release/iasis --serial <hub-serial> update
```

`mise run host:build` builds IasisOS and the host tools that carry it, and `update` writes that image; `update path/to/IasisOS.uf2` writes another. The hub restarts into its RP2350 boot ROM, which takes the image, and returns as the new IasisOS. The host writes only the ROM that answers with the hub's chip ID from its USB hub's management port. `mise run firmware:build` builds `IasisOS.uf2` alone, the form each release carries.

A hub with blank flash enumerates as its boot ROM when an external host is connected, and `update` writes it the same way. For a failed application, connect an external host, hold BOOTSEL, press and release RESET, then release BOOTSEL. Both buttons are under the lid. An interrupted write can be recovered with the same sequence. SWD provides independent access.

IasisOS writes a trace over RTT, at `info` and above unless `DEFMT_LOG` sets another level for a build. A probe on SWD reads it from a running hub without reflashing it: `probe-rs attach --chip RP235x target/thumbv8m.main-none-eabihf/release/iasis-firmware`, given the ELF of the build the hub runs. The trace never holds the firmware up: what the probe does not collect in time is dropped.

IasisOS and the host tools share the development USB IDs `1209:0001` from the protocol crate.

## Port power

Ports start enabled and reserve power only while attached. Disabling a port removes Rp and discharges VBUS. PD-capable targets negotiate fixed supplies over USB PD revision 2.0.

A port whose voltage or negotiation fails switches its output off, discharges VBUS and attaches again. A protection trip holds the port off until the target is unplugged or the port is enabled again. Three failures within 30 seconds hold it off the same way, or until the hub's supply changes.

| Supply path     | Target offer                               | Budget                                                                                                                                                 |
| --------------- | ------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Host presence   | Fixed 5 V / 0 mA PDO                       | Iasis reserves 650 mA. The remaining current selects the shared limiter below.                                                                         |
| Charging supply | Fixed 5, 9, 15 or 20 V, up to 3 A per port | The brick contract covers input cable and board losses, logic power and a 35 W reserve when the CM5 carrier is powered. The ports share the remainder. |

With host USB alone, charged Apple targets use 5 V for attachment and PD signalling. A 5 V / 0 mA offer is how PD says a source has no power to give, and the OLED shows `No charging`. Targets running their own OS take nothing on it. iPads draw anyway while they restore, roughly 0.15–0.3 A each while iBEC loads and about 55 mA in the restore OS, all through the shared limiter below; M5 Macs draw nothing measurable. The port ADC measures connector voltage only. Ordinary USB peripherals draw their operating current from the same 5 V supply. Each port has a nominal 0.51 A current limit. Component tolerance can put the trip point below 500 mA. All five also share a limiter, which holds their total current to the threshold below.

| Host current advertisement | Shared host-port limiter (max) |
| -------------------------- | ------------------------------ |
| Default USB power          | Target power off               |
| 1.5 A                      | 550 mA                         |
| 3 A                        | 1890 mA                        |

On Default USB power, including a USB-A to USB-C cable, the hub stays visible and the five target outputs stay off until the charging input has power. The board budgets with the target outputs off are 100 mA on USB 2 and 240 mA on SuperSpeed in the configuration descriptors. Rev A's USB7206C reports local power good even when the target outputs are off. Iasis reads the supply and switch state from the RP2354.

Rev A exceeds the USB 2 suspend-current limit on host power. Use the charging input with hosts that enforce it.

On the charging input, Iasis requests the highest-power fixed profile it supports, up to 20 V. The host USB port stays at 5 V.

Each port starts at 5 V. With a 20 V brick, one port can offer 5, 9, 15 and 20 V at different currents for the same share of power, up to 3 A. The target chooses its profile independently. With a 9 or 12 V input, the ports offer 5 and 9 V.

Iasis reserves 5 V / 900 mA to start a charging port. A new target waits if existing charging ports can release that much power. When even their minimum loads leave too little, the new target can use presence if both inputs cover its shared tier. Each port holds the larger of what it was offered and what it agreed until the device has moved to a lower offer, so every commitment stays funded. Devices share the power at one level. A device that takes less than its share leaves the rest to the others; when one arrives the others are offered less, and it starts as they move down. A device that does not answer PD keeps its 5 V allowance. When power becomes available, Iasis updates the charging ports one at a time, so a device can take a higher offer without a replug. The highest voltage offers the most power, so rounding does not move a device between voltages, and a device that changes profile at the same share is sent no new offers. An active restore keeps its contract and the offers it was given while the supply covers that contract; a request for another voltage, or for more than the supply has left, is refused. If the supply drops below what the ports hold, as when side power turns on, restores keep power first while their contracts fit, smallest first, then the other ports, smallest first; the rest switch off and start again when their 5 V allowance fits.

When a brick is connected, attached targets reconnect on charging power as capacity allows. A port preparing or running a restore or revive keeps its 5 V path until the job ends. Other ports can charge in the meantime. The brick and host must both cover the shared limiter tier while a port uses presence.

Removing the brick interrupts every target on a charging stage, including targets charging at 5 V. Leave the brick connected throughout a restore on a charging port. Presence ports keep their switches on while the host allowance and connector voltage hold.

## Controls and status

`iasis status --watch` shows the hub temperature near the port 1 and 2 output switches (TH1191, sensor A) and the shared core regulator (TH1192, sensor B). It can run during a restore. Use the raw ADC counts in `status --json` for calibration.

The [button and CLI controls](../../README.md#use) operate on ready ports. USB peripherals can use the data link without negotiating Power Delivery. Once a PD contract has settled, Iasis asks the target which Apple debug actions its port offers, and sends DFU or reboot only where the target lists them. DFU also needs the port to list DFU USB, which a Mac does on its [DFU port](https://support.apple.com/en-us/120694) alone: told to enter DFU from another port, some Macs restart into DFU where no host can reach them. Those ports still take reboot. A target that lists neither still charges and can be restored once it is in DFU by other means; a USB-C to Lightning cable does not carry Apple debug. `iasis status` prints each port's list. Commands use the attachment generation to reject requests for a device that has since disconnected.

A colour is what the port holds, a pulse is work under way or a result awaited, and brightness is the selection.

| Port light      | Meaning                                                      |
| --------------- | ------------------------------------------------------------ |
| Off             | Empty or disabled port, or a device still being identified   |
| Green           | Target Iasis can act on, or restore complete                 |
| White           | Device Iasis cannot act on, with or without a PD contract    |
| Blue, steady    | A host sees the target in DFU or recovery                    |
| Blue, pulsing   | DFU sent; no host has seen the result                        |
| Amber, pulsing  | Reboot sent                                                  |
| Amber, steady   | Waiting for power                                            |
| Purple, pulsing | Restore image being fetched, or restore preparing or running |
| Purple, steady  | Restore outcome unknown; check the host                      |
| Red             | Port fault or failed restore                                 |
| Dimmed          | Outside the selection                                        |

A fault keeps full brightness whatever is selected. While the button is held, the bright ports are the ones the action will reach.

`iasis --hub "Bench A" brightness 35` sets all six lights on Bench A to 35 percent. The range is 10 to 100 percent, with a factory setting of 50. Each hub keeps its name and brightness through restarts and firmware updates. Its name appears in the OLED title bar.

Iasis cannot tell a target in DFU from a running one: the target looks the same on its power pins either way. Only a host sees the USB mode, so blue turns steady when [`iasisd`](../../README.md#host-agent) or a restore command reports DFU on that port, and the report is forgotten three seconds after the host stops. Without a host a sent DFU keeps pulsing until the target is unplugged or restarted. With one, a DFU not seen within 15 seconds gives way to the mode the host does see. A green light alone gives no information about the target's USB mode.

| Status light    | Meaning                                              |
| --------------- | ---------------------------------------------------- |
| Green           | A host is attached                                   |
| White           | No host; the button still sends DFU and Reboot       |
| Blue            | Releasing the button sends DFU                       |
| Amber           | Releasing sends Reboot                               |
| Purple          | Releasing asks the host for a restore                |
| Blinking purple | Erase armed. Hold until confirmed, or tap to cancel. |
| Red             | Charging input protection or root I²C fault.         |

Some cables, including USB-C to Lightning, can keep the port white after the device is unplugged. Use `iasis ports` to list devices on the host.

At the Erase prompt, hold for three seconds to start the restore. You can keep holding once it starts. Letting go earlier cancels.

The status LED can briefly turn red when the charging supply is unplugged. Its input protector signals while the voltage falls.

A restore or revive failure reported by the host lights that port red at full brightness. The OLED labels it `FAIL` and shows `Install failed` when the port is selected. The result belongs to the current USB-C attachment. A detach clears it, including one caused by restarting the target. A manual Configurator restore needs a host process to relay its result to the hub.

If updates stop during preparation or restore, the port turns steady purple after five seconds and power stays on. Terminal results have no heartbeat timeout. Command results and feedback fields are defined in the [protocol](../protocol/README.md).

## OLED

Connect an [Adafruit 938](https://www.adafruit.com/product/938) to the display I²C connector. The 128×64 screen shows all five physical ports. Bars beside a port number show its USB link: one for USB 2, two for USB 3. A selected port that a hold would reach is filled; any other selected port is outlined. In the menubar, a plug means brick power. The USB symbol marks a live Iasis host session. A network symbol appears while the host shares Internet access on at least one of the hub’s ports.

A port reads `DFU` or `REC` while the host sees it in DFU or recovery, `DFU?` for a DFU sent and not yet seen, `BOOT` after a reboot is sent, and `IMG` with a percentage while the host fetches its restore image.

| Selection | Lower area                                                                                                                                                                         |
| --------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| All       | Connector voltages. An available restore estimate replaces the usual brick input voltage and reserved/available watts with minutes left until the last restore or revive finishes. |
| One port  | Connector voltage and USB link, then the PD contract or default USB allowance.                                                                                                     |

A target with a USB link shows `USB2` or `USB3` after its voltage, as the hub reports the port. A target in DFU holds USB 2 whatever its cable carries. Without a link nothing is shown, which a cable without data lines and a device that has not enumerated both look like.

The selected port shows its negotiated voltage and current, for example `PD 20V 780mA (16W)`. A port on the brick’s default USB allowance shows `USB 5V 900mA (5W)`. The display preserves the negotiated current and rounds the derived wattage in parentheses to whole watts. The overview counts the capacity reserved from the brick, including the USB starting allowance and voltage tolerance. Holding the button lists the actions the selected targets take and highlights the one a release will send. A fault, restore progress or button feedback replaces the readings. Holding on a target whose connector carries no DFU USB, such as a Mac's other port, shows `Not in DFU port`. Restore is available for the ports the host can put in DFU or already sees in a supported DFU mode.

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

A cooperative ROM update disables target power and retains the CM5 supply, host selection, hub-run state and host orientation. The application restores those outputs before releasing GPIO isolation. Each time it starts, the application resets the hub to turn on its I²C interface, so the hub enumerates again on the host. Without firmware the hub attaches on its own. Physical RESET resets the hub and drops CM5 power. Power loss or a failed update on a CM5 host requires recovery through the external host port or SWD; the CM5 service port does not power the hub.

I²C transactions time out after 20 ms. Recovery resets the mux and MCU I²C controller, quarantines the failed downstream branch until reboot and preserves the root expander outputs. An unrecoverable root fault disables all targets while leaving USB diagnostics available.

The board supplies VCORE externally: firmware sets the internal regulator to HIZ and avoids POWMAN deep sleep. Boot and retained-output handling live in [`src/board.rs`](src/board.rs); flash and RAM placement is in [`memory.x`](memory.x).

The linker layout derives from [Embassy's RP235x example](https://github.com/embassy-rs/embassy/blob/main/examples/rp235x/memory.x), under Apache-2.0.
