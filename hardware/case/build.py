"""Build the Iasis enclosure from build/board.step: parts, fit checks and outputs.

Writes build/: the board STEP with the case parts added, print-oriented STL and 3MF parts
and report.txt. Exits 1 when a fit check fails.
"""

import sys
from pathlib import Path

from build123d import Location, Mesher, Rot, export_stl

import board as kicad
import case
import check

BUILD = Path(__file__).resolve().parent / "build"


def main() -> int:
    board = kicad.load(BUILD / "board.step")
    lay = case.layout(board)
    plate = case.plate(lay)
    covers = {name: case.cover(lay, name, display) for name, display in case.COVERS.items()}
    report = check.run(lay, plate, covers)

    lines = ["Iasis enclosure", ""]
    for name, c in covers.items():
        bb = c.part.bounding_box()
        what = case.COVERS[name].name if case.COVERS[name] else "passive"
        lines.append(f"  {name:8s} {bb.size.X:.1f} x {bb.size.Y:.1f} x {bb.size.Z:.1f} mm ({what})")
    lines += ["", *report.lines, ""]
    lines.append(f"{len(report.failures)} failures" if report.failures else "all checks passed")
    text = "\n".join(lines) + "\n"
    (BUILD / "report.txt").write_text(text)
    print(text, end="")

    print_dir = BUILD / "print"
    print_dir.mkdir(exist_ok=True)
    parts = {"plate": plate} | {f"cover-{n}": Rot(X=180) * c.part for n, c in covers.items()}
    for name, part in parts.items():  # covers print top down, the plate flat
        bb = part.bounding_box()
        part = part.moved(Location((-bb.center().X, -bb.center().Y, -bb.min.Z)))
        export_stl(part, print_dir / f"{name}.stl", tolerance=0.01, angular_tolerance=0.1)
        mesher = Mesher()
        mesher.add_shape(part, linear_deflection=0.01, angular_deflection=0.1, part_number=name)
        mesher.write(str(print_dir / f"{name}.3mf"))

    extra = [plate, *(c.part for c in covers.values())]
    extra += [c.display for c in covers.values() if c.display is not None]
    kicad.export_assembly(board, extra, BUILD / "iasis-case.step")
    return 1 if report.failures else 0


if __name__ == "__main__":
    sys.exit(main())
