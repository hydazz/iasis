# Iasis board

KiCad 10 project for the six-layer, 125 × 89 mm Iasis board. One layout carries two assembly variants, Hub and HubPD; the [repository README](../../../README.md) describes them.

<img src="preview/board.png" alt="Hub PD, top side" width="400"> <img src="preview/layout.png" alt="Tracks on every copper layer, pads and top silkscreen outlines" width="400">

## Files

- `iasis.kicad_sch` and its sheets, `iasis.kicad_pcb`, `iasis.kicad_pro`, and `iasis.kicad_dru` for the rules the board setup cannot express.
- `Iasis.kicad_sym`, `Iasis.pretty` and `Iasis.3dshapes`: project symbols, footprints and models. `NOTICE` files give their sources and licences.
- `jlc_cpl.py` corrects placements for JLCPCB (below); `preview.py` writes `preview/board.png` and `preview/layout.png`; `diff.py` shows a board change (below).

## Fab files

Tools come from mise (`mise install` at the repository root); KiCad 10 must be at the path `KICAD_CLI` names in `.mise/config.toml`.

```bash
mise run //hardware/lite/pcb:fab
```

The task writes `build/`:

| File                           | Contents                                                  |
| ------------------------------ | --------------------------------------------------------- |
| `gerbers.zip`                  | Gerber and drill files                                    |
| `bom-Hub.csv`, `bom-HubPD.csv` | Bill of materials per variant, one row per LCSC part      |
| `cpl-Hub.csv`, `cpl-HubPD.csv` | Placements per variant, corrected for JLCPCB's footprints |

Run `mise run //hardware/lite/pcb:preview` to refresh the tracked board and copper-layout images. Commit them with the design changes they show.

Run `mise run //hardware/lite/pcb:step` to export `build/board.step`, which the case tasks consume. Run `mise run //hardware/lite/pcb:check` for DRC with schematic parity and ERC errors; reports are written to `build/drc.json` and `build/erc.json`.

The upload ZIP contains Gerber and drill files. It excludes the optional Gerber job file, whose board-preview colour and impedance-control metadata differ from the order settings below.

The underside carries the name, repository URL and `REV ${REVISION}`, read from the board's title block. The PCB revision identifies a fabricated board design; tag its source commit (for example, `hw-rev-a`) and keep that release's fabrication files together. Hub and HubPD share the PCB revision.

### Logo

Supply a filled SVG to place a logo on the back silkscreen:

```bash
mise run //hardware/lite/pcb:fab -- --logo ../../../logos/logo.svg
```

The logo fits within a 45 × 45 mm slot centred 82 mm from the left and 42.5 mm from the top, viewed from below. Its proportions and viewing orientation are preserved. Use solid filled paths; convert text and strokes to paths before exporting the SVG. KiCad's bundled Python, selected by `KICAD_PYTHON`, handles the import.

Without `--logo`, the board has only its identity text. The source board is never modified. Outputs include a board copy, project settings and `bottom.svg` alongside the fabrication files. Use `--output DIR` to change the output directory.

The root `logos/` directory holds local SVG inputs and ignores them by default. A logo can also be supplied from any other path.

## Visual diff

```bash
mise run //hardware/lite/pcb:diff
```

compares the last commit's board with the working copy; `diff REV` compares a revision with the working copy, and `diff OLD NEW` two revisions. The task plots each copper, silkscreen and courtyard layer and the outline of both boards, without zone fills, and draws what was removed red, what was added green and the rest grey. It opens `build/diff/index.html`, which pans and zooms each layer and counts the tracks, vias and footprints that changed.

## JLCPCB order

PCB options:

| Option                  | Setting                 | Reason                                                                                                        |
| ----------------------- | ----------------------- | ------------------------------------------------------------------------------------------------------------- |
| Layers, size            | 6, 125 × 89 mm          |                                                                                                               |
| Thickness               | 1.6 mm                  | Economic assembly of six layers needs 1.6 mm, green and ENIG                                                  |
| Specify stackup         | JLC06161H-3313          | The 90 Ω pairs (0.1356 mm tracks, 0.1501 mm gap) are sized for it; accept JLC's 1.54 mm actual thickness note |
| Colour                  | Green                   | As thickness                                                                                                  |
| Surface finish          | ENIG                    | As thickness. JLC preselects OSP, which rules out Economic assembly, and has no HASL                          |
| Via covering            | Epoxy filled and capped | No charge on six layers; some vias sit in pads                                                                |
| Min via hole / diameter | 0.3 / (0.4/0.45) mm     | No hole is smaller; 0.2 mm holes add a small-via charge and force TG155 and a 4-wire Kelvin test              |
| Impedance control       | No requirement          | ±10 % control adds a charge and requires production-file confirmation                                         |
| Mark on PCB             | Remove mark             |                                                                                                               |

Assembly options: Economic PCBA, top side, tooling holes added by JLCPCB, Confirm Parts Placement on (JLC confirms on its own after 72 hours unless "Do not confirm automatically" is ticked), parts selected by customer. PCBA quantity can be 2 or all 5 boards.

Upload `gerbers.zip`, then the variant's BOM and CPL, and:

1. Tick the USB-C rows (J1001, J401–J901). JLC charges a small fee for these parts and leaves them unticked after every upload.
2. Check every row is matched, in stock and selectable; Economic cannot place a part JLC marks Standard Only. Replace such a part in the schematic and regenerate the files, rather than in JLC's table, so the BOM here stays the order's source.
3. In Component Placements, every part should sit on its pads, and JLC's Align should change nothing. A part that Align turns or moves needs an entry in `jlc_cpl.py`.
4. The closest customs description is Research/Education/DIY, Development Board (HS 847330).

## Placement corrections

JLCPCB places its own (EasyEDA) footprint for each LCSC part, with its origin at the CPL's Mid X/Y and turned by Rotation. `jlc_cpl.py` holds, by LCSC code, the turn to add and the EasyEDA origin's position in the KiCad footprint, wherever the two footprints differ.

For a new part, fetch its EasyEDA footprint from `https://easyeda.com/api/products/<LCSC>/components?version=6.4.19.5` (pads in 0.254 mm units, y down, origin at `dataStr.head`). Match its pads to the KiCad pads by number at each quarter turn, then record the turn and origin that fit. Two-terminal chip parts need no entry.
