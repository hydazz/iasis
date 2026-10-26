# Iasis firmware

The firmware targets the RP2354B's Cortex-M33, 48 GPIOs and 2 MiB stacked flash. It uses the board's 12 MHz crystal and the default 150 MHz system clock. The external shared VCORE supply requires the internal regulator in HIZ; the firmware unlocks and sets that register and does not enter POWMAN deep-sleep states.

## Build and load

```sh
mise install
mise exec -- rustup target add thumbv8m.main-none-eabihf
mise run firmware:build
```

The output is `target/thumbv8m.main-none-eabihf/release/iasis-firmware`, an ELF image beginning at `0x10000000`. The linker allows 2048 KiB flash and 512 KiB main SRAM. The two additional 4 KiB SRAM banks are unused. The ROM image definition is kept after the vector table, inside the first 4 KiB. There is one application, with no flash-resident bootloader, partition table or A/B slots.

Use Raspberry Pi's [picotool](https://github.com/raspberrypi/picotool) to load the ELF through the ROM's PICOBOOT interface. On an operating board:

```sh
target/release/iasis bootloader --serial <board-serial>
picotool info
picotool load -v -x target/thumbv8m.main-none-eabihf/release/iasis-firmware
```

Select the intended ROM device with picotool's bus/address options if several are attached. The CLI reports acceptance of the update request; `picotool info` establishes ROM enumeration. Loading replaces the application in place. Interrupted writes have no rollback. Recovery uses the external host connector: hold BOOTSEL, press and release RESET, then release BOOTSEL. Physical RESET also resets the hub and drops CM5 power. A blank controller can enter the same ROM recovery path. SWD remains available independently of the hub.

## Reset and update

During a planned update, firmware disables the ten target host/charge requests and isolates their pads. It also isolates GPIO22 (`SIDE_POWER_EN`), GPIO30 (`SHIFT_OE_N`) and GPIO32 (`EXP_RESET_N`). Together these preserve CM5 power, the shift-register host selection, and expander outputs such as hub run and external-host orientation. ROM USB activity GPIO is explicitly disabled.

The application records those three output levels in POWMAN scratch registers. On return from the ROM it accepts a valid one-shot record only with a watchdog timer reboot reason. Startup configures output data, output enable and muxing before releasing pad isolation. It never runs the normal Embassy GPIO constructor on these retained controls. Only an image honouring this startup contract can preserve the powered host through an update.

Cold power-on, brownout and physical RESET reset the pad latches; CM5 power defaults off. Once startup arms the watchdog, expiry uses asynchronous POWMAN reset, deliberately dropping the retained controls. The raw POWMAN and watchdog reset registers are both reported: POWMAN's last chip-level cause alone does not identify a normal ROM reboot. An invalid image or failure before watchdog startup may still need physical recovery.

The diagnostic firmware does not turn on CM5 power or implement the required 20 V supply policy. Retention supports an already-powered host during an intentional update; it is not a power-loss recovery mechanism. Actual waveforms and enumeration on Iasis hardware remain unmeasured.

## Board interfaces

| Function              | Connection                                         |
| --------------------- | -------------------------------------------------- |
| USB management        | USB7206C internal downstream USB2 port 6           |
| I²C root              | GPIO24 SDA, GPIO25 SCL; 100 kHz, external pull-ups |
| TCA6408A              | Root address `0x20`, reset GPIO32                  |
| TCA9548A              | Root address `0x70`, reset tied to `AON_RESET_N`   |
| Target FUSB302        | Mux channels 0–4, address `0x22`                   |
| External-host FUSB302 | Mux channel 5, address `0x22`                      |
| Hub configuration     | Mux channel 6, left deselected                     |
| CH224                 | Mux channel 7, address `0x22`                      |
| Port VBUS ADC         | GPIO40, 41, 42, 43, 45, in physical port order     |
| HV bus ADC            | GPIO44                                             |
| Thermistors A, B      | GPIO47, GPIO46                                     |

Every I²C transaction has a 20 ms deadline. A timeout stops bus sampling for that boot and leaves USB and ADC telemetry running. On this PCB, resetting the expander does not reset the mux. A permanently stuck selected branch needs board-wide reset or power removal; firmware does not claim to recover it through an I²C write.

The hub branch is not probed. The hub boots without seeing the root bus pull-ups, so its default USB path can work without MCU configuration. Enabling its SMBus interface requires a deliberate hub-reset/configuration sequence; selecting channel 6 after boot is not sufficient.

The image-section placement in `memory.x` is adapted from [Embassy's RP235x example](https://github.com/embassy-rs/embassy/blob/main/examples/rp235x/memory.x), under Apache-2.0. Embassy, Raspberry Pi's ROM source and the manufacturer register descriptions inform the implementation; no RestoreKit source is vendored.
