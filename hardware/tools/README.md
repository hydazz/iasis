# Board tooling

Board directories own their KiCad projects and `.mise.toml` settings. The shared tasks use KiCad 10's CLI and bundled Python (`pcbnew` and `wx`), selected by `KICAD_CLI` and `KICAD_PYTHON` in the root mise configuration. Install KiCad's standard symbols, footprints and 3D models with KiCad; project libraries in `hardware/shared` hold custom or modified parts and their notices.

Run `mise install` at the repository root, then select a board:

```bash
mise run //hardware/boards/iasis:fab
mise run //hardware/boards/cm5:step
mise run //hardware/boards/flex:preview
mise run //hardware/boards/cm5:check
```

| Task      | Output                                                                                                 |
| --------- | ------------------------------------------------------------------------------------------------------ |
| `fab`     | `build/boards/BOARD/gerbers.zip`, BOM/CPL files where assembled, `bottom.svg` and a board/project copy |
| `step`    | `build/boards/BOARD/board.step`; Iasis also exports `board-cm5.step` for the enclosure                 |
| `preview` | `preview/board.png` and `preview/layout.png` in the board directory                                    |
| `check`   | `build/boards/BOARD/drc.json` and `erc.json`; failures return a nonzero exit status                    |

Exports read the saved design. `check` refills zones in memory and runs native DRC with schematic parity and ERC, reporting errors. Both reports are produced even when a design check fails. Exports and previews do not run these checks or save changes to the source board.

Board photos use KiCad's native top view with automatic camera fit. Transparent space is trimmed to a 24-pixel margin, so wide and tall boards need no individual framing settings.

## Fabrication

Copper layers come from the board. Assembly populations come from the project's KiCad variants; a project with no named variants produces `bom.csv` and `cpl.csv`. Named variants produce `bom-VARIANT.csv` and `cpl-VARIANT.csv`. The BOM groups parts by LCSC code and footprint and excludes DNP parts; the CPL includes placements belonging to that BOM.

The board task selects non-copper fabrication layers. The flex uses `F.Mask` and `B.Mask` for coverlay openings and `User.1` for its stiffener drawing, with no assembly output. Its flat STEP is a fabrication view; the enclosure models the flex in its bent position.

`jlc_corrections.csv` matches a supplier part **and** a KiCad footprint. `rotation` is the counter-clockwise turn to add; `x` and `y` locate the EasyEDA origin in the unrotated KiCad footprint, in mm with Y down. The converter rotates that offset into the placement coordinates. Existing corrections cover top-side assembly; a corrected part moved to the bottom needs its placement measured before export. Confirm new parts in JLCPCB's placement viewer.

The Iasis task supplies its optional back-silkscreen logo slot. Pass `--logo PATH` to that task to apply artwork to the exported copy. See the [Iasis fabrication instructions](../boards/iasis/README.md).

`--output DIR` selects an export directory, relative to the board directory when invoked through mise. To force an export or preview despite existing cached outputs, use `mise run --force //hardware/boards/BOARD:TASK -- --output DIR`.

## Adding a board

Put its project in `hardware/boards/NAME` and add a `.mise.toml` extending the `pcb:fab`, `pcb:step`, `pcb:preview` and `pcb:check` templates. Use the CM5 configuration as the basic example and the flex for a bare board. `project:format` and `project:fmt-check` format the component's mise file. The root tasks discover board directories through mise's monorepo glob.

The scripts also accept an explicit PCB path. Running them in a directory with one `.kicad_pro` selects that project automatically. Board-specific variants, artwork dimensions and fabrication layers belong in the board's task settings.
