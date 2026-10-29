"""Export the case STLs and board/case assembly STEPs, for the main board alone and the CM5
build."""

import argparse
from pathlib import Path

from build123d import Part, Rot
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
    for name, c in covers.items():
        printing.export(f"{prefix}cover-{name}", c.part, print_dir, c.markings, rotation=Rot(X=180))
        printing.export(f"{prefix}button-{name}", c.button, print_dir, rotation=Rot(X=180))
        if c.retainer is not None:
            printing.export(f"{prefix}display-retainer", c.retainer.part, print_dir)

    extra: list[Shape] = [tray, *case.feet(lay)]
    for c in covers.values():
        extra += [c.part, c.markings, c.button]
        if c.display is not None:
            extra.append(c.display)
        if c.retainer is not None:
            extra.append(c.retainer.part)
    step = output / f"iasis-{prefix}case.step"
    kicad.export_assembly(lay.board, extra, step, getattr(lay.board, "attach", None))
    return step


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, help="build output directory")
    args = parser.parse_args()
    output = args.output.resolve() if args.output else BUILD
    output.mkdir(parents=True, exist_ok=True)
    printing.export("tpu-foot", case.foot(), output / "print")
    steps = [
        write("", *case.assemble(), output),
        write("cm5-", *case.assemble_cm5(), output),
    ]
    print(f"STLs: {output / 'print'}")
    print("Assemblies: " + ", ".join(str(s) for s in steps))


if __name__ == "__main__":
    main()
