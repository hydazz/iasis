# Board tools

Shared scripts for checking KiCad projects, exporting fabrication files and making board previews. Each PCB keeps its own project and `.mise.toml`; these tools work across the boards.

Install the [root dependencies](../../README.md#development), including KiCad 10 and its standard symbols, footprints and 3D models. The scripts use the root uv environment and call `kicad-cli` from `PATH`, or from `KICAD_CLI` if set. They don't need KiCad's Python bindings. Custom and modified KiCad parts live in `hardware/shared` with their licence notices.

## Run a board task

```sh
mise run //hardware/boards/iasis:fab
mise run //hardware/boards/cm5:step
mise run //hardware/boards/iasis:preview
mise run //hardware/boards/cm5:lint
```

| Task      | Output                                                                                        |
| --------- | --------------------------------------------------------------------------------------------- |
| `fab`     | `build/boards/BOARD/gerbers.zip`, BOM/CPL files, `bottom.svg` and a copy of the board/project |
| `step`    | `build/boards/BOARD/board.step` (Iasis also exports `board-cm5.step` for the case)            |
| `preview` | `board.png` in the board folder                                                               |
| `lint`    | `build/boards/BOARD/drc.json` and `erc.json`                                                  |

Exports use the saved design and don't alter the source. `lint` refills zones in memory, then runs KiCad DRC, schematic parity checks and ERC. It writes both reports even if a check fails, and exits nonzero on errors. Exporting or previewing doesn't run those checks.

Previews use KiCad's top view with automatic framing, then trim the transparent border to 24 pixels.

## JLCPCB exports

Copper layers come from the PCB. Assembly populations come from KiCad variants: projects without named variants get `bom.csv` and `cpl.csv`; named ones get `bom-VARIANT.csv` and `cpl-VARIANT.csv`.

The BOM groups populated parts by LCSC code and footprint. The CPL only includes parts from the BOM.

`jlc_corrections.csv` adjusts placements when KiCad and JLCPCB's EasyEDA footprints use different rotations or origins. Each correction is keyed by _both_ LCSC part and KiCad footprint:

- `rotation` is a counter-clockwise offset to add.
- `x` and `y` give the EasyEDA origin relative to the unrotated KiCad footprint, in mm with Y pointing down. The converter rotates that offset into the final placement.

Existing corrections are for top-side assembly. If one of those parts moves to the bottom, measure it again. Check new parts in JLCPCB's placement viewer before ordering.

Pass `--output DIR` to choose an export directory (relative to the board folder when called through mise). To bypass cached outputs, use `mise run --force //hardware/boards/BOARD:TASK -- --output DIR`.
