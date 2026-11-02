"""SC1163 RTC battery seat and lead space beneath the CM5 carrier."""

from dataclasses import dataclass
from functools import cached_property

from build123d import Align, Box, Circle, Compound, Part, Pos, RectangleRounded, extrude

from .side import SIDE, Pair

BASE = (Align.CENTER, Align.CENTER, Align.MIN)
# Joy-IT RB-RTC-BAT (SC1163's EAN): 20 x 30 x 3 mm, approximately 100 mm leads.
# The fit envelope includes wrap/adhesive allowance; these are not supplier maxima.
PACK = (20.8, 30.8, 3.5)
POCKET_DEPTH = 1.0
LEAD_MIN = 90.0
WIRE_D = 0.8  # JST SH maximum insulation diameter
PLUG_OVER = 2.3  # 6.3 mm mated height above PCB, plus 0.25 mm allowance, less 4.25 mm header


@dataclass
class Battery:
    x: float
    y: float
    floor: float
    seam: float
    plug_x: float
    plug_y: float
    plug_bottom: float

    @property
    def bottom(self) -> float:
        return self.floor - POCKET_DEPTH

    @cached_property
    def pack(self) -> Part:
        profile = Pos(self.x, self.y + 5) * Circle(PACK[0] / 2)
        profile += Pos(self.x, self.y - PACK[1] / 2 + 7) * RectangleRounded(9, 14, 2)
        body = extrude(Pos(0, 0, self.bottom) * profile, amount=PACK[2])
        body.label = "RTC battery fit envelope"
        return body

    @cached_property
    def pocket(self) -> Part:
        profile = Pos(self.x, self.y + 5) * Circle(11)
        profile += Pos(self.x, self.y - 9) * RectangleRounded(12, 14, 2)
        # Finger/tool access to lift the adhesive-mounted pack after removing the boards.
        profile += Pos(self.x - 10, self.y + 5) * Circle(4)
        return extrude(Pos(0, 0, self.bottom) * profile, amount=POCKET_DEPTH + 0.1)

    @property
    def path(self) -> list[tuple[float, float]]:
        return [
            (self.x, self.y - PACK[1] / 2),
            (self.x, self.y - 22),
            (self.seam, self.y - 22),
            (self.seam, self.plug_y),
        ]

    @cached_property
    def channel(self) -> Part:
        profile = Circle(2).moved(Pos(*self.path[0]))
        for (x0, y0), (x1, y1) in zip(self.path, self.path[1:]):
            profile += Pos((x0 + x1) / 2, (y0 + y1) / 2) * RectangleRounded(
                abs(x1 - x0) + 4, abs(y1 - y0) + 4, 1.99
            )
        return extrude(Pos(0, 0, self.bottom) * profile, amount=POCKET_DEPTH + 0.1)

    @cached_property
    def plug(self) -> Part:
        # Reserve the flanged SH housing above the header, including its exposed top.
        body = Pos(self.plug_x, self.plug_y, self.plug_bottom) * Box(2.8, 5, PLUG_OVER, align=BASE)
        body.label = "RTC plug clearance"
        return body

    @property
    def wire_z(self) -> float:
        return self.bottom + WIRE_D / 2 + 0.1

    @property
    def bend_z(self) -> float:
        return self.plug_bottom + PLUG_OVER + 2

    @property
    def lead_length(self) -> float:
        flat = sum(
            abs(x1 - x0) + abs(y1 - y0) for (x0, y0), (x1, y1) in zip(self.path, self.path[1:])
        )
        return flat + self.bend_z - self.wire_z + abs(self.seam - self.plug_x) + 2

    @cached_property
    def lead(self) -> Compound:
        # Paired wires lie side by side along Y at the seam, only one wire wide across it.
        pieces = []
        for (x0, y0), (x1, y1) in zip(self.path, self.path[1:]):
            pieces.append(
                Pos((x0 + x1) / 2, (y0 + y1) / 2, self.wire_z)
                * Box(abs(x1 - x0) + 1.8, abs(y1 - y0) + 1.8, WIRE_D)
            )
        pieces += [
            Pos(self.seam, self.plug_y, (self.wire_z + self.bend_z) / 2)
            * Box(WIRE_D, 1.8, self.bend_z - self.wire_z + WIRE_D),
            Pos((self.seam + self.plug_x) / 2, self.plug_y, self.bend_z)
            * Box(abs(self.seam - self.plug_x) + WIRE_D, 1.8, WIRE_D),
            Pos(self.plug_x, self.plug_y, self.bend_z - 1) * Box(WIRE_D, 1.8, 2),
        ]
        return Compound(pieces, label="RTC lead clearance")


def seat(board: Pair, floor: float) -> Battery:
    assert board.side is not None
    side = board.side.body.moved(board.shift).bounding_box()
    plug = board.box(SIDE + "J104")
    return Battery(
        side.min.X + 56,
        side.max.Y - 36,
        floor,
        board.join_x,
        plug.center().X,
        plug.center().Y,
        plug.max.Z,
    )
