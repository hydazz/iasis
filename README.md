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

The current board and enclosure live in `hardware/lite/pcb` and `hardware/lite/case`.

```bash
mise install
mise run deps
```

| Task | Output |
| --- | --- |
| `mise run build` | Case STLs, assembly STEP and PCB fabrication files |
| `mise run preview` | Tracked board and case images |
| `mise run check` | Python lint/format checks, case fit checks, KiCad DRC and ERC |
| `mise run format` | Formatted Python sources |

Builds, previews and checks are independent. Component tasks can be run directly, for example `mise run //hardware/lite/case:build`.

## Fabrication

Tools come from mise (`mise install` at the repository root); KiCad 10 is needed at the path `KICAD_CLI` names in `.mise/config.toml`.

```bash
mise run //hardware/lite/pcb:fab
```

This writes `hardware/lite/pcb/build/`: `gerbers.zip` (gerbers and drill files for JLCPCB), and `bom-Hub.csv`, `bom-HubPD.csv`, `bom-CM5.csv` and their matching `cpl-*.csv` files for JLCPCB assembly of each variant, with placements corrected for JLCPCB's footprints. [hardware/lite/pcb](hardware/lite/pcb/README.md) lists the JLCPCB order options. The enclosure has its own tasks; see [hardware/lite/case](hardware/lite/case/README.md).

## License

Original software, firmware, documentation, CAD and enclosure sources are licensed under [Apache-2.0](LICENSE). Third-party material retains its original licenses and notices.
