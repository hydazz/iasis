# Iasis case

A 3D-printed enclosure for the Iasis board, generated with [build123d](https://github.com/gumyr/build123d) from the board's own STEP export. A plate carries the board on six standoffs and a cover drops over it. The cover carries the openings, so the seam runs round the underside, rising only where the plate fills the slots under the LAN and power jacks.

## Build

Tools come from mise (`mise install` at the repository root). mise also creates and activates `.venv` here with its Python, and uv fills it from `uv.lock`. KiCad 10 is needed only for the export. Run tasks in this directory, or elsewhere as `mise run //hardware/case:<task>`.

```bash
mise run board-step
```

```bash
mise run case
```

`board-step` exports the populated board to `build/board.step` with `kicad-cli`. `case` builds everything from that file alone, warns when the board has changed since the export, and exits non-zero when a fit check fails. It writes `build/`:

| File | Contents |
| --- | --- |
| `iasis-case.step` | KiCad's board STEP with the plate, every cover and the cooler, fan and display envelopes added, in KiCad's coordinates |
| `print/*.stl`, `print/*.3mf` | plate and covers in print orientation |
| `report.txt` | sizes and fit checks |

## Preview

Start OCP CAD Viewer, then send it a cover (`passive`, `fan` or `display`) from a second terminal:

```bash
uv run python -m ocp_viewer
```

```bash
uv run view.py fan
```

Run both in `hardware/case`. The viewer opens at http://127.0.0.1:3939.

## Fusion 360

Open `build/iasis-case.step`. The case parts are solid bodies named `plate`, `cover passive`, `cover fan` and `cover display`; hide the covers you are not working on. The envelopes are translucent boxes, not parts.

## Design

- **Board reference.** Every part instance in the board STEP is named after its reference designator. The outline, the six 3.2 mm (M3) holes, connector faces, lights, buttons and part heights come from that file; `case.py` names the references it uses (`PORTS`, `LIGHTS`, `BOOT`, `BUTTON`, `CM5`, `NVME`) and holds the fit and form numbers. What the STEP lacks is modelled as an explicit envelope: the CM5 cooler (41 × 56 × 12.7 mm, Raspberry Pi's published size), the fan and the display module.
- **Fastening.** Six M3 × 12 button-head screws (ISO 7380) go up through the plate and the board's M3 holes into M3 × 5.7 heat-set inserts in the cover's pillars, clamping the board between plate and cover. Four 12 mm stick-on feet sit in pockets in the plate.
- **USB-C.** A Type-C plug seats only when its overmold reaches the receptacle mouth, and these receptacles stop at the board edge. Each port gets an opening that passes a maximum-size overmold (12.35 × 6.5 mm) through the wall to the mouth.
- **LAN and power.** The RJ45 and the power DIN sit in slots that hug the jack and the DIN's round boss, so both can be flush with the wall. The cover drops past them, so each slot runs to the rim and a gate on the plate fills it under the jack.
- **Lights and buttons.** The host and five port pixels and the CM5 activity LED shine sideways through slots in the walls no larger than their faces. RPIBOOT is pressed through a 1.6 mm pinhole beside the host port. The confirm button is a tongue cut into the top, with a pin down to SW1171.
- **Display.** The display cover hangs an Adafruit 938 (1.3 in, 128 × 64 OLED, STEMMA QT) glass up under a bevelled window in the top, between ports 2 and 3, on four M2 × 5 self-tapping screws into posts. A 200 mm STEMMA QT (Qwiic) cable runs to J1131, the board's I2C display socket in front of the CM5.
- **Cooling.** Slots in the top over the CM5 cooler, the five port power stages and the SSD; intake slots low in the end walls and along the rear. The fan cover puts a Noctua NF-A4x10 5V PWM over the cooler in place of its slots. The plate is the same for every cover.
- **Marks.** The name is engraved 0.6 mm into the top near the front left corner.
- **Printing.** PETG or ASA, 1.6 mm walls, no supports. Covers print top down, which gives the top its bed finish; the plate prints flat.

After a board change, run both tasks and fix what `report.txt` flags.
