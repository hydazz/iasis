"""Render the case previews: the main board's case and the CM5 build's."""

import argparse
from pathlib import Path
from time import monotonic

from build123d import Compound, Pos, extrude

from . import case, sheet
from .render import render, save

PREVIEW = Path(__file__).resolve().parents[2] / "preview"
VIEW = (-300, -400, 250)
BODY, TEXT = (214, 216, 220), (35, 38, 42)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, help="preview output directory")
    args = parser.parse_args()
    output = args.output.resolve() if args.output else PREVIEW
    output.mkdir(parents=True, exist_ok=True)
    for name, assemble in (
        ("case.png", case.assemble),
        ("case-cm5.png", case.assemble_cm5),
    ):
        start = monotonic()
        print(f"{name}: building geometry", flush=True)
        lay, tray, covers = assemble(cover_names=("passive",))
        visible = Compound([lay.board.parts[ref] for ref in (*lay.ports, *case.LIGHTS)])
        c = covers["passive"]
        shown = [
            (tray, BODY, True),
            *[(foot, (53, 59, 57), False) for foot in case.feet(lay)],
            (c.part, BODY, True),
            (c.markings, TEXT, False),
            (lay.button, BODY, True),
            (visible, (100, 106, 112), False),
        ]
        print(f"{name}: rendering ({monotonic() - start:.1f}s building geometry)", flush=True)
        changed = save(render(shown, VIEW), output / name)
        print(
            f"{name}: {'written' if changed else 'unchanged'} in {monotonic() - start:.1f}s",
            flush=True,
        )
    start = monotonic()
    a = sheet.assemble()
    f, c = a.frame, a.covers["display"]
    marks = Compound([extrude(Pos(0, 0, f.top_out) * m, amount=0.02) for m in c.marks])
    visible = Compound([f.lay.board.parts[ref] for ref in (*f.lay.ports, *case.LIGHTS)])
    shown = [
        (a.base, sheet.BLACK, True),
        (c.part, sheet.BLACK, True),
        (marks, sheet.WHITE, False),
        (a.button, (30, 31, 34), True),
        (visible, (100, 106, 112), False),
    ]
    if c.display is not None:
        shown.append((c.display, (18, 22, 30), False))
    changed = save(render(shown, VIEW), output / "case-sheet.png")
    print(f"case-sheet.png: {'written' if changed else 'unchanged'} in {monotonic() - start:.1f}s")


if __name__ == "__main__":
    main()
