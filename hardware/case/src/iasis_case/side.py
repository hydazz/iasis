"""The CM5 side board beside the main board: its populated STEP, the Raspberry Pi Cooler
for CM5 and the two side-link flex options.

Geometry is in the main board's coordinates (board.py). The side board sits left of the
main board with the rear edges in line and both boards' bottom faces level, so the
straight flex runs flat between two edge-facing ZIFs.
"""

from dataclasses import dataclass
from pathlib import Path

from build123d import (
    Align,
    Box,
    Compound,
    Cylinder,
    FilletPolyline,
    Location,
    Part,
    Plane,
    Pos,
    Rectangle,
    sweep,
)

from .board import Board, load

STEP = Path(__file__).resolve().parents[4] / "build" / "boards" / "cm5" / "board.step"
LINK = "J2001"
GAP = 17.64  # main board left edge to side board right edge
STOP = 2.75  # FPC stop inside each ZIF mouth
FLEX_W, FLEX_T = 12.5, 0.19
STIFF_L, STIFF_T = 6.0, 0.13  # stiffener over each tail from the tip, 0.32 mm there
CONTACT = 0.6  # flex centre above the board top in a bottom-contact ZIF

# Raspberry Pi Cooler for CM5 (product brief): bosses on the module's four mounting
# holes carry the fin block. The module sits 1.5 mm up on its sockets and is 1.24 mm thick.
COOLER = (56.0, 41.0, 12.7)  # along the long hole pitch, across it, above the module
MODULE_TOP, BOSS_H, BOSS_D = 1.5 + 1.24, 2.7, 5.0
CM5_HOLE_D = 2.7

# Rainbow concept: a vertical 0.5 mm 24-pin ZIF on each board, such as SHOU HAN
# FPC 0.5-24P LTH4.0-A (C53457113), with the flex arched over the gap.
VZIF = (4.4, 16.4, 4.0)  # across the edge, along it, high
VZIF_IN = 2.5  # slot centre inside each facing edge
VZIF_DEPTH = 2.5  # flex inserted from above
RISE, BEND = 2.0, 3.0  # straight leg above each connector, bend radius

BASE = (Align.CENTER, Align.CENTER, Align.MIN)


@dataclass
class Side:
    board: Board  # in its own coordinates
    place: Location  # side board coordinates to main board coordinates
    cooler: Part
    straight: Part
    rainbow: Part
    vertical_zifs: Part

    @property
    def body(self) -> Part:
        return self.board.body.moved(self.place)

    @property
    def parts(self) -> Compound:
        return Compound(list(self.board.parts.values())).moved(self.place)


def assemble(main: Board, step: Path = STEP) -> Side:
    side = load(step)
    edge = -main.size.X / 2  # main board left edge, where both flex options start
    place = Location((edge - GAP - side.size.X / 2, (main.size.Y - side.size.Y) / 2, 0))
    top = main.body.bounding_box().max.Z
    y = main.box(LINK).center().Y

    holes = side.holes(CM5_HOLE_D)
    if len(holes) != 4:
        raise ValueError(f"expected the CM5's four {CM5_HOLE_D} mm holes, found {len(holes)}")
    c = sum(holes[1:], holes[0]) / 4
    span = [max(getattr(h, a) for h in holes) - min(getattr(h, a) for h in holes) for a in "XY"]
    fins = COOLER[:2] if span[0] > span[1] else COOLER[1::-1]
    seat = side.body.bounding_box().max.Z
    cooler = Pos(c.X, c.Y, seat + MODULE_TOP + BOSS_H) * Box(*fins, COOLER[2] - BOSS_H, align=BASE)
    for h in holes:
        cooler += Pos(h.X, h.Y, seat + MODULE_TOP) * Cylinder(BOSS_D / 2, BOSS_H, align=BASE)

    tip_in, tip_out = edge + STOP, edge - GAP - STOP
    z = top + CONTACT
    straight = Pos((tip_in + tip_out) / 2, y, z) * Box(tip_in - tip_out, FLEX_W, FLEX_T)
    for x in (tip_in - STIFF_L / 2, tip_out + STIFF_L / 2):
        straight += Pos(x, y, z + FLEX_T / 2) * Box(STIFF_L, FLEX_W, STIFF_T, align=BASE)

    xm, xs = edge + VZIF_IN, edge - GAP - VZIF_IN
    z0, zb = top + VZIF[2] - VZIF_DEPTH, top + VZIF[2] + RISE + BEND
    path = FilletPolyline((xm, y, z0), (xm, y, zb), (xs, y, zb), (xs, y, z0), radius=BEND)
    rainbow = sweep(Plane(origin=(xm, y, z0), z_dir=(0, 0, 1)) * Rectangle(FLEX_T, FLEX_W), path)
    vertical = Pos(xm, y, top) * Box(*VZIF, align=BASE) + Pos(xs, y, top) * Box(*VZIF, align=BASE)

    return Side(side, place, cooler.moved(place), straight, rainbow, vertical)
