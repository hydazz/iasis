# iasis

Five-port USB-C hub with programmable DFU signalling for Apple devices.

The host connects to a USB 3 hub and an RP2354 management controller. Restore traffic passes through the hub; the controller handles port orientation, power and DFU commands. A Mac can remain the restore host. A CM5 build adds a side board with a Raspberry Pi Compute Module 5 as the host, using the same USB control interface.

One six-layer PCB supports three assembly variants:

- **Hub:** host-powered logic and limited 5 V target presence supplies.
- **HubPD:** the same hub with a USB-C power input. A connected power supply runs the whole board and its power is shared across the target ports at 5, 9, 15 or 20 V, so a lone device can take most of it. Without one, HubPD works like Hub.
- **CM5:** HubPD with a separate CM5 board, joined by a coplanar header and socket. HubPD includes the connector and protected power feed. The CM5 build needs a power supply that offers 20 V.

The CM5 BOM and placements cover the main board; the daughterboard is a separate design. Without a power supply, the available upstream USB power limits the combined target supply; five simultaneous restores are a prototype test goal.

The MCU connects to an internal USB2 hub port, so the same host cable supports management and RP2354 ROM programming. BOOTSEL and RESET buttons provide recovery without soldering; SWD and 3.3 V UART solder pads provide independent debugging. The optional display is independent of core operation. DFU, revive and erase are separate deliberate actions.

In development. Rust firmware and a host library/CLI provide telemetry, fixed-5 V port control, PD and Apple DFU/reboot signalling. Hardware operation is not yet validated. Higher-voltage charging, host-powered target supplies and restore orchestration remain unimplemented.

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

Ports start disabled. `power <port> on` enables a physical port when the confirmed brick contract has enough reserved power. Each port detects attachment and orientation independently, prepares its TPS55288 stage, checks VBUS and advertises one 5 V / 500 mA PDO. Insufficient supply leaves the port waiting; missing or unsafe readings leave it faulted. Status exposes both conditions. The current policy uses the brick's existing contract; it does not negotiate a higher input voltage. CM5 power remains off at cold boot.

`dfu <port>` sends Apple Silicon signalling; `--device t2` selects T2. `reboot <port>` sends the reboot VDM. These require an enabled, attached port with valid 5 V power; wait for `Ready` or `Contract` in status before signalling. A PD acknowledgement confirms transport only. An unacknowledged attempt has an unknown effect: Apple devices can act without GoodCRC. Observe USB enumeration to confirm DFU or reboot. Neither command starts a restore. Button gestures are not implemented.

`ports` groups USB2 devices beneath hub ports 1–5, anchored by the management controller on port 6. It follows physical paths through USB2 re-enumeration and nested hubs. An empty entry means no USB2 enumeration, not cable detachment. SuperSpeed companion mapping and persistent ECID correlation are not implemented.

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
