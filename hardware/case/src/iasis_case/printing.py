"""Print-oriented STL exports with aligned, optional lettering."""

from pathlib import Path

from build123d import Compound, Location, Mesher
from build123d.topology.shape_core import Shape


def export(
    name: str,
    body: Shape,
    directory: Path,
    markings: Shape | None = None,
    rotation: Location | None = None,
) -> None:
    directory.mkdir(parents=True, exist_ok=True)
    rotation = rotation or Location()
    body = rotation * body
    if markings is not None:
        markings = rotation * markings
    bb = Compound([body] + ([markings] if markings is not None else [])).bounding_box()
    on_bed = Location((-bb.center().X, -bb.center().Y, -bb.min.Z))
    for shape, suffix in ((body, ""), (markings, "-lettering")):
        if shape is not None:
            mesher = Mesher()
            mesher.add_shape(
                Compound([on_bed * shape], label=f"{name}{suffix}"),
                linear_deflection=0.01,
                angular_deflection=0.1,
            )
            mesher.write(directory / f"{name}{suffix}.stl")
