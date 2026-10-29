# iasis

Iasis is a five-port USB-C hub for putting Apple devices into DFU mode and restoring them. A Mac or Linux computer does the restore work; Iasis handles the connections, port power and DFU signalling. A separate Raspberry Pi Compute Module 5 board is planned as an onboard restore host.

It's still a prototype. The Rust firmware and host CLI cover port control, DFU signalling, device discovery and headless restores, but the hardware and five simultaneous restores haven't been tested end to end. Higher-voltage charging and automatic CM5 startup aren't implemented yet.

## Hardware

The main six-layer PCB has three assembly options:

- **Hub:** USB-powered, with limited 5 V power for connected devices.
- **HubPD:** adds a USB-C power input. With a power supply connected, the hardware is designed for 5, 9, 15 or 20 V output at the target ports, though current firmware only uses 5 V. Without a brick, it behaves like Hub.
- **CM5:** HubPD with a separate Compute Module 5 board connected through a board-to-board header. It needs a power supply that can provide 20 V.

The MCU is an RP2354, connected to an internal USB 2 hub port. The host cable carries both management commands and RP2354 ROM updates. BOOTSEL and RESET buttons provide recovery access, with SWD and a 3.3 V UART TX pad for debugging. The display is optional.

The CM5 assembly option on the main board includes its connector and protected power feed; the CM5 carrier is a separate PCB. Power from a USB host is limited, so running five connected devices without a separate supply remains something to test.

The hardware lives in [the main board](hardware/boards/iasis/README.md), [CM5 board](hardware/boards/cm5/README.md) and [case](hardware/case/README.md). Custom KiCad parts are in `hardware/shared`, and [board tools](hardware/tools/README.md) handle exports and checks.

## Development

Install the tools and dependencies:

```sh
mise install
mise run deps
```

| Task               | What it does                                                           |
| ------------------ | ---------------------------------------------------------------------- |
| `mise run build`   | Builds the CLI, firmware, cases, STEP models and PCB fabrication files |
| `mise run preview` | Updates the checked-in board and case images                           |
| `mise run lint`    | Runs software lint, KiCad DRC/ERC and schematic parity checks          |
| `mise run test`    | Runs software, placement-conversion and enclosure-fit tests            |
| `mise run format`  | Formats Rust, Python, Markdown and configuration files                 |

Tasks can also be run individually, such as `mise run //hardware/case:build`. Builds, previews and checks are separate; building doesn't automatically refresh the pictures or run DRC.

Use KiCad 10 with its standard libraries. The tasks find `kicad-cli` on `PATH` (including its usual macOS app location), or you can set `KICAD_CLI`. For a non-standard installation, add its path to the ignored `.mise/config.local.toml`. The board scripts don't need KiCad's Python bindings.

Python tools use the root `pyproject.toml`, `uv.lock` and `.venv`. Open the repository root in your editor and select that environment. `mise run python:lint` runs Ruff and Pyright on the case and board tools.

`mise install` also sets up Rustfmt, Clippy, the firmware target and Lefthook. The hooks extend `hydazz/.github` to format staged files, lint Python and workflows, and update changed mise locks. CAD checks run through mise.

## CLI

The Cargo workspace contains the RP2354 firmware (`iasis-firmware`), PD handling (`iasis-pd`), shared USB types (`iasis-protocol`) and host library/CLI (`iasis`). Firmware targets ARM; the other crates build on macOS and Linux.

```sh
mise run firmware:build
mise run host:build
mise run software:lint
mise run software:test

cargo run -p iasis -- list
cargo run -p iasis -- --serial 0123456789abcdef status --watch
cargo run -p iasis -- --serial 0123456789abcdef ports --watch
cargo run -p iasis -- --serial 0123456789abcdef power 1 off
cargo run -p iasis -- --serial 0123456789abcdef power 1 on
cargo run -p iasis -- --serial 0123456789abcdef dfu 1
```

`status --json` gives machine-readable status. Ports start enabled for attachment. Turning a port off disables it; turning it back on also retries a fault. The controller can send Apple Silicon DFU, T2 DFU (`dfu <port> --device t2`) or reboot commands. Wait until the port is `Ready` or `Contract` before sending one.

A PD acknowledgement only confirms the message was received, not that the device entered DFU. Check USB enumeration to confirm. The button uses a click for DFU, a hold for reboot, and two clicks followed by a hold to request a restore from a listening host. A DFU command by itself never erases or restores a device.

### Power from the host

Hub and HubPD without a separate supply use the upstream 5 V connection. Firmware reserves power for the board before sharing what's left between attached targets; it doesn't reserve power for empty ports. The protected host path supplies device presence, not charging. Bad or missing measurements are reported as errors, and CM5 power starts off.

A native USB-C connection uses the host's advertised current. For a legacy A-to-C connection, you can explicitly declare the _remaining_ USB allowance in mA:

```sh
iasis --serial 0123456789abcdef host-budget 1200
```

For example, a dedicated Pi 4 has up to 1200 mA shared USB allowance. A Pi 5 with its recommended 5 A supply has 1600 mA shared with USB devices and its fan; declaring 1500 mA leaves 100 mA for the fan, assuming nothing else is connected. Subtract other loads before setting a budget. USB-C current advertisement always takes precedence. The [firmware README](crates/firmware/README.md) has the power limits and sequencing details.

### Finding devices

`ports` relates USB 2 and SuperSpeed devices back to physical ports 1–5 using Linux sysfs or macOS IOKit. It reads ECIDs from DFU/recovery descriptors or read-only queries through the system `usbmuxd`. A tracked device keeps its physical port and identity across USB re-enumeration. Missing identity, a moved device and a replacement are reported separately; disappearing from the USB tree doesn't necessarily mean the cable was unplugged.

### Restoring devices

Install [idevicerestore](https://github.com/libimobiledevice/idevicerestore) and its libimobiledevice/libirecovery dependencies. Iasis uses the normal system `usbmuxd`: macOS includes it, while Linux needs the service and USB permissions configured. The CLI checks the required idevicerestore options and daemon before starting a restore. It doesn't run its own daemon or detach USB kernel drivers.

With a local IPSW:

```sh
iasis ipsw /path/to/UniversalMac.ipsw
iasis --serial 0123456789abcdef restore /path/to/UniversalMac.ipsw --ports 1,2,3,4,5 --erase
iasis --serial 0123456789abcdef restore /path/to/UniversalMac.ipsw --button --erase --host-ma 1500
```

**`--erase` authorizes erasing the selected devices.** Button mode waits for a _new_ restore gesture after the command is ready, then exits when that batch finishes. It won't act on old or missed button events. Ports and ECIDs are checked again before launching, and each worker selects its device by ECID. Controller identities aren't restore targets. Overlapping commands can't claim the same connector. You can still use power, DFU and reboot controls while a batch is running.

The CLI checks the IPSW ZIP and BuildManifest locally. It doesn't download IPSWs or check whether Apple is currently signing them; idevicerestore handles device compatibility and signing. Up to five processes can use the same archive, with separate working directories, caches and logs under `restores/` (or `--logs`). Some compressed OS images need an extracted copy per worker, so allow for the extra disk space.

Progress and outcomes are sent back to the port LEDs. A zero exit code alone is `Unconfirmed`; `RestoreReported` also requires idevicerestore's final success status. Neither proves the device has booted or that erasure was verified. If feedback stops for five seconds, the LED reports `Unconfirmed` without cycling power. There are no automatic retries, background service or GUI yet. Five real concurrent restores on both host platforms still need testing.

## Firmware updates and diagnostics

`iasis bootloader --serial <serial>` asks the RP2354 to enter its ROM updater; see [firmware](crates/firmware/README.md) for flashing and recovery. Status includes raw ADC readings, faults, CH224 input status and host CC/orientation. ADC readings aren't calibrated, and the CH224's negotiated current is a limit, not measured current draw.

Development firmware uses the [pid.codes experimental VID/PID](https://pid.codes/1209/0001/), `1209:0001`. Get an assigned identity before distributing it. To change IDs, set `IASIS_USB_VID` and `IASIS_USB_PID` as hex values at firmware build time and pass matching `--vid`/`--pid` options to the CLI. Discovery also checks the product string and protocol header. Linux needs read/write access to the USB device.

## Fabrication

```sh
mise run //hardware/boards/iasis:fab
```

The main board's output is in `build/boards/iasis/`: `gerbers.zip` and per-variant JLCPCB BOM/CPL files (`bom-Hub.csv`, `cpl-Hub.csv`, etc.). The [main board README](hardware/boards/iasis/README.md) has the PCB and assembly order settings. The [CM5 board](hardware/boards/cm5/README.md) has its own exports; the [case README](hardware/case/README.md) covers printing and assembly.

## CI and versions

CI runs workflow lint, software checks on Linux/macOS, hardware checks and a final Build Success job. Hardware CI uses a pinned KiCad 10 image for DRC/ERC, schematic parity, STEP exports, enclosure fit and fabrication files. The `build/` artifact lasts 14 days, including diagnostic reports on failure. DC workload tests, the routing census, USB routing review and previews currently run locally.

Renovate extends `hydazz/renovate-config`; Embassy dependencies update together. The shared preset controls schedules and automerge. Require Build Success on `main` before enabling automerge.

All Rust crates share the version in `Cargo.toml`; software releases use `vX.Y.Z` tags. The USB wire protocol is versioned separately. Each board has its own KiCad revision, independent of the software version. Tag a fabrication commit as `iasis-rev-a` or `cm5-rev-a` for Revision A, and keep its outputs as a permanent record. CI artifacts are only temporary snapshots. A first release will also add retained fabrication packages and Release Please for software versions.

## License

Original code, firmware, CAD, documentation and enclosure files are [Apache-2.0](LICENSE). Third-party assets keep their own licences and notices.
