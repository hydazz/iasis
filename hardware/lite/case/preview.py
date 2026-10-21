"""Render the case preview."""

import argparse
from pathlib import Path

from build123d import Compound

import case
from render import render, save

PREVIEW = Path(__file__).resolve().parent / "preview"
VIEW = (-300, -400, 250)
BODY, TEXT = (214, 216, 220), (35, 38, 42)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--logo", type=Path, help="filled SVG placed beside the name")
    parser.add_argument("--output", type=Path, help="preview output directory")
    args = parser.parse_args()
    output = args.output.resolve() if args.output else PREVIEW
    lay, tray, covers = case.assemble(args.logo)
    visible = Compound([lay.board.parts[ref] for ref in (*case.PORTS, *case.LIGHTS)])
    c = covers["passive"]
    shown = [
        (tray, BODY),
        (c.part, BODY),
        (c.markings, TEXT),
        (c.button, BODY),
        (visible, (100, 106, 112)),
    ]
    output.mkdir(parents=True, exist_ok=True)
    if save(render(shown, VIEW), output / "case.png"):
        print(f"{output / 'case.png'} written")


if __name__ == "__main__":
    main()
