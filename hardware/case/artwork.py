"""Lucide SVG strokes expanded into printable, planar inlays."""

from functools import lru_cache
from pathlib import Path

from build123d import Align, Face, Pos, Sketch, Wire, import_svg


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
