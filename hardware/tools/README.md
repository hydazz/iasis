# Board tools

Export fabrication files, check KiCad designs and render board previews. Each board's `.mise.toml` uses the shared tasks in the root mise configuration.

## Run

Install the [development tools](../../README.md#build) and KiCad 10 with its standard libraries. Scripts use the root uv environment and find `kicad-cli` on `PATH`; set `KICAD_CLI` for another location.

Run from the repository root, substituting the board name:

```sh
mise run //hardware/boards/iasis:lint
mise run //hardware/boards/iasis:fab
```

| Task      | Output                                                                    |
| --------- | ------------------------------------------------------------------------- |
| `fab`     | Gerber/drill ZIP, BOM/CPL files, `bottom.svg`, board/project/rules copies |
| `step`    | Populated `board.step`                                                    |
| `preview` | `board.png` beside the source board                                       |
| `lint`    | `drc.json` and `erc.json`                                                 |

Outputs other than previews go to `build/boards/BOARD/`. `lint` checks DRC, schematic parity and ERC, refilling zones in memory. It writes both reports and fails on errors. Exports use the saved design and leave source files unchanged; run checks separately.

Pass `--output DIR` to choose another output directory. Relative paths resolve from the board directory under mise. To rebuild a cached STEP or preview, use `mise run --force //hardware/boards/iasis:step`.

## Assembly files

Copper layers come from the PCB; populations come from KiCad variants. The default gets `bom.csv` and `cpl.csv`; named variants get `bom-VARIANT.csv` and `cpl-VARIANT.csv`.

BOM rows are grouped by LCSC code. The CPL includes only parts in that BOM. `gerbers.zip` contains Gerbers and drills, excluding the optional `.gbrjob` metadata.

## Placement corrections

[`jlc_corrections.csv`](jlc_corrections.csv) maps differences between KiCad and JLCPCB/EasyEDA footprint origins. A correction matches both the LCSC code and KiCad footprint.

| Field      | Meaning                                                                        |
| ---------- | ------------------------------------------------------------------------------ |
| `rotation` | Counter-clockwise rotation to add, in degrees                                  |
| `x`, `y`   | EasyEDA origin relative to the unrotated KiCad footprint, in mm; Y points down |

The converter rotates the offset with the final placement. Measure corrections against numbered pads, then check them in JLCPCB's placement viewer. Existing corrections cover top-side assembly; remeasure a part moved to the bottom.
