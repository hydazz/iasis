# Iasis case

A 3D-printed enclosure for the compact Iasis Hub and Hub PD board, generated with [build123d](https://github.com/gumyr/build123d) from the board's STEP export. A plate carries the board on four standoffs. Passive and optional OLED covers share that plate.

## Build

Tools come from mise (`mise install` at the repository root); mise also creates and activates `.venv` here with its Python, and uv fills it from `uv.lock`. The board export needs KiCad 10 at the path `KICAD_CLI` names in the root `.mise/config.toml`. Run tasks in this directory, or elsewhere as `mise run //hardware/case:<task>`.

```bash
mise run case
```

`case` first runs `board-step`, which exports the populated board to `build/board.step` with `kicad-cli` whenever the PCB or its project models have changed. It then builds the parts, checks their fit and writes `build/`, exiting non-zero when a fit check fails:

| File                         | Contents                                                                                   |
| ---------------------------- | ------------------------------------------------------------------------------------------ |
| `iasis-case.step`            | KiCad's board STEP with the plate, covers and display envelope added, in KiCad coordinates |
| `print/*.stl`, `print/*.3mf` | Plate and covers in print orientation                                                      |
| `report.txt`                 | Sizes and fit checks                                                                       |

Open `build/iasis-case.step` in a STEP viewer or CAD editor. The case parts are solid bodies named `plate`, `cover passive` and `cover display`; hide the cover you are not inspecting. The body named `Adafruit 938 1.3in 128x64 OLED envelope` is the display module's clearance envelope, not its shape.

## Design

- **Board reference.** Part instances in the STEP carry their reference designators. The outline, four 3.2 mm mounting holes, connector faces, lights, switches and populated heights come from that file. `case.py` names the required references and holds fit and form dimensions. The fit checks see only modelled parts, so every placed component needs a 3D model in the board. The optional display module is a clearance envelope.
- **Fastening.** Four M3 × 12 button-head screws (ISO 7380) pass through the plate and board into M3 × 5.7 heat-set inserts in the cover pillars. Four 12 mm stick-on feet sit in plate pockets.
- **Ports.** Five target USB-C ports face the front. Host and power USB-C ports face the rear. Each opening passes a maximum-size Type-C plug overmold (12.35 × 6.5 mm) through the wall to the receptacle mouth.
- **Controls.** The host and five target indicators shine sideways through wall slots. The confirmation switch SW1171 has a flexible tongue in the top. Labelled top holes give access to BOOTSEL (SW1101) and RESET (SW1102).
- **Display.** The display cover hangs an Adafruit 938 (1.3 in, 128 × 64 OLED, STEMMA QT) under a bevelled top window at the board centre, aligned between ports 2 and 3. Four M2 × 5 self-tapping screws secure the module to posts. A Qwiic cable connects it to the board's I2C display socket. The cover height reserves room for the module above the populated board.
- **Ventilation.** The passive cover has rear top slots and low end/rear wall slots. The display cover retains wall slots.
- **Marks.** The name is engraved 0.6 mm into the top near the front left corner.
- **Printing.** Covers print top down; the plate prints flat. Check the chosen material, fit and button flex on a physical print before assembly.

Run `mise run case` after board changes and resolve failures in `report.txt`. The checks cover modelled fit, plug access and recovery access; they do not establish print tolerances or thermal performance.
