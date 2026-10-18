"""Show the board and one enclosure variant in OCP CAD Viewer.

uv run python -m ocp_viewer             # the viewer, at http://127.0.0.1:3939
uv run view.py [passive|display]     # in a second terminal
"""

import sys

from build123d import Box, Color, Compound, Location, Pos, Shape
from ocp_viewer import show

import board as kicad
import build
import case

# Colours by reference prefix; the viewer does not read the models' own colours.
PART_COLORS = {
    "J": (0.72, 0.73, 0.75),
    "U": (0.13, 0.13, 0.14),
    "Q": (0.18, 0.18, 0.19),
    "L": (0.30, 0.30, 0.32),
    "C": (0.78, 0.66, 0.45),
    "R": (0.10, 0.10, 0.10),
    "D": (0.92, 0.92, 0.88),
    "H": (0.80, 0.68, 0.35),
}
BOARD_COLOR = (0.08, 0.30, 0.18)
PREVIEW_SMALL = 5.0  # mm, bounding-box diagonal under which a part is drawn as its box
PREVIEW_MIN = 1.2  # mm, bounding-box diagonal of the smallest solid drawn


def preview_board(lay: case.Layout) -> list[Shape]:
    """The board for on-screen review, one shape per part: small parts as boxes, tiny
    solids dropped, colours by reference prefix."""
    body = lay.board.body.moved(Location())
    body.color, body.label = Color(*BOARD_COLOR), "PCB"
    shapes: list[Shape] = [body]
    for ref, part in lay.board.placed().items():
        rough = lay.board.rough(ref)
        if rough.diagonal < PREVIEW_SMALL:  # passives and small ICs: their box is enough
            kept = [Pos(rough.center()) * Box(rough.size.X, rough.size.Y, rough.size.Z)]
        else:
            kept = [s for s in [*part.solids(), *part.shells()]
                    if s.bounding_box().diagonal >= PREVIEW_MIN]  # fmt: skip
        if kept:
            piece = Compound(kept)
            rgb = PART_COLORS.get(ref[0], (0.5, 0.5, 0.5))
            piece.color, piece.label = Color(*rgb), ref
            shapes.append(piece)
    return shapes


def main() -> None:
    variant = sys.argv[1] if len(sys.argv) > 1 else "passive"
    if variant not in case.COVERS:
        sys.exit(f"unknown variant {variant}; choose {' or '.join(case.COVERS)}")
    lay = case.layout(kicad.load(build.STEP))
    cover = case.cover(lay, variant, case.COVERS[variant])
    shapes = [Compound(preview_board(lay)), case.plate(lay), cover.part]
    names = ["board", "plate", f"cover {variant}"]
    alphas = [1.0, 1.0, 0.5]
    if cover.display is not None:
        shapes.append(cover.display)
        names.append("display envelope")
        alphas.append(0.3)
    show(*shapes, names=names, alphas=alphas)


if __name__ == "__main__":
    main()
