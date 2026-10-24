# Iasis case

A screwless 3D-printable enclosure for Iasis, generated with [build123d](https://github.com/gumyr/build123d) from the populated board. Passive and OLED covers share a tray. The bodies measure 136 × 104.6 mm, with heights of 17.1 mm for the passive cover and 19.6 mm for the OLED cover. The button stands another 0.2 mm above the roof. The CM5 build has its own enclosure, one tray and cover for the main board and the CM5 side board: 212.6 × 104.6 mm and 25.1 mm high, set by the CM5 cooler.

<img src="preview/case.png" alt="Iasis enclosure" width="500">

<img src="preview/case-cm5.png" alt="Iasis CM5 build enclosure from the rear" width="500">

## Build

Install the repository's mise tools and KiCad 10, then run:

```bash
mise run //hardware/case:build
```

The task exports the populated boards from `../boards/iasis` and `../boards/cm5`, then writes the case files for both builds to `build/case/iasis/` at the repository root.

| Output | Contents |
| --- | --- |
| `build/case/iasis/iasis-case.step` | Board and both enclosure variants |
| `build/case/iasis/print/tray.stl` | Shared tray, floor down |
| `build/case/iasis/print/cover-passive.stl`, `cover-display.stl` | Covers, top down, with recessed markings |
| `build/case/iasis/print/cover-*-lettering.stl` | Aligned colour fills for the cover markings |
| `build/case/iasis/print/button-*.stl` | Matching button caps, face down |
| `build/case/iasis/print/display-retainer.stl` | OLED retaining frame, flat side down |
| `build/case/iasis/iasis-cm5-case.step` | Both boards and the CM5 build's enclosure variants |
| `build/case/iasis/print/cm5-*.stl` | The CM5 build's tray, covers, lettering, buttons and retainer, oriented as above |

Run `mise run //hardware/case:preview` to refresh `preview/case.png` and `preview/case-cm5.png`. Run `mise run //hardware/case:check` to check solid validity, component clearance, USB access and moving parts for both enclosures. Neither is part of the build task.

Use `--output DIR` on `build` or `preview` to change their output directories.

The installed package also provides `iasis-case-build`, `iasis-case-preview`, `iasis-case-check` and `iasis-case-view`; run them with `uv run --locked` from this directory.

## Print and assemble

Use a 0.4 mm nozzle and 0.20 mm layers, without supports. Print the button in the body colour. For multicolour covers, import the cover and its matching lettering STL together as parts of one object, preserving their positions, then assign filaments. Leave the lettering out for recessed markings.

Place the board on the tray pegs, insert the button from inside the cover, and press the cover down until both end catches engage. To open, press the two round end-wall pads inward and lift the cover vertically.

The OLED cover accepts an Adafruit 938 STEMMA QT display. Seat it on the cover's locating pins, slip the retaining frame into the two rear keepers, then press its front edge into the finger catch. To remove it, push the finger pad inward, lift the front edge and slide the frame out. Connect a Qwiic cable before closing the case, keeping the lead clear of the button and catches.

The CM5 build's enclosure holds both boards on its tray pegs, the side board's RJ45, accessory and service USB-C ports and nRPIBOOT pinhole in the rear wall beside the host and power ports. Fit the CM5 cooler's screws from below before seating the side board, then fit the side-link flex into both vertical connectors, contacts toward the gap, and close their actuators, with the cover removed.

A dot marks target port 1; laptop and plug icons identify host and power. The activity icon marks the Iasis status LED. The button legend denotes a click for DFU and a hold to reboot connected targets. Firmware is not yet implemented. Remove the cover to reach BOOTSEL and RESET.

## Viewer

Run `mise run //hardware/case:view` with [OCP CAD Viewer](https://marketplace.visualstudio.com/items?itemName=bernhard-42.ocp-cad-viewer) to show Tray, PCB and Cover layers. Both cover variants are under Cover, the OLED one hidden; show one at a time. Run `mise run //hardware/case:view-cm5` for the CM5 build.

Use this directory's `.venv`, configured in `.vscode/settings.json`. Update the viewer library with `uv lock --upgrade-package ocp-vscode` when the extension requires a newer version.

Icons are from [Lucide](https://lucide.dev/); source SVGs and licences are in `src/iasis_case/icons/`. Verify cable fit, button movement and latch operation on a physical print.
