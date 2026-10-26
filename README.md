# iasis

Five-port USB-C hub with programmable DFU signalling for Apple devices.

The host connects to a USB 3 hub and an RP2354 management controller. Restore traffic passes through the hub; the controller handles port orientation, power and DFU commands. A Mac can remain the restore host. A CM5 build adds a side board with a Raspberry Pi Compute Module 5 as the host, using the same USB control interface.

One six-layer PCB supports three assembly variants:

- **Hub:** host-powered logic and limited 5 V target presence supplies.
- **HubPD:** the same hub with a USB-C power input. A connected power supply runs the whole board and its power is shared across the target ports at 5, 9, 15 or 20 V, so a lone device can take most of it. Without one, HubPD works like Hub.
- **CM5:** HubPD with a separate CM5 board, joined by a coplanar header and socket. HubPD includes the connector and protected power feed. The CM5 build needs a power supply that offers 20 V.

The CM5 BOM and placements cover the main board; the daughterboard is a separate design. Without a power supply, the available upstream USB power limits the combined target supply; five simultaneous restores are a prototype test goal.

The MCU connects to an internal USB2 hub port, so the same host cable supports management and RP2354 ROM programming. BOOTSEL and RESET buttons provide recovery without soldering; SWD and 3.3 V UART TX solder pads provide independent debugging. The optional display is independent of core operation. DFU, revive and erase are separate deliberate actions.

In development. Rust firmware and a macOS/Linux CLI provide host-powered attachment, fixed-5 V brick power, PD/Apple signalling, physical-port discovery and a headless restore workflow. Hardware operation and simultaneous restores are not yet validated. Higher-voltage charging and automatic CM5 cold start remain unimplemented.

## Development

The main board is in `hardware/boards/iasis` and the CM5 side board in `hardware/boards/cm5`. Shared libraries are in `hardware/shared`, board tools in `hardware/tools`, and the enclosure in `hardware/case`.

```bash
mise install
mise run deps
```

| Task               | Output                                                                                    |
| ------------------ | ----------------------------------------------------------------------------------------- |
| `mise run build`   | Host CLI, firmware ELF, case STLs, assembly STEP and PCB fabrication files                |
| `mise run preview` | Tracked board and case images                                                             |
| `mise run check`   | Source checks, placement-conversion tests, case fit, and every board's DRC/parity and ERC |
| `mise run format`  | Formatted Rust, Python, Markdown and configuration                                        |

Builds, previews and checks are independent. Component tasks can be run directly, for example `mise run //hardware/case:build`.

Python dependencies and commands use the root `pyproject.toml`, `uv.lock` and `.venv`. Open the repository root in your editor and select that environment. `mise run typecheck` checks the case and board tools together and is included in `mise run check`.

Install KiCad 10 with its standard libraries. Tasks invoke `kicad-cli` through `PATH`; the mise configuration includes the usual macOS application directory. For another installation, set `_.path` in the ignored `.mise/config.local.toml`, or set `KICAD_CLI` to the executable. Board scripts use ordinary Python and do not require KiCad's Python bindings.

`mise install` installs the local Lefthook pre-commit hook. It formats staged Rust, Python, Markdown and configuration files, lints Python, and refreshes the mise lock when tool configuration changes. CAD checks run through mise when requested.

## Software

The Cargo workspace contains `iasis-firmware` (Embassy on RP2354B), `iasis-pd` (PD messages and power policy), `iasis-protocol` (the shared wire format), and `iasis` (the host library and CLI). The host supports macOS and Linux. Firmware uses its own ARM target; the other crates build and test on the host.

```bash
mise run firmware:build
mise run host:build
mise run software:check
cargo run -p iasis -- list
cargo run -p iasis -- --serial 0123456789abcdef status --json
cargo run -p iasis -- --serial 0123456789abcdef status --watch
cargo run -p iasis -- --serial 0123456789abcdef power 1 on
cargo run -p iasis -- --serial 0123456789abcdef dfu 1
cargo run -p iasis -- --serial 0123456789abcdef ports --watch
cargo run -p iasis -- --serial 0123456789abcdef power 1 off
```

Ports start enabled for attachment. Native USB-C current advertisement or an explicit legacy-host allowance determines the protected host-presence budget. Hub and HubPD without a brick use the same 5 V path; charging converters are not required. `power <port> off` disables a port, and `on` retries a fault. The fixed-5 V brick path reserves power only for detected attachments. Missing or unsafe readings remain visible as errors. CM5 power stays off at cold boot.

On a dedicated Pi 4, declare up to its 1200 mA shared USB allowance after subtracting other loads. A Pi 5 with its recommended 5 A supply has 1600 mA shared with USB and its fan; for example, 1500 mA reserves 100 mA for the fan when no other USB loads exist. These are deployment declarations, not values inferred from an A-to-C cable. Native USB-C Rp takes precedence. The firmware reserves board/UI power before choosing its aggregate limiter; host presence requests no charging and depends on charged targets. See [firmware](crates/firmware/README.md) for limits and sequencing.

```sh
iasis --serial 0123456789abcdef host-budget 1200
```

`dfu <port>` sends Apple Silicon signalling; `--device t2` selects T2. `reboot <port>` sends reboot. Wait for `Ready` or `Contract` before signalling. A PD acknowledgement confirms transport only; USB enumeration confirms DFU. Neither command starts a restore. The physical button sends DFU with one click and reboot with a hold. Two clicks followed by a hold request a host restore.

`ports` maps USB2 and SuperSpeed descendants to physical ports 1–5. Companion links come from Linux sysfs or macOS IOKit; missing or contradictory links remain explicit. ECID comes from DFU/recovery descriptors or read-only restored/lockdown queries through system usbmuxd. A tracked target keeps its board, connector and ECID through re-enumeration. Missing identity, movement and replacement are separate outcomes; an empty USB listing does not prove cable detachment.

### Headless restores

Install upstream [idevicerestore](https://github.com/libimobiledevice/idevicerestore) with its libimobiledevice/libirecovery dependencies. Use the normal system usbmuxd service: macOS provides it; Linux needs the service running and suitable USB device permissions. Iasis checks the executable's required options and daemon availability before target actions. It does not start a private daemon or detach kernel drivers.

Prepare a local IPSW, then select physical ports or wait for the button:

```sh
iasis ipsw /path/to/UniversalMac.ipsw
iasis --serial 0123456789abcdef restore /path/to/UniversalMac.ipsw --ports 1,2,3,4,5 --erase
iasis --serial 0123456789abcdef restore /path/to/UniversalMac.ipsw --button --erase --host-ma 1500
```

**`--erase` authorizes erasing the selected targets.** Button mode waits for a new Restore gesture after preparation and exits when that batch finishes. Existing or missed events do not trigger a restore. Selected attachments and ordinary DFU ECIDs are checked again immediately before launch; Port DFU controller identities cannot select a restore target. Concurrent restore commands claim individual board connectors, preventing overlapping batches on the same ports. Explicit power, DFU and reboot controls remain available during a batch.

Preparation streams the archive for ZIP CRC validation and checks its BuildManifest. It does not download firmware or establish current signing eligibility; idevicerestore checks target compatibility and Apple's signing response. One archive is shared by up to five concurrent ECID-selected processes on either OS. Each worker has a separate writable cache, working directory and stdout/stderr logs under `restores/` (override with `--logs`). Compressed OS images may need one extracted copy per worker; allow disk space accordingly.

The foreground command monitors each worker independently and sends stage progress and outcomes to the RP2354 LEDs. Exit zero alone is Unconfirmed; RestoreReported requires the backend's final restore status too. It does not independently establish that the OS booted or verify erasure. Losing feedback marks the port Unconfirmed after five seconds without switching its power. No automatic retries, native GUI or background service are included. Five real simultaneous restores on macOS and Linux remain validation goals; tests exercise five concurrent child processes, identity transitions, archive integrity and process outcomes without attached hardware.

Telemetry includes eight ADC inputs, fault pins, expander inputs, CH224 status and external-host CC/orientation. Stable upstream CC measurements set the external-host USB3 orientation without changing hub run or host selection. ADC values are raw counts; firmware voltage checks use the nominal divider ratio. CH224 current is the negotiated ceiling, not measured consumption.

`iasis bootloader --serial <serial>` requests the controller's ROM updater. See [firmware](crates/firmware/README.md) for loading, power sequencing and recovery limits.

Development builds use the [pid.codes private experimentation ID](https://pid.codes/1209/0001/), `1209:0001`. Obtain an assigned identity before distributing firmware. Set both `IASIS_USB_VID` and `IASIS_USB_PID` as hexadecimal values when building firmware, then pass matching `--vid` and `--pid` to the CLI. Discovery also checks the product string and protocol header. Linux requires read/write permission on the matching USB device; no kernel driver is detached automatically.

## Fabrication

Install the development dependencies above, then export a board:

```bash
mise run //hardware/boards/iasis:fab
```

This writes `build/boards/iasis/`: `gerbers.zip` (Gerbers and drill files for JLCPCB), and `bom-Hub.csv`, `bom-HubPD.csv`, `bom-CM5.csv` and their matching `cpl-*.csv` files for JLCPCB assembly of each variant, with placements corrected for JLCPCB's footprints. [hardware/boards/iasis](hardware/boards/iasis/README.md) lists the main board's JLCPCB order options.

CM5 has the same `fab`, `step`, `preview` and `check` tasks and exports one assembly population. See [board tooling](hardware/tools/README.md) for inputs and outputs and [the enclosure](hardware/case/README.md) for printing and assembly.

## License

Original software, firmware, documentation, CAD and enclosure sources are licensed under [Apache-2.0](LICENSE). Third-party material retains its original licenses and notices.
