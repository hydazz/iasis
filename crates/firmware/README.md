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

Ports start enabled for attachment. `power <port> off` disables one; `power <port> on` retries a fault. Each port has independent attachment generations, deadlines, PD message IDs and command results. Only detected attachments reserve power, so empty connectors cannot starve later ports. Firmware never forwards restore data.

External-host orientation and current advertisement are measured on mux channel 5 while retaining both FUSB302 Rd terminations. A stable orientation sets only expander P2. BMC activity cannot refresh an allowance. Sampling is scheduled every 10 ms; a separate 5 ms task withdraws the direct host requests when the last valid observation reaches 50 ms, even if the I²C task is recovering a stuck branch. USB reset, deconfiguration and suspension withdraw host allocation. This timing follows the 60 ms Type-C current-adjustment bound; actual GPIO and current waveforms still need measurement.

Without a brick, Hub and HubPD use `HOST_TARGET_5V` through the AP22653 aggregate limiter and each target's TPS259470. Native USB-C Rp supplies a measured 1.5 A or 3 A allowance. Default Rp on a legacy A-to-C connection does not identify a Pi's larger budget: the host must declare its remaining allowance, including Iasis, with `host-budget <milliamps>`. The hint is at most 1600 mA, clears on USB reset, and never overrides native Rp. Subtract other USB devices and the Pi 5 fan before declaring it.

The policy reserves 800 mA for board/UI/cable power (3.5 W at 4.5 V, rounded up), then selects an aggregate upper threshold of 170, 550, 1450 or 1890 mA. These are hardware protection thresholds, not per-target promises. A 1200 mA host allowance selects 170 mA; 1500 selects 550 mA; 3000 selects 1890 mA. Five attached targets share that presence supply. A generic 900 mA allowance has insufficient margin under this reservation and admits none. The policy needs no charging-stage I²C access, so it works with the Hub population's absent converters.

Host presence advertises one fixed 5 V / 0 mA PDO to request signalling without charging. It rejects a nonzero operating-current request. This does not establish zero actual target draw before or without a PD contract: the aggregate limiter and per-port protection remain responsible for the electrical bound. Charged-target restore behaviour has upstream precedent, but the fixed-PDO policy, target battery use and five-target current draw require Iasis validation. Host connector voltage must stay within the nominal 4.4–5.35 V presence window.

Brick operation offers one fixed 5 V / 500 mA PDO. It requires a confirmed CH224 PD contract, asserted power-good, valid fault inputs and an HV bus between 7 and 21 V. The policy uses 80% of input power, reserves 3.5 W for logic and 32 W for an already-powered side board, then reserves 6 W per target for its nominal 1 A stage limit and losses. A 9 V / 3 A input admits three attached targets; 20 V / 3 A admits five without CM5 power. Firmware leaves input voltage selection unchanged. Active host-powered attachments retain their branch when a brick arrives; it does not switch their source during a restore.

After detecting Rd and obtaining a grant, the brick path disables and discharges the isolated converter for 250 ms, programs and reads back 5.10 V and the 1 A limit, and waits 20 ms for settling. This overlaps the 150 ms CC debounce. Both source paths recheck Rd and safe-zero before closing the selected gate. PD begins only after connector voltage is valid (4.85–5.35 V for the regulated brick path). Missing ADC data, protection faults, bus errors and voltage failures remove source requests. Detach and disable open CC and discharge the connector; a fresh safe-zero observation is required before reconnecting.

Requests beyond the advertised PDO are rejected. The implementation handles Source_Capabilities, Request/Accept/PS_RDY, soft reset and rejection of role swaps. A hard reset recycles that port through full disconnect and safe-zero checks. Higher-voltage charging, VCONN/cable discovery and shared-capacity renegotiation are not implemented.

DFU signalling sends the attributed Apple Silicon or T2 action over SOP-double-prime Debug after a debug probe. Reboot sends its action directly. Each is one logical attempt with the FUSB302’s bounded retries; no automatic cable re-priming or repeated action loop is implemented. Apple actions may execute without GoodCRC, so status distinguishes acknowledged transport, an unconfirmed attempt and a transmission failure. USB enumeration and target behaviour establish the result. The host command includes the last attachment generation; stale commands are rejected.

Nominal divider calculations, packet framing/CRC, PDO/RDO rules, attachment admission, button gestures and LED brightness limits are checked without hardware. On-board validation still needs ADC calibration, stage/discharge waveforms, CC timing, protection fault injection and PD/Apple interoperability with real targets. These measurements do not gate building the firmware or host.

## Button and feedback

The active-low button is GPIO21, debounced for 30 ms. One click (400 ms gesture window) sends Apple Silicon DFU to ready ports; a 1.2 s hold sends reboot. Two clicks followed by a hold publish a restore request with the selected port mask and attachment generations. Only a running host command can act on that request; firmware never erases devices. T2 signalling is available through the CLI.

GPIO20 drives the six SK6812SIDE-A pixels through PIO/DMA, in board order: status, then ports 5 through 1. Green means ready or restore reported, blue means preparation/activity, amber means waiting or unconfirmed, and red means fault/failure. RGB sums stay below the 85/255 brightness reservation. Frames include over 200 µs reset time. Host feedback is per port and generation; a missing five-second heartbeat changes active feedback to Unconfirmed without changing power.

## Board interfaces

| Function              | Connection                                            |
| --------------------- | ----------------------------------------------------- |
| USB management        | USB7206C internal downstream USB2 port 6              |
| I²C root              | GPIO24 SDA, GPIO25 SCL; 400 kHz, external pull-ups    |
| TCA6408A              | Root address `0x20`, reset GPIO32                     |
| TCA9548A              | Root address `0x70`, `AON_RESET_N AND MUX_RUN`, GPIO1 |
| Target FUSB302        | Mux channels 0–4, address `0x22`                      |
| External-host FUSB302 | Mux channel 5, address `0x22`                         |
| Hub configuration     | Mux channel 6, left deselected                        |
| CH224                 | Mux channel 7, address `0x22`                         |
| Port VBUS ADC         | GPIO40, 41, 42, 43, 45, in physical port order        |
| HV bus ADC            | GPIO44                                                |
| Thermistors A, B      | GPIO47, GPIO46                                        |

Every I²C transaction has a 20 ms deadline. A timeout pulses GPIO1 low for 10 µs, resets the MCU I²C controller and verifies access to the deselected mux. Exposed downstream branches are quarantined for this boot. The affected target is switched off; other ports can continue. The root expander is not reset: hub run, host selection and external-host orientation are preserved. A root fault that cannot recover disables all target requests and leaves USB/ADC diagnostics running. Status reports timeouts, recoveries and the quarantined-channel mask. Physical RESET still resets the whole board and clears quarantine.

GPIO1 replaces UART RX. The debug header is NC/GND/TX; GPIO0 remains UART TX. GPIO32 still independently controls expander reset. The mux AND gate retains AON reset precedence, with a 4.7k GPIO1 pull-down covering RP2350 A2 input leakage.

The hub branch is not probed. The hub boots without seeing the root bus pull-ups, so its default USB path can work without MCU configuration. Enabling its SMBus interface requires a deliberate hub-reset/configuration sequence; selecting channel 6 after boot is not sufficient.

The image-section placement in `memory.x` is adapted from [Embassy's RP235x example](https://github.com/embassy-rs/embassy/blob/main/examples/rp235x/memory.x), under Apache-2.0. Embassy, Raspberry Pi's ROM source and the manufacturer register descriptions inform the implementation; no RestoreKit source is vendored.
