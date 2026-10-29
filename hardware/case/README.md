# Iasis case

A snap-fit, 3D-printable case for Iasis, made with [build123d](https://github.com/gumyr/build123d). The normal case has a plain lid or one with an OLED cutout; both use the same tray. There's also a longer version for the main board and CM5 carrier together.

<img src="preview/case.png" alt="Iasis enclosure" width="500">

<img src="preview/case-cm5.png" alt="Iasis CM5 enclosure" width="500">

## Build

Install the [repository dependencies](../../README.md#development) and KiCad 10, then run:

```sh
mise run //hardware/case:build
```

Files are written to `build/case/iasis/` at the repository root. The build exports the populated main and CM5 boards first, then generates these case files:

| File                                           | What it's for                                      |
| ---------------------------------------------- | -------------------------------------------------- |
| `iasis-case.step`                              | Main board and both lid options                    |
| `print/tray.stl`                               | Shared tray, printed floor-down                    |
| `print/tpu-foot.stl`                           | TPU foot, printed flat-side down (make four)       |
| `print/cover-passive.stl`, `cover-display.stl` | Lids, printed top-down                             |
| `print/cover-*-lettering.stl`                  | Optional multicolour lettering for the lids        |
| `print/button-*.stl`                           | Matching button caps, printed face-down            |
| `print/display-retainer.stl`                   | OLED retaining frame, flat-side down               |
| `iasis-cm5-case.step`                          | Both boards and the longer case options            |
| `print/cm5-*.stl`                              | Longer tray, lids, lettering, buttons and retainer |

Refresh the images with `mise run //hardware/case:preview`. Run `mise run //hardware/case:test` to check solids, clearances, USB access and moving parts. Neither runs automatically during the build. You can pass `--output DIR` to `build` or `preview`.

The root Python environment also provides `iasis-case-build`, `iasis-case-preview`, `iasis-case-check` and `iasis-case-view` through `uv run --locked`.

## Print and assemble

Use a 0.4 mm nozzle, 0.20 mm layers and no supports. Print the button cap in the case colour. For multicolour markings, import the lid and matching lettering STL as parts of one object, retaining their positions, then assign the filaments. Otherwise leave the lettering out for recessed text.

The four TPU feet are 12 mm across and 2 mm thick, with a 0.4 mm bottom chamfer. Glue them into the underside recesses (12.4 mm across, 0.8 mm deep); about 1.2 mm will protrude before adhesive thickness.

Set the PCB onto the tray pegs. Fit the button from inside the lid, then press the lid down until the catches engage. To open it, squeeze the round pads at either end and lift the lid straight up. The floor is solid; vents above the PCB and slots in the lid provide airflow.

### OLED lid

The display option fits an Adafruit 938 STEMMA QT OLED. Place it on the locating pins and slide the retaining frame under the two rear keepers, then snap its front edge into the catch. To remove it, push the finger pad inward, lift the front and slide the frame out. Connect the Qwiic cable before closing the case, keeping it away from the button and lid catches.

### CM5 case

The longer tray holds both boards and exposes the CM5 board's Ethernet, accessory USB-C, service USB-C and nRPIBOOT pinhole along the rear. The CM5 side has nine rows of vents over the cooler, separated from the main controls by a shallow groove.

The model uses a [Waveshare CM4-HEATSINK (SKU 19623)](https://www.waveshare.com/cm4-heatsink.htm), ASIN B093FSS6XX, with a nominal 55 × 40 × 5 mm body and a provisional 3 mm mounting allowance above the module PCB. Check the real mounting hardware and thermal-pad contact before relying on the fit; the CAD clearance check doesn't test cooling.

The cooler reaches 12.21 mm above the carrier underside, with the inside roof at 13.5 mm (1.29 mm clearance). The lid is 1.6 mm thick; the underside-to-base stack is 5 mm. The Ethernet shell reaches 15.05 mm, almost level with the top surface at 15.1 mm, through an open rear notch. Taller cooler spacers will need a taller case.

With power disconnected, attach the cooler screws from below. Align the rear board edges, keep both boards level, slide the CM5 header into the hub socket and lower the pair onto the tray pegs. The header isn't shrouded; the board edges and tray pegs provide the alignment.

## Size and labels

| Case                  | External size           |
| --------------------- | ----------------------- |
| Main board, plain lid | 136 × 104.6 × 17.1 mm   |
| Main board, OLED lid  | 136 × 104.6 × 19.6 mm   |
| CM5, either lid       | 212.6 × 104.6 × 20.1 mm |

The button rises about 0.2 mm above the lid. A dot marks target port 1. Laptop and plug symbols mark the host and power connectors; the activity symbol marks the status LED. The CM5 case uses Lucide USB/terminal symbols for the accessory and service ports. Ethernet has no top label.

The three button labels use 4.5 mm bold text (roughly 3.3 mm-high capitals), right-aligned beside their gesture symbols:

| Mark               | Gesture               | Action                        |
| ------------------ | --------------------- | ----------------------------- |
| Dot                | Click                 | DFU                           |
| Bar                | Hold                  | Reboot connected devices      |
| Two dots and a bar | Two clicks, then hold | Request restore from the host |

A restore gesture needs a host already listening with a prepared IPSW and explicit erase permission; the button doesn't erase anything by itself. Remove the lid to reach BOOTSEL and RESET.

## Preview and viewer

`mise run //hardware/case:preview` renders images with pygfx's offscreen WebGPU renderer, skips unchanged inputs and exits. A working graphics driver is required. To force new images, run `mise run --force //hardware/case:preview`.

For an interactive model, use [OCP CAD Viewer](https://marketplace.visualstudio.com/items?itemName=bernhard-42.ocp-cad-viewer) with `mise run //hardware/case:view`, or `mise run //hardware/case:view-cm5` for the longer case. The viewer separates Tray, PCB and Cover; both lids appear under Cover, so show one at a time. Use the root `.venv` configured in `.vscode/settings.json`. If the extension needs a newer viewer package, run `uv lock --upgrade-package ocp-vscode`.

The symbols come from [Lucide](https://lucide.dev/), with source SVGs and licences in `src/iasis_case/icons/`. Check cable clearance, button travel and catches on a real print before treating the fit as proven.
