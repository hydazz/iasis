"""Planar case markings from text and SVG artwork."""

from functools import lru_cache
from pathlib import Path

from build123d import Align, Face, FontStyle, Pos, Sketch, Text, Wire, import_svg


def wordmark(name: str, size: float, logo: Path | None = None) -> Sketch:
    text = Text(name, font_size=size, font_style=FontStyle.BOLD)
    if logo is None:
        return text
    shapes = import_svg(logo)
    mark = Sketch([face for shape in shapes for face in shape.faces()])
    if not mark.faces():
        raise ValueError(f"{logo}: expected filled SVG artwork")
    bounds = mark.bounding_box().size
    mark = mark.scale(min(20 / bounds.X, 10.8 / bounds.Y))
    lb, tb = mark.bounding_box(), text.bounding_box()
    width = lb.size.X + 1.8 + tb.size.X
    combined = (
        Pos(-width / 2 - lb.min.X, -lb.center().Y) * mark
        + Pos(-width / 2 + lb.size.X + 1.8 - tb.min.X, -tb.center().Y) * text
    )
    return Sketch(combined.faces())


@lru_cache
def icon(name: str, size: float = 7.0, stroke: float = 0.65) -> Sketch:
    """Centre an icon from its 24-unit SVG grid, with stroke width in millimetres."""
    result = Sketch()
    for shape in import_svg(Path(__file__).with_name("icons") / f"{name}.svg", align=Align.CENTER):
        if not isinstance(shape, Wire):
            raise TypeError(f"{name}: expected a stroke-only SVG")
        wire = shape.scale(size / 24)
        outline = Face(wire.offset_2d(stroke / 2))
        if wire.is_closed:
            outline -= Face(wire.offset_2d(-stroke / 2))
        result += outline
    centre = result.bounding_box().center()
    return Pos(-centre.X, -centre.Y) * Sketch(result.faces())
