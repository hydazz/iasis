"""Screw joints, seam guides and OLED mounting posts, in assembled coordinates."""

from dataclasses import dataclass
from functools import cached_property
from math import cos, radians, sin
from typing import cast

from build123d import (
    Align,
    Box,
    Compound,
    Cylinder,
    Part,
    Plane,
    Polygon,
    Pos,
    RegularPolygon,
    Rot,
    extrude,
)

FIT = 0.10
SCREW_D, SCREW_CLEAR = 3.0, 3.4
HEAD_D, HEAD_H = 6.0, 2.6
NUT_AF, NUT_H = 5.5, 2.4
NUT_FIT, NUT_PLAY = 0.15, 0.3
BOSS_D = 9.2
BASE = (Align.CENTER, Align.CENTER, Align.MIN)


def section(points: list[tuple[float, float]], y: float, width: float) -> Part:
    return extrude(Pos(0, y + width / 2, 0) * (Plane.XZ * Polygon(*points, align=None)), width)


@dataclass(frozen=True)
class Fastener:
    x: float
    y: float
    bearing: float  # under-head face; screw points toward +Z
    length: float
    angle: float  # nut-loading direction, measured from +X

    @property
    def tip(self) -> float:
        return self.bearing + self.length

    @property
    def nut_z(self) -> float:
        return self.tip - NUT_H - 1.0

    @property
    def place(self):
        return Pos(self.x, self.y, self.nut_z) * Rot(Z=self.angle)

    @cached_property
    def pocket(self) -> Part:
        af = NUT_AF + 2 * NUT_FIT
        hexagon = extrude(RegularPolygon(af / 2, 6, major_radius=False), NUT_H + NUT_PLAY)
        entry = Pos(4, 0, 0) * Box(8, af, NUT_H + NUT_PLAY, align=BASE)
        return cast(Part, self.place * (hexagon + entry))

    @cached_property
    def bore(self) -> Part:
        return Pos(self.x, self.y, self.bearing - 0.1) * Cylinder(
            SCREW_CLEAR / 2, self.length + 0.4, align=BASE
        )

    @cached_property
    def receiver_cut(self) -> Part:
        # Face-down printing bridges the nut slot before closing around the screw.
        # The long opening anchors both ends of that first bridge; a round bore
        # would start with a floating inner perimeter.
        passage = Pos(self.x, self.y, self.bearing - 0.1) * Box(
            SCREW_CLEAR, SCREW_CLEAR, self.nut_z - self.bearing + 0.2, align=BASE
        )
        bridge = self.place * (Pos(0, 0, -0.4) * Box(SCREW_CLEAR, NUT_AF + 1.1, 0.5, align=BASE))
        return cast(Part, self.pocket + self.bore + passage + bridge)

    @cached_property
    def screw(self) -> Part:
        head = Pos(self.x, self.y, self.bearing - HEAD_H) * Cylinder(HEAD_D / 2, HEAD_H, align=BASE)
        shank = Pos(self.x, self.y, self.bearing) * Cylinder(SCREW_D / 2, self.length, align=BASE)
        return cast(Part, head + shank)

    @cached_property
    def nut(self) -> Part:
        body = extrude(RegularPolygon(NUT_AF / 2, 6, major_radius=False), NUT_H)
        body -= Cylinder(SCREW_D / 2, NUT_H, align=BASE)
        return cast(Part, self.place * body)

    @property
    def hardware(self) -> Compound:
        return Compound([self.screw, self.nut], label="M3 screw and nut")

    def nut_at(self, distance: float) -> Part:
        a = radians(self.angle)
        return Pos(distance * cos(a), distance * sin(a), 0) * self.nut

    @property
    def driver(self) -> Part:
        return Pos(self.x, self.y, self.bearing - HEAD_H - 30) * Cylinder(3, 30, align=BASE)


def guides(edge: float, split: float) -> list[Part]:
    parts = []
    for y in (-36, 36):
        guide = section(
            [
                (edge - 0.8, split + 1.6),
                (edge - 1, split + 1.6),
                (edge - 2.4, split + 0.2),
                (edge - 2.4, split - 0.8),
                (edge - 2, split - 1.2),
                (edge - 1.2, split - 1.2),
                (edge - 1.2, split + 0.2),
                (edge - 0.8, split + 0.6),
            ],
            y,
            6,
        )
        parts += [guide, guide.mirror(Plane.YZ)]
    return parts


def guide_pockets(edge: float, split: float) -> list[Part]:
    parts = []
    for y in (-36, 36):
        cut = Pos(edge - 1.8, y, split - 1.2 - FIT) * Box(
            1.2 + 2 * FIT, 6 + 2 * FIT, 1.2 + 2 * FIT, align=BASE
        )
        parts += [cut, cut.mirror(Plane.YZ)]
    return parts


@dataclass(frozen=True)
class DisplayMount:
    x: float
    face: float
    roof: float
    thickness: float
    holes: tuple[tuple[float, float], ...]

    screw_d: float = 2.5
    screw_length: float = 4.0
    head_d: float = 5.0
    head_h: float = 2.2
    post_d: float = 4.0
    pilot_d: float = 2.2

    @property
    def rear(self) -> float:
        return self.face - self.thickness

    @property
    def tip(self) -> float:
        return self.rear + self.screw_length

    @cached_property
    def centres(self) -> tuple[tuple[float, float], ...]:
        return tuple((self.x + x, y) for x, y in self.holes)

    @cached_property
    def pilots(self) -> list[Part]:
        # Smooth bores: the screws form their threads in the printed posts.
        return [
            Pos(x, y, self.face - 0.1)
            * Cylinder(self.pilot_d / 2, self.tip - self.face + 0.3, align=BASE)
            for x, y in self.centres
        ]

    @cached_property
    def posts(self) -> list[Part]:
        return [
            cast(
                Part,
                Pos(x, y, self.face)
                * Cylinder(self.post_d / 2, self.roof - self.face + 0.5, align=BASE)
                - pilot,
            )
            for (x, y), pilot in zip(self.centres, self.pilots, strict=True)
        ]

    @cached_property
    def heads(self) -> list[Part]:
        return [
            Pos(x, y, self.rear - self.head_h) * Cylinder(self.head_d / 2, self.head_h, align=BASE)
            for x, y in self.centres
        ]

    @cached_property
    def screws(self) -> list[Part]:
        return [
            cast(
                Part,
                head
                + Pos(x, y, self.rear) * Cylinder(self.screw_d / 2, self.screw_length, align=BASE),
            )
            for (x, y), head in zip(self.centres, self.heads, strict=True)
        ]
