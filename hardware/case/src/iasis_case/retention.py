"""Screw joints, locating pegs and OLED mounts, in assembled coordinates."""

from dataclasses import dataclass
from functools import cached_property
from math import cos, radians, sin
from typing import cast

from build123d import (
    Align,
    Box,
    Compound,
    Cone,
    Cylinder,
    Part,
    Plane,
    Pos,
    Rectangle,
    RegularPolygon,
    Rot,
    extrude,
    loft,
)

SCREW_D, SCREW_CLEAR = 3.0, 3.4
HEAD_D, HEAD_H = 6.0, 2.6
HEAD_RECESS = HEAD_H + 0.2
HEAD_CLEAR = HEAD_D + 0.4
NUT_AF, NUT_H = 5.5, 2.4
NUT_CLEARANCE, NUT_PLAY = 0.1, 0.2
BOSS_D = 9.2
BASE = (Align.CENTER, Align.CENTER, Align.MIN)


@dataclass(frozen=True)
class Fastener:
    x: float
    y: float
    bearing: float  # under-head face; screw points toward +Z
    length: float
    angle: float  # nut-loading direction, measured from +X
    nut_clearance: float = NUT_CLEARANCE  # total across-flats allowance

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
        af = NUT_AF + self.nut_clearance
        hexagon = extrude(RegularPolygon(af / 2, 6, major_radius=False), NUT_H + NUT_PLAY)
        entry = Pos(4, 0, 0) * Box(8, af, NUT_H + NUT_PLAY, align=BASE)
        height = NUT_H + NUT_PLAY
        mouth = loft(
            [
                Pos(3.3, 0, height / 2) * (Plane.YZ * Rectangle(af, height)),
                Pos(BOSS_D / 2, 0, height / 2) * (Plane.YZ * Rectangle(af + 1.0, height + 1.0)),
            ]
        )
        return cast(Part, self.place * (hexagon + entry + mouth))

    @cached_property
    def bore(self) -> Part:
        return Pos(self.x, self.y, self.bearing - 0.1) * Cylinder(
            SCREW_CLEAR / 2, self.length + 0.4, align=BASE
        )

    @cached_property
    def head_cut(self) -> Part:
        recess = Pos(self.x, self.y, self.bearing - HEAD_RECESS - 0.1) * Cylinder(
            HEAD_CLEAR / 2, HEAD_RECESS + 0.1, align=BASE
        )
        # Two anchored bridge layers close the counterbore before the round bore.
        bridge = Pos(self.x, self.y, self.bearing - 0.1) * Box(
            SCREW_CLEAR, HEAD_CLEAR, 0.3, align=BASE
        )
        passage = Pos(self.x, self.y, self.bearing - 0.1) * Box(
            SCREW_CLEAR, SCREW_CLEAR, 0.5, align=BASE
        )
        return cast(Part, recess + bridge + passage + self.bore)

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


@dataclass(frozen=True)
class DisplayMount:
    x: float
    face: float
    roof: float
    thickness: float
    holes: tuple[tuple[float, float], ...]
    post_d: float = 4.6
    pilot_d: float = 2.4
    pin_d: float = 2.3
    embed: float = 2.4
    head_d: float = 4.8
    head_h: float = 1.2

    @property
    def rear(self) -> float:
        return self.face - self.thickness

    @cached_property
    def centres(self) -> tuple[tuple[float, float], ...]:
        return tuple((self.x + x, y) for x, y in self.holes)

    @cached_property
    def pilots(self) -> list[Part]:
        bore = Pos(0, 0, self.face) * Cylinder(self.pilot_d / 2, self.embed + 0.2, align=BASE)
        entry = Pos(0, 0, self.face - 0.01) * Cone(
            self.pilot_d / 2 + 0.2, self.pilot_d / 2, 0.21, align=BASE
        )
        return [cast(Part, Pos(x, y, 0) * (bore + entry)) for x, y in self.centres]

    @cached_property
    def posts(self) -> list[Part]:
        post = Pos(0, 0, self.face) * Cylinder(
            self.post_d / 2, self.roof - self.face + 0.5, align=BASE
        )
        # The wide root stays above the glass; the post is rigid, not a spring.
        post += Pos(0, 0, self.roof - 0.3) * Cone(self.post_d / 2, 3.1, 0.3, align=BASE)
        return [
            cast(Part, Pos(x, y, 0) * post - pilot)
            for (x, y), pilot in zip(self.centres, self.pilots, strict=True)
        ]

    @cached_property
    def pin(self) -> Part:
        """Printed head-down; the head seats against the actual PCB thickness."""
        length = self.thickness + self.embed
        head = Cylinder(self.head_d / 2, self.head_h, align=BASE)
        stem = Pos(0, 0, self.head_h) * Cylinder(self.pin_d / 2, length - 0.4, align=BASE)
        tip = Pos(0, 0, self.head_h + length - 0.4) * Cone(
            self.pin_d / 2, self.pin_d / 2 - 0.2, 0.4, align=BASE
        )
        return cast(Part, head + stem + tip)

    @cached_property
    def pins(self) -> list[Part]:
        return [Pos(x, y, self.rear - self.head_h) * self.pin for x, y in self.centres]
