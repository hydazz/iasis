# Iasis case

A 3D-printable enclosure for Iasis, generated with [build123d](https://github.com/gumyr/build123d) from the populated board. Passive and OLED covers share a tray. Both bodies measure 128 × 91.6 × 20.1 mm, excluding feet.

<img src="preview/case.png" alt="Iasis enclosure" width="500">

## Build

Install the repository's mise tools and KiCad 10, then run:

```bash
mise run //hardware/case:case
```

The task exports the board, checks the enclosure fit, generates `build/` and updates `preview/case.png`.

| Output | Contents |
| --- | --- |
| `build/iasis-case.step` | Board and both enclosure variants |
| `build/print/tray.*` | Shared tray, floor down |
| `build/print/cover-*.stl`, `cover-*.3mf` | Covers, top down, with recessed markings |
| `build/print/cover-*-multicolour.3mf` | Covers with flush markings as a second material |
| `build/print/button-*.*` | Matching button caps, face down |
| `build/report.txt` | Dimensions and fit checks |

## Print and assemble

Use a 0.4 mm nozzle and 0.20 mm layers, without supports. Print the button in the body colour. For multicolour covers, assign filaments to the aligned `Body` and `Lettering` parts; do not arrange them separately.

Fit four M3 × 5.7 heat-set inserts to the cover. Place the board on the tray, insert the button from inside the cover, and fasten with four M3 × 10 ISO 7380 screws from below. Add feet at least 2 mm thick to clear the screw heads.

The OLED cover accepts an Adafruit 938 1.3-inch display, secured with four M2 × 4 self-tapping screws. Connect it to the board with a Qwiic cable, keeping the lead flat below the display.

A dot marks target port 1; laptop and plug icons identify host and power. The button legend denotes a click for DFU and a hold to reboot connected targets. Firmware is not yet implemented. Remove the cover to reach BOOTSEL and RESET.

## Viewer

Run `case.py` with [OCP CAD Viewer](https://marketplace.visualstudio.com/items?itemName=bernhard-42.ocp-cad-viewer) to show Tray, PCB and Cover layers. Both cover variants are under Cover; show one at a time. Run `uv run case.py display` to start with the OLED cover visible.

Use this directory's `.venv`, configured in `.vscode/settings.json`. Update the viewer library with `uv lock --upgrade-package ocp-vscode` when the extension requires a newer version.

Icons are from [Lucide](https://lucide.dev/); source SVGs and licences are in `icons/`. Verify cable fit and button movement on a physical print.
