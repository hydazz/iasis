"""The CM5 side board beside the main board: its populated STEP, the Waveshare CM4-HEATSINK
envelope, its screw heads, and both boards as one for the CM5 build's
enclosure.

Side geometry is in the main board's coordinates (board.py). The side board sits left of
the main board with the rear edges in line and both boards' bottom faces level. Each board
carries one half of the coplanar right-angle header/socket pair at the facing edge.
"""

from dataclasses import dataclass, field
from pathlib import Path

from build123d import (
    Align,
    Box,
    Compound,
    Cylinder,
    Face,
    GeomType,
    Location,
    Part,
    Pos,
    RectangleRounded,
    Solid,
)

from .board import Board, load

STEP = Path(__file__).resolve().parents[4] / "build" / "boards" / "cm5" / "board.step"
GAP = 1.5  # main board left edge to side board right edge


# Waveshare 19623 / CM4-HEATSINK: 55 x 40 x 5 mm body, excluding mounting hardware.
# Reserve 3 mm between the module PCB and heatsink base. This is a fit allowance;
# confirm the supplied standoffs and CM5 thermal-pad contact on the physical module.
COOLER = (55.0, 40.0, 5.0)
MODULE_TOP, BOSS_H, BOSS_D = 1.5 + 1.24, 3.0, 5.0
CM5_HOLE_D = 2.7
SCREW_HEAD = (4.5, 1.65)  # M2.5 pan head under the board, diameter and height

SIDE = "S:"  # the side board's reference prefix in the CM5 build

BASE = (Align.CENTER, Align.CENTER, Align.MIN)


@dataclass
class Side:
    board: Board  # in its own coordinates
    place: Location  # side board coordinates to main board coordinates
    cooler: Part
    screws: Part  # the cooler screws' heads under the side board

    @property
    def body(self) -> Solid | Compound:
        return self.board.body.moved(self.place)

    @property
    def parts(self) -> Compound:
        return Compound(list(self.board.parts.values())).moved(self.place)


def assemble(main: Board, step: Path = STEP) -> Side:
    side = load(step)
    edge = -main.size.X / 2  # main board left edge
    place = Location((edge - GAP - side.size.X / 2, (main.size.Y - side.size.Y) / 2, 0))

    holes = side.holes(CM5_HOLE_D)
    if len(holes) != 4:
        raise ValueError(f"expected the CM5's four {CM5_HOLE_D} mm holes, found {len(holes)}")
    c = sum(holes[1:], holes[0]) / 4
    span = [max(getattr(h, a) for h in holes) - min(getattr(h, a) for h in holes) for a in "XY"]
    fins = COOLER[:2] if span[0] > span[1] else COOLER[1::-1]
    seat = side.body.bounding_box().max.Z
    cooler = Pos(c.X, c.Y, seat + MODULE_TOP + BOSS_H) * Box(
        fins[0], fins[1], COOLER[2], align=BASE
    )
    for h in holes:
        cooler += Pos(h.X, h.Y, seat + MODULE_TOP) * Cylinder(BOSS_D / 2, BOSS_H, align=BASE)
    under = (Align.CENTER, Align.CENTER, Align.MAX)
    screws = Part([
        Pos(h.X, h.Y, 0) * Cylinder(SCREW_HEAD[0] / 2, SCREW_HEAD[1], align=under)
        for h in holes
    ])  # fmt: skip

    return Side(side, place, cooler.moved(place), screws.moved(place))


@dataclass
class Pair(Board):
    """Both boards of the CM5 build as one board for the enclosure, centred on their common
    outline: the main board's references as they are, the side board's prefixed with SIDE,
    and the cooler and its screw heads as parts."""

    side: Side | None = None
    shift: Location = field(default_factory=Location)  # main board coordinates to these
    corner: float = 0.0

    @property
    def join_x(self) -> float:
        """Centre of the gap between the boards, in enclosure coordinates."""
        assert self.side is not None
        return self.side.body.bounding_box().max.X + GAP / 2 + self.shift.position.X

    def outline(self) -> Face:
        bb = self.body.bounding_box()
        return (
            Pos(bb.center().X, bb.center().Y) * RectangleRounded(bb.size.X, bb.size.Y, self.corner)
        ).face()

    @property
    def attach(self) -> list[tuple[Path, Location]]:
        """The side board's STEP in the main board STEP's coordinates."""
        assert self.side is not None
        s = self.side
        return [(s.board.step, self.origin.inverse() * self.shift * s.place * s.board.origin)]


def pair(main: Board, side: Side) -> Pair:
    bodies = Compound([main.body, side.body])
    c = bodies.bounding_box().center()
    shift = Location((-c.X, -c.Y, 0))
    parts = {ref: p.moved(shift) for ref, p in main.parts.items()}
    parts |= {SIDE + ref: p.moved(shift * side.place) for ref, p in side.board.parts.items()}
    extra = {"cooler": side.cooler, "cooler screws": side.screws}
    parts |= {name: Compound([shape.moved(shift)], label=name) for name, shape in extra.items()}
    outlines = (main.outline(), side.board.outline())
    radii = [e.radius for o in outlines for e in o.edges() if e.geom_type == GeomType.CIRCLE]
    return Pair(
        bodies.moved(shift),
        parts,
        main.step,
        shift * main.origin,
        side=side,
        shift=shift,
        corner=min(radii, default=0.0),
    )
