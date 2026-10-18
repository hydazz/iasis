# Iasis case

A 3D-printed enclosure for the compact Iasis Hub and Hub PD board, generated with [build123d](https://github.com/gumyr/build123d) from the board's STEP export. A plate carries the board on four standoffs. Passive and optional OLED covers share that plate.

## Build

Tools come from mise (`mise install` at the repository root). mise also creates and activates `.venv` here with its Python, and uv fills it from `uv.lock`. KiCad 10 is needed only for the export. Run tasks in this directory, or elsewhere as `mise run //hardware/case:<task>`.

```bash
mise run board-step
mise run case
```

`board-step` exports the populated board to `build/board.step` with `kicad-cli` and writes `board.models.json` using KiCad’s Python. Set `KICAD_PYTHON` if that interpreter is outside the standard macOS application bundle. `case` builds from the STEP and its companion inventory, warns when the board has changed since the export, and exits non-zero when a fit check fails. It writes `build/`:

| File                         | Contents                                                                                   |
| ---------------------------- | ------------------------------------------------------------------------------------------ |
| `iasis-case.step`            | KiCad's board STEP with the plate, covers and display envelope added, in KiCad coordinates |
| `print/*.stl`, `print/*.3mf` | Plate and covers in print orientation                                                      |
| `report.txt`                 | Sizes and fit checks                                                                       |

Use a fresh STEP export of the compact board. The earlier carrier board export does not contain the recovery controls this enclosure requires.

## Preview

Start OCP CAD Viewer, then send it a cover (`passive` or `display`) from a second terminal:

```bash
uv run python -m ocp_viewer
```

```bash
uv run view.py passive
```

Run both in `hardware/case`. The viewer opens at http://127.0.0.1:3939.

Open `build/iasis-case.step` in a STEP viewer or CAD editor. The case parts are solid bodies named `plate`, `cover passive` and `cover display`; hide the cover you are not inspecting. Translucent clearance envelopes identify the display and components without 3D models; they bound clearance, rather than reproducing the part’s shape.

## Design

- **Board reference.** Part instances in the STEP carry their reference designators. The outline, four 3.2 mm mounting holes, connector faces, lights, switches and populated heights come from that file. `case.py` names the required references and holds fit and form dimensions. Parts without 3D models use explicitly labelled clearance envelopes from their actual placed courtyards, datasheet maximum heights and a 0.15 mm solder seating allowance. The report lists those envelopes and fails if any component body remains unmodelled. The optional display module has its own envelope.
- **Fastening.** Four M3 × 12 button-head screws (ISO 7380) pass through the plate and board into M3 × 5.7 heat-set inserts in the cover pillars. Four 12 mm stick-on feet sit in plate pockets.
- **Ports.** Five target USB-C ports face the front. Host and power USB-C ports face the rear. Each opening passes a maximum-size Type-C plug overmold (12.35 × 6.5 mm) through the wall to the receptacle mouth.
- **Controls.** The host and five target indicators shine sideways through wall slots. The confirmation switch SW1171 has a flexible tongue in the top. Labelled top holes give access to BOOTSEL (SW1101) and RESET (SW1102).
- **Display.** The display cover hangs an Adafruit 938 (1.3 in, 128 × 64 OLED, STEMMA QT) under a bevelled top window at the board centre, aligned between ports 2 and 3. Four M2 × 5 self-tapping screws secure the module to posts. A Qwiic cable connects it to the board's I2C display socket. The cover height reserves room for the module above the populated board.
- **Ventilation.** The passive cover has rear top slots and low end/rear wall slots. The display cover retains wall slots.
- **Marks.** The name is engraved 0.6 mm into the top near the front left corner.
- **Printing.** Covers print top down; the plate prints flat. Check the chosen material, fit and button flex on a physical print before assembly.

Run both build tasks after board changes and resolve failures in `report.txt`. For a geometry check without exporting print files, run `uv run build.py --check-only`. Geometry checks cover modelled fit, plug access and recovery access; they do not establish print tolerances or thermal performance.

Clearance envelope heights come from these package drawings:

| Part | Maximum package height | Source |
| --- | --- | --- |
| PSPMAA1040-100M-ANP | 4.0 mm | [PROD Tech PSPMAA series, 1040 drawing](https://datasheet.lcsc.com/datasheet/pdf/08c11c8d5a8f832fc44fdee6ce5245a5.pdf) |
| LMR51450SDRRR | 0.8 mm | [TI LMR51450, DRR0012G](https://www.ti.com/lit/ds/symlink/lmr51450.pdf) |
| LM74502DDFR | 1.1 mm | [TI LM74502, DDF0008A](https://www.ti.com/lit/ds/symlink/lm74502.pdf) |
| CH224Q | 0.8 mm | [WCH CH224 V2.0, DFN10](https://www.wch-ic.com/downloads/CH224DS1_PDF.html) |
