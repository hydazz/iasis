# Firmware

RP2354B firmware for Iasis. It handles USB-C attachment, power, PD/Apple DFU signalling, the button and status LEDs. Restore data travels through the USB hub to the host, not through the RP2354.

## Build and flash

```sh
mise install
mise run firmware:build
```

The ELF is `target/thumbv8m.main-none-eabihf/release/iasis-firmware`. Flash it with Raspberry Pi's [picotool](https://github.com/raspberrypi/picotool) over the RP2354 ROM's PICOBOOT interface:

```sh
target/release/iasis bootloader --serial <board-serial>
picotool info
picotool load -v -x target/thumbv8m.main-none-eabihf/release/iasis-firmware
```

The CLI command requests a ROM reboot; check `picotool info` to confirm the ROM actually appeared. If more than one ROM device is connected, select the intended one using picotool's bus/address options.

There's no resident bootloader, partition table or A/B rollback. Flashing replaces the application, so an interrupted write may leave it unbootable. To enter ROM manually, connect the external host, hold BOOTSEL, press and release RESET, then release BOOTSEL. This also works with blank flash. Physical RESET resets the hub and drops CM5 power. SWD is independent of the USB hub.

### Memory and clock

The RP2354B uses its Cortex-M33, 48 GPIOs and 2 MiB stacked flash. The board has a 12 MHz crystal and runs the default 150 MHz system clock. The linker places the ELF at `0x10000000`, with 2048 KiB flash and 512 KiB main SRAM. The extra two 4 KiB SRAM banks aren't used. The ROM image definition sits just after the vector table, within the first 4 KiB.

The board supplies VCORE externally, so firmware puts the internal regulator into HIZ after unlocking its control register. It doesn't use POWMAN deep sleep.

## Updating without dropping the host

A planned ROM update first turns off the ten target host/charge requests and isolates those GPIO pads. It also isolates GPIO22 (`SIDE_POWER_EN`), GPIO30 (`SHIFT_OE_N`) and GPIO32 (`EXP_RESET_N`) so the current CM5 supply, host selection, hub-run state and external-host orientation can survive the update. ROM USB activity GPIO is disabled too.

Firmware stores those three output levels and the shift-register image in POWMAN scratch registers. On startup, it preserves `HOST_EXT`, clears target-related shift outputs and leaves hub-run/orientation alone. It accepts the saved state only once and only after a watchdog-timer reboot. The pin data, output enable and mux are set before isolation is removed; these pins deliberately bypass the usual Embassy GPIO constructor. The newly flashed image must follow the same startup rules for retention to work.

Cold power-on, brownout and physical RESET clear the retained state and leave CM5 power off. After the watchdog is armed, a timeout uses asynchronous POWMAN reset, which also drops these controls. Firmware reports both raw POWMAN and watchdog reset registers because the chip-level POWMAN cause alone isn't enough to identify a ROM reboot.

The CM5 can potentially remain powered during a cooperative update, but the firmware doesn't yet negotiate the required 20 V input or start it from cold. The CM5 service USB connector isn't another power input. If power is lost or an image fails before watchdog startup, recovery needs the main board's external host connection or SWD. Retention and ROM enumeration still need testing on real Iasis hardware.

## Port power

Ports start enabled for attachment. `power <port> off` disables one; `power <port> on` enables it again or retries a fault. Each port tracks its own attachment generation, deadlines, PD message IDs and command results. Only attached devices reserve power, so an empty port doesn't consume another port's allowance.

### Host-powered (Hub / HubPD without a brick)

The upstream 5 V supply reaches targets through `HOST_TARGET_5V`, an AP22653 aggregate limiter and each port's TPS259470. Native USB-C Rp provides a 1.5 A or 3 A current allowance. On a legacy A-to-C host, Default Rp doesn't tell firmware how much current a Pi can spare: declare the _remaining_ budget, including Iasis, with `host-budget <milliamps>`. That hint tops out at 1600 mA, clears on USB reset and never overrides native Rp. Allow for other USB devices and the Pi 5 fan.

Firmware reserves 800 mA for the board, UI and cable (3.5 W at 4.5 V, rounded up). It then selects an aggregate protection threshold of 170, 550, 1450 or 1890 mA. For example:

| Upstream allowance | Target aggregate threshold |
| ------------------ | -------------------------- |
| 900 mA             | None (not enough headroom) |
| 1200 mA            | 170 mA                     |
| 1500 mA            | 550 mA                     |
| 3000 mA            | 1890 mA                    |

These are protection limits shared by all attached ports, not promised current per device. The policy doesn't talk to the charging converters, which are absent from the Hub build.

For presence and signalling, a port advertises one fixed 5 V / 0 mA PDO and rejects any request for operating current. That doesn't guarantee the device draws zero current before a contract; the limiters still do the electrical protection. The upstream connector needs to stay within 4.4–5.35 V. This fixed-PDO approach, battery use and five-device current draw still need physical validation.

### Powered from a brick (HubPD / CM5)

The current firmware offers a fixed 5 V / 500 mA PDO to a target. It requires a confirmed CH224 input contract, power-good, healthy fault inputs and 7–21 V on the HV bus. The power budget uses 80% of input power, reserves 3.5 W for logic and 32 W if the CM5 side board is already powered, then reserves 6 W per attached target for its 1 A stage limit and conversion losses. At 9 V / 3 A it admits three targets; at 20 V / 3 A it admits five without CM5 power.

The firmware doesn't change the CH224 input-voltage selection. When a brick is plugged in, ports already powered from the host stay on that path rather than switching supplies during a restore.

After Rd detection and a power grant, the target converter is disabled and discharged for 250 ms. Firmware writes and checks 5.10 V and the 1 A limit, then waits 20 ms to settle; this overlaps the 150 ms CC debounce. Both power paths check Rd and safe-zero again before closing the gate. PD only starts once connector voltage is good (4.85–5.35 V on the brick path).

Missing ADC readings, protection faults, I²C errors or bad voltage shut off source requests. Detaching or disabling a port opens CC and discharges the connector. Reconnection waits for a fresh safe-zero reading.

### Host orientation and Type-C timing

The external host's CC/orientation and current advertisement come from mux channel 5, while both FUSB302 Rd terminations remain in place. Stable orientation updates only expander P2; BMC activity alone doesn't extend a current allowance.

Sampling runs every 10 ms. A separate 5 ms task turns off direct host power requests once the last valid CC reading is 50 ms old, even if the I²C task is hung recovering a branch. USB reset, deconfiguration and suspend also withdraw host allocation. The aim is to stay within the 60 ms Type-C adjustment bound; actual GPIO and current timing still need measurement.

## PD and Apple actions

PD handling covers Source_Capabilities, Request/Accept/PS_RDY, soft reset and rejection of role swaps. Requests above the advertised PDO are rejected. A hard reset puts that port through disconnect and safe-zero checks again. Higher-voltage charging, VCONN/cable discovery and renegotiating shared capacity aren't implemented yet.

DFU uses the Apple Silicon or T2 action over SOP-double-prime Debug after a debug probe. Reboot sends its action directly. Each command gets one logical attempt with the FUSB302's bounded retries; there's no cable re-priming loop. An Apple command can take effect without GoodCRC, so status distinguishes an acknowledged message, an unconfirmed attempt and a transmit failure. The host needs to check target USB enumeration or behaviour. Commands include the last attachment generation, and stale ones are rejected.

## Button, LEDs and feedback

The active-low button is on GPIO21, with 30 ms debounce:

| Gesture                       | Action                                                           |
| ----------------------------- | ---------------------------------------------------------------- |
| Click (400 ms gesture window) | DFU on ready ports (Apple Silicon)                               |
| Hold for 1.2 s                | Reboot                                                           |
| Two clicks, then hold         | Send a restore request with port mask and attachment generations |

Only a listening host with a prepared IPSW and erase authorization can act on the restore request. Firmware never erases an Apple device. T2 DFU is available from the CLI.

GPIO20 drives six SK6812SIDE-A LEDs using PIO/DMA: status, then ports 5 through 1. Green is ready/restore reported, blue is preparation/activity, amber is waiting/unconfirmed, and red is fault/failure. RGB sums are capped at 85/255 and frames include more than 200 µs reset time.

Restore feedback is tied to each port and attachment generation. If the host stops reporting for five seconds, active feedback becomes `Unconfirmed` without changing port power.

## Board connections

| Function              | Connection                                         |
| --------------------- | -------------------------------------------------- |
| Management USB        | USB7206C internal USB 2 downstream port 6          |
| Root I²C              | GPIO24 SDA, GPIO25 SCL; 400 kHz, external pull-ups |
| TCA6408A              | Root `0x20`, reset on GPIO32                       |
| TCA9548A              | Root `0x70`, `AON_RESET_N AND MUX_RUN`, GPIO1      |
| Target FUSB302        | Mux channels 0–4, address `0x22`                   |
| External-host FUSB302 | Mux channel 5, address `0x22`                      |
| Hub configuration     | Mux channel 6, normally left deselected            |
| CH224                 | Mux channel 7, address `0x22`                      |
| Port VBUS ADC         | GPIO40, 41, 42, 43, 45 (ports 1–5)                 |
| HV bus ADC            | GPIO44                                             |
| Thermistors A/B       | GPIO47 / GPIO46                                    |

### I²C recovery

An I²C transaction has a 20 ms deadline. On timeout, firmware pulses GPIO1 low for 10 µs, resets the MCU I²C controller and checks access to the deselected mux. The downstream branch that faulted is quarantined until the next boot, and its target is switched off; other ports can keep working. The root expander isn't reset, so hub run, host selection and external-host orientation stay as they were.

If the root bus can't recover, all target requests are disabled but USB and ADC diagnostics remain available. Status records timeouts, recoveries and the quarantined-channel mask. Physical RESET clears quarantine by resetting the whole board.

GPIO1 replaces UART RX. The debug header is NC/GND/TX, with GPIO0 for UART TX; GPIO32 still resets the expander separately. The mux AND gate gives AON reset precedence, with a 4.7 kΩ GPIO1 pull-down to handle RP2350 A2 input leakage.

The hub's I²C branch isn't probed. It starts without the root pull-ups, allowing its default USB configuration to work without MCU setup. To use the hub's SMBus interface, it would need an intentional reset/configuration sequence; selecting mux channel 6 after boot won't do it.

## Still to test on hardware

Unit tests cover divider calculations, packet CRC/framing, PDO/RDO rules, attachment admission, button gestures and LED brightness. The actual board still needs ADC calibration, CC and discharge timing, fault injection, PD/Apple interoperability and ROM-update retention measurements. Five connected targets also need current-draw testing.

The image placement in `memory.x` comes from [Embassy's RP235x example](https://github.com/embassy-rs/embassy/blob/main/examples/rp235x/memory.x) (Apache-2.0). Embassy, Raspberry Pi ROM sources and chip datasheets inform the implementation; no RestoreKit source is included.
