"""Export the case STLs and board/case assembly STEPs, for the main board alone and the CM5
build."""

import argparse
from pathlib import Path

from build123d import Compound, Part, Pos, Rot
from build123d.topology.shape_core import Shape

from . import board as kicad
from . import case, printing

BUILD = Path(__file__).resolve().parents[4] / "build" / "case" / "iasis"


def write(
    prefix: str, lay: case.Layout, tray: Part, covers: dict[str, case.Cover], output: Path
) -> Path:
    """Print STLs named with prefix, and the assembly STEP; returns the STEP's path."""
    print_dir = output / "print"
    printing.export(f"{prefix}tray", tray, print_dir)
    printing.export(f"{prefix}button", lay.button, print_dir, rotation=Rot(X=180))
    for name, c in covers.items():
        printing.export(f"{prefix}cover-{name}", c.part, print_dir, c.markings, rotation=Rot(X=180))
        if not prefix and c.display_mount is not None:
            pins = Compound([Pos(x, y) * c.display_mount.pin for x in (-4, 4) for y in (-4, 4)])
            printing.export("display-pins", pins, print_dir)

    extra: list[Shape] = [tray, lay.button, *case.feet(lay), *(f.hardware for f in lay.fasteners)]
    if lay.battery is not None:
        extra += [lay.battery.pack, lay.battery.lead, lay.battery.plug]
    for c in covers.values():
        extra += [c.part, c.markings]
        if c.display is not None:
            extra.append(c.display)
            assert c.display_mount is not None
            extra.extend(c.display_mount.pins)
    step = output / f"iasis-{prefix}case.step"
    kicad.export_assembly(lay.board, extra, step, getattr(lay.board, "attach", None))
    return step


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, help="build output directory")
    args = parser.parse_args()
    output = args.output.resolve() if args.output else BUILD
    output.mkdir(parents=True, exist_ok=True)
    for obsolete in ("tpu-foot.stl", "display-retainer.stl", "cm5-display-retainer.stl"):
        (output / "print" / obsolete).unlink(missing_ok=True)
    for prefix in ("", "cm5-"):
        for name in case.COVERS:
            (output / "print" / f"{prefix}button-{name}.stl").unlink(missing_ok=True)
    steps = [
        write("", *case.assemble(), output),
        write("cm5-", *case.assemble_cm5(), output),
    ]
    print(f"STLs: {output / 'print'}")
    print("Assemblies: " + ", ".join(str(s) for s in steps))


if __name__ == "__main__":
    main()
