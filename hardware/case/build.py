"""Build the Iasis enclosure from build/board.step: parts, fit checks and outputs.

Writes build/: the board STEP with the case parts added, print-oriented STL and 3MF parts
and report.txt. Rerenders preview/case.png. Exits 1 when a fit check fails.
"""

import sys
from pathlib import Path

from build123d import Compound, Rot
from build123d.topology.shape_core import Shape

import board as kicad
import case
import check
import printing
from render import render, save

HERE = Path(__file__).resolve().parent
BUILD = HERE / "build"
PREVIEW = HERE / "preview"
VIEW = (-300, -400, 250)  # above the front left corner
BODY, TEXT = (214, 216, 220), (35, 38, 42)


def main() -> int:
    board = kicad.load(BUILD / "board.step")
    lay = case.layout(board)
    tray = case.tray(lay)
    covers = {name: case.cover(lay, name, display) for name, display in case.COVERS.items()}
    report = check.run(lay, tray, covers)

    lines = ["Iasis enclosure", ""]
    for name, c in covers.items():
        bb = c.part.bounding_box()
        what = case.COVERS[name].name if case.COVERS[name] else "passive"
        lines.append(
            f"  {name:8s} {bb.size.X:.1f} x {bb.size.Y:.1f} x "
            f"{bb.max.Z - lay.z_bottom:.1f} mm assembled ({what})"
        )
    lines += ["", *report.lines, ""]
    lines.append(f"{len(report.failures)} failures" if report.failures else "all checks passed")
    text = "\n".join(lines) + "\n"
    (BUILD / "report.txt").write_text(text)
    print(text, end="")
    if report.failures:
        return 1

    print_dir = BUILD / "print"
    print_dir.mkdir(exist_ok=True)
    printing.export("tray", tray, print_dir)
    for name, c in covers.items():
        printing.export(f"cover-{name}", c.part, print_dir, c.markings, rotation=Rot(X=180))
        printing.export(f"button-{name}", c.button, print_dir, rotation=Rot(X=180))

    extra: list[Shape] = [tray]
    for c in covers.values():
        extra += [c.part, c.markings, c.button]
        if c.display is not None:
            extra.append(c.display)
    kicad.export_assembly(board, extra, BUILD / "iasis-case.step")

    PREVIEW.mkdir(exist_ok=True)
    visible = Compound([board.parts[ref] for ref in (*case.PORTS, *case.LIGHTS)])
    c = covers["passive"]
    shown = [
        (tray, BODY),
        (c.part, BODY),
        (c.markings, TEXT),
        (c.button, BODY),
        (visible, (100, 106, 112)),
    ]
    if save(render(shown, VIEW), PREVIEW / "case.png"):
        print("preview/case.png written")
    return 0


if __name__ == "__main__":
    sys.exit(main())
