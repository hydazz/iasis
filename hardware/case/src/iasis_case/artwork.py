"""Planar case icons from Lucide SVGs."""

from functools import lru_cache
from pathlib import Path

from build123d import Align, Compound, Face, Pos, Sketch, Wire, import_svg
from OCP.OCP.BRepOffsetAPI import BRepOffsetAPI_MakeOffset
from OCP.OCP.GeomAbs import GeomAbs_Arc


@lru_cache
def icon(name: str, size: float = 7.0, stroke: float = 0.65) -> Sketch:
    """Centre an icon from its 24-unit SVG grid, with stroke width in millimetres."""
    result = Sketch()
    for shape in import_svg(Path(__file__).with_name("icons") / f"{name}.svg", align=Align.CENTER):
        if not isinstance(shape, Wire):
            raise TypeError(f"{name}: expected a stroke-only SVG")
        wire = shape.scale(size / 24)
        outline = Face(wire.offset_2d(stroke / 2))
        if outline.normal_at().Z < 0:
            outline = -outline
        if wire.is_closed:
            # Keep enclosed holes only when they are at least one printable stroke wide.
            inset = BRepOffsetAPI_MakeOffset()
            inset.Init(GeomAbs_Arc)
            edges = wire.edges()
            if len(edges) == 1:
                wire = Wire([edges[0].trim(0, 0.5), edges[0].trim(0.5, 1)])
            inset.AddWire(wire.wrapped)
            inset.Perform(-stroke / 2)
            if not inset.IsDone():
                raise ValueError(f"{name}: could not inset closed stroke")
            for inner in Compound(inset.Shape()).wires():
                inner_size = inner.bounding_box().size
                if min(inner_size.X, inner_size.Y) >= stroke:
                    outline -= Face(inner)
        result += outline
    centre = result.bounding_box().center()
    return Pos(-centre.X, -centre.Y) * Sketch(result.faces())
