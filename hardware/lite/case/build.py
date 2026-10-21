"""Export the case STLs and board/case assembly STEP."""

import argparse
from pathlib import Path

from build123d import Rot
from build123d.topology.shape_core import Shape

import board as kicad
import case
import printing

HERE = Path(__file__).resolve().parent
BUILD = HERE / "build"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, help="separate build directory")
    args = parser.parse_args()
    output = args.output.resolve() if args.output else BUILD
    output.mkdir(parents=True, exist_ok=True)
    lay, tray, covers = case.assemble()

    print_dir = output / "print"
    printing.export("tray", tray, print_dir)
    for name, c in covers.items():
        printing.export(f"cover-{name}", c.part, print_dir, c.markings, rotation=Rot(X=180))
        printing.export(f"button-{name}", c.button, print_dir, rotation=Rot(X=180))
        if c.retainer is not None:
            printing.export("display-retainer", c.retainer.part, print_dir)

    extra: list[Shape] = [tray]
    for c in covers.values():
        extra += [c.part, c.markings, c.button]
        if c.display is not None:
            extra.append(c.display)
        if c.retainer is not None:
            extra.append(c.retainer.part)
    kicad.export_assembly(lay.board, extra, output / "iasis-case.step")
    print(f"STLs: {print_dir}")
    print(f"Assembly: {output / 'iasis-case.step'}")


if __name__ == "__main__":
    main()
