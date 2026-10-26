# iasis

Five-port USB-C hub with programmable DFU signalling for Apple devices.

The host connects to a USB 3 hub and an RP2354 management controller. Restore traffic passes through the hub; the controller handles port orientation, power and DFU commands. A Mac can remain the restore host. A CM5 build adds a side board with a Raspberry Pi Compute Module 5 as the host, using the same USB control interface.

One six-layer PCB supports three assembly variants:

- **Hub:** host-powered logic and limited 5 V target presence supplies.
- **HubPD:** the same hub with a USB-C power input. A connected power supply runs the whole board and its power is shared across the target ports at 5, 9, 15 or 20 V, so a lone device can take most of it. Without one, HubPD works like Hub.
- **CM5:** HubPD with the side-link connector and protection fitted. The separate CM5 board joins through a short flex and needs a power supply that offers 20 V.

The CM5 BOM and placements cover the main board; the daughterboard is a separate design. Without a power supply, the available upstream USB power limits the combined target supply; five simultaneous restores are a prototype test goal.

The MCU connects to an internal USB2 hub port, so the same host cable supports management and RP2354 ROM programming. BOOTSEL and RESET buttons provide recovery without soldering; SWD and 3.3 V UART solder pads provide independent debugging. The optional display is independent of core operation. DFU, revive and erase are separate deliberate actions.

In development. Diagnostic firmware and a Rust host library/CLI are available. Target power, PD, DFU and restore orchestration are not implemented. Hardware and restore workflows are not yet validated.

## Development

The main board is in `hardware/boards/iasis`, the CM5 side board in `hardware/boards/cm5`, and the joining flex in `hardware/boards/flex`. Shared libraries are in `hardware/shared`, board tools in `hardware/tools`, and the enclosure in `hardware/case`.

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

The Cargo workspace contains `iasis-firmware` (Embassy on RP2354B), `iasis-protocol` (the shared, allocation-free wire format), and `iasis` (the host library and CLI). The host supports macOS and Linux. The default Cargo members are the host and protocol; firmware uses its own ARM target.

```bash
mise run firmware:build
mise run host:build
mise run software:check
cargo run -p iasis -- list
cargo run -p iasis -- --serial 0123456789abcdef status --json
cargo run -p iasis -- --serial 0123456789abcdef status --watch
```

Firmware keeps all ten target power requests off and leaves CM5 power off at cold boot. It samples the eight ADC inputs, fault pins, expander inputs, FUSB302 identities and available CH224 status. Missing readings are reported as unavailable. ADC values are raw counts; CH224 current is the negotiated ceiling, not measured consumption. No button gesture starts a DFU or restore operation yet.

`iasis bootloader --serial <serial>` requests the controller's ROM updater. See [firmware](crates/firmware/README.md) for loading, recovery and retained power behaviour. USB bus and port paths are reported for inspection; mapping Apple devices across USB2/USB3 re-enumeration is not implemented.

Development builds use the [pid.codes private experimentation ID](https://pid.codes/1209/0001/), `1209:0001`. Obtain an assigned identity before distributing firmware. Set both `IASIS_USB_VID` and `IASIS_USB_PID` as hexadecimal values when building firmware, then pass matching `--vid` and `--pid` to the CLI. Discovery also checks the product string and protocol header. Linux requires read/write permission on the matching USB device; no kernel driver is detached automatically.

## Fabrication

Install the development dependencies above, then export a board:

```bash
mise run //hardware/boards/iasis:fab
```

This writes `build/boards/iasis/`: `gerbers.zip` (Gerbers and drill files for JLCPCB), and `bom-Hub.csv`, `bom-HubPD.csv`, `bom-CM5.csv` and their matching `cpl-*.csv` files for JLCPCB assembly of each variant, with placements corrected for JLCPCB's footprints. [hardware/boards/iasis](hardware/boards/iasis/README.md) lists the main board's JLCPCB order options.

CM5 and flex have the same `fab`, `step`, `preview` and `check` tasks. CM5 exports one assembly population; flex exports bare-board fabrication files including its stiffener drawing. See [board tooling](hardware/tools/README.md) for inputs and outputs and [the enclosure](hardware/case/README.md) for printing and assembly.

## License

Original software, firmware, documentation, CAD and enclosure sources are licensed under [Apache-2.0](LICENSE). Third-party material retains its original licenses and notices.
