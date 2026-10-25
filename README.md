# iasis

Five-port USB-C hub with programmable DFU signalling for Apple devices.

The host connects to a USB 3 hub and an RP2354 management controller. Restore traffic passes through the hub; the controller handles port orientation, power and DFU commands. A Mac can remain the restore host. A CM5 build adds a side board with a Raspberry Pi Compute Module 5 as the host, using the same USB control interface.

One six-layer PCB supports three assembly variants:

- **Hub:** host-powered logic and limited 5 V target presence supplies.
- **HubPD:** the same hub with a USB-C power input. A connected power supply runs the whole board and its power is shared across the target ports at 5, 9, 15 or 20 V, so a lone device can take most of it. Without one, HubPD works like Hub.
- **CM5:** HubPD with the side-link connector and protection fitted. The separate CM5 board joins through a short flex and needs a power supply that offers 20 V.

The CM5 BOM and placements cover the main board; the daughterboard is a separate design. Without a power supply, the available upstream USB power limits the combined target supply; five simultaneous restores are a prototype test goal.

The MCU connects to an internal USB2 hub port, so the same host cable supports management and RP2354 ROM programming. BOOTSEL and RESET buttons provide recovery without soldering; SWD and 3.3 V UART solder pads provide independent debugging. The optional display is independent of core operation. DFU, revive and erase are separate deliberate actions.

In development. Firmware and host software are not yet implemented. Hardware and restore workflows are not yet validated.

## Development

The main board is in `hardware/boards/iasis`, the CM5 side board in `hardware/boards/cm5`, and the joining flex in `hardware/boards/flex`. Shared libraries are in `hardware/shared`, board tools in `hardware/tools`, and the enclosure in `hardware/case`.

```bash
mise install
mise run deps
```

| Task               | Output                                                                                    |
| ------------------ | ----------------------------------------------------------------------------------------- |
| `mise run build`   | Case STLs, assembly STEP and PCB fabrication files                                        |
| `mise run preview` | Tracked board and case images                                                             |
| `mise run check`   | Source checks, placement-conversion tests, case fit, and every board's DRC/parity and ERC |
| `mise run format`  | Formatted Python, Markdown and configuration                                              |

Builds, previews and checks are independent. Component tasks can be run directly, for example `mise run //hardware/case:build`.

`mise run typecheck` checks the case package with its uv environment and the board tools with KiCad's bundled Python. It is included in `mise run check`.

`mise install` installs the local Lefthook pre-commit hook. It formats staged Python, Markdown and configuration files, lints Python, and refreshes the mise lock when tool configuration changes. CAD checks run through mise when requested.

## Fabrication

Tools come from mise (`mise install` at the repository root); KiCad 10 is needed at the path `KICAD_CLI` names in `.mise/config.toml`.

```bash
mise run //hardware/boards/iasis:fab
```

This writes `build/boards/iasis/`: `gerbers.zip` (Gerbers and drill files for JLCPCB), and `bom-Hub.csv`, `bom-HubPD.csv`, `bom-CM5.csv` and their matching `cpl-*.csv` files for JLCPCB assembly of each variant, with placements corrected for JLCPCB's footprints. [hardware/boards/iasis](hardware/boards/iasis/README.md) lists the main board's JLCPCB order options.

CM5 and flex have the same `fab`, `step`, `preview` and `check` tasks. CM5 exports one assembly population; flex exports bare-board fabrication files including its stiffener drawing. See [board tooling](hardware/tools/README.md) for inputs and outputs and [the enclosure](hardware/case/README.md) for printing and assembly.

## License

Original software, firmware, documentation, CAD and enclosure sources are licensed under [Apache-2.0](LICENSE). Third-party material retains its original licenses and notices.
