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

The application records those three output levels and the shift-register image in POWMAN scratch registers. Startup preserves HOST_EXT while clearing target-related shift outputs, and leaves the expander’s orientation and hub-run pins unchanged. On return from the ROM it accepts a valid one-shot record only with a watchdog timer reboot reason. Startup configures output data, output enable and muxing before releasing pad isolation. It never runs the normal Embassy GPIO constructor on these retained controls. Only an image honouring this startup contract can preserve the powered host through an update.

Cold power-on, brownout and physical RESET reset the pad latches; CM5 power defaults off. Once startup arms the watchdog, expiry uses asynchronous POWMAN reset, deliberately dropping the retained controls. The raw POWMAN and watchdog reset registers are both reported: POWMAN's last chip-level cause alone does not identify a normal ROM reboot. An invalid image or failure before watchdog startup may still need physical recovery.

ROM-only updating is a deliberate trade-off: no bootloader or A/B image is present. Retention can keep an already-powered CM5 alive during a cooperative update, but cold power-on or RESET leaves it off. This firmware does not negotiate the required 20 V supply or start the CM5. Its service USB connector is not an alternative power input. Recovery from lost power or unusable firmware therefore needs an external host through the main board, or SWD. Actual retention waveforms and ROM enumeration on Iasis remain unmeasured.

## Port operation

Ports start disabled. The host enables each port explicitly; the request remains enabled while waiting for sufficient supply. Each port has its own attachment generation, deadlines, PD message IDs and command result. Firmware never forwards restore data.

External-host orientation is measured on mux channel 5 with both FUSB302 Rd terminations maintained. Samples taken during CC signalling are discarded; a stable orientation sets only expander P2. Detach or an uncertain reading leaves its last routing value in place. The firmware does not negotiate upstream PD or allocate upstream host power.

The implemented source policy is one fixed 5 V / 500 mA PDO from the optional charging stage. A confirmed CH224 PD contract, asserted power-good input, no input-protection fault and nominal HV bus between 7 and 21 V are required. The firmware uses 80% of the observed input power, reserves 3.5 W for logic and another 32 W if side power is already on, then reserves 6 W per enabled port. That covers the nominal 1 A stage limit and margin despite advertising only 500 mA. For example, a 9 V / 3 A input admits three ports; a 20 V / 3 A input admits five without the side board powered. This firmware leaves the CH224’s input-voltage selection unchanged. No host-powered target budget is assumed.

Before advertising CC, firmware disables and discharges the isolated TPS55288 output for 250 ms, clears latched protection flags, programs and reads back the 5.10 V reference and nominal 1 A current limit, and enables the stage. Output protection remains at its lower threshold. After 150 ms of CC debounce and a manual Rd check, the correct orientation is selected and the target gate closes. PD starts only after nominal connector voltage reaches 4.85–5.35 V. Missing ADC data, protection faults, bus errors and voltage failures remove the source request. Detach and disable open CC, turn off the stage and discharge the connector; a fresh safe-zero observation is required before reconnecting.

Requests selecting anything beyond the advertised fixed PDO are rejected. The implementation supports Source_Capabilities, Request/Accept/PS_RDY, soft reset and rejection of role swaps. A received hard reset recycles the port through full disconnect and safe-zero checks. It does not implement a complete USB PD policy engine, higher-voltage PDOs, VCONN/cable discovery or shared-capacity renegotiation.

DFU signalling sends the attributed Apple Silicon or T2 action over SOP-double-prime Debug after a debug probe. Reboot sends its action directly. Each is one logical attempt with the FUSB302’s bounded retries; no automatic cable re-priming or repeated action loop is implemented. Apple actions may execute without GoodCRC, so status distinguishes acknowledged transport, an unconfirmed attempt and a transmission failure. USB enumeration and target behaviour establish the result. The host command includes the last attachment generation; stale commands are rejected.

Nominal divider calculations, packet framing/CRC, PDO/RDO rules, independent message counters and USB2 path mapping are checked without hardware. On-board validation still needs ADC calibration, stage/discharge waveforms, CC timing, protection fault injection and PD/Apple interoperability with real targets. These measurements do not gate building the firmware or host.

## Board interfaces

| Function              | Connection                                         |
| --------------------- | -------------------------------------------------- |
| USB management        | USB7206C internal downstream USB2 port 6           |
| I²C root              | GPIO24 SDA, GPIO25 SCL; 400 kHz, external pull-ups |
| TCA6408A              | Root address `0x20`, reset GPIO32                  |
| TCA9548A              | Root address `0x70`, reset tied to `AON_RESET_N`   |
| Target FUSB302        | Mux channels 0–4, address `0x22`                   |
| External-host FUSB302 | Mux channel 5, address `0x22`                      |
| Hub configuration     | Mux channel 6, left deselected                     |
| CH224                 | Mux channel 7, address `0x22`                      |
| Port VBUS ADC         | GPIO40, 41, 42, 43, 45, in physical port order     |
| HV bus ADC            | GPIO44                                             |
| Thermistors A, B      | GPIO47, GPIO46                                     |

Every I²C transaction has a 20 ms deadline. A timeout stops I²C for that boot, disables all direct target power requests and leaves USB and ADC telemetry running. A responding bus with an address/transaction error faults the affected port; a missing CH224 or expander reading withdraws the shared power allocation. On this PCB, resetting the expander does not reset the mux. A permanently stuck selected branch needs board-wide reset or power removal; firmware does not claim to recover it through an I²C write.

The hub branch is not probed. The hub boots without seeing the root bus pull-ups, so its default USB path can work without MCU configuration. Enabling its SMBus interface requires a deliberate hub-reset/configuration sequence; selecting channel 6 after boot is not sufficient.

The image-section placement in `memory.x` is adapted from [Embassy's RP235x example](https://github.com/embassy-rs/embassy/blob/main/examples/rp235x/memory.x), under Apache-2.0. Embassy, Raspberry Pi's ROM source and the manufacturer register descriptions inform the implementation; no RestoreKit source is vendored.
