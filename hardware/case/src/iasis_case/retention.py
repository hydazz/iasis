"""Finger-release end catches and mating guides, in assembled coordinates."""

from dataclasses import dataclass
from functools import cached_property
from typing import cast

from build123d import Align, Box, Cylinder, Part, Plane, Polygon, Pos, Rot, extrude

FIT = 0.15
BEAM_THICKNESS = 0.8
RELEASE, STOP = 0.4, 0.35
LATCH_PLAY = 0.2
BASE = (Align.CENTER, Align.CENTER, Align.MIN)


def paired(part: Part) -> list[Part]:
    return [part, part.mirror(Plane.YZ)]


def section(points: list[tuple[float, float]], y: float, width: float) -> Part:
    """Extrude an X/Z section towards decreasing Y."""
    return extrude(Pos(0, y + width / 2, 0) * (Plane.XZ * Polygon(*points, align=None)), width)


@dataclass(frozen=True)
class Joint:
    edge: float
    split: float
    roof: float
    y: float = 0.0
    pad_z: float = 6.2

    @property
    def shoulder(self) -> float:
        return self.split + 0.7

    @property
    def root(self) -> float:
        return self.roof + 0.8

    @property
    def inner(self) -> float:
        return self.edge - 2.4

    @cached_property
    def pad(self) -> Part:
        plane = Plane(
            origin=(self.edge - 1.6, self.y, self.pad_z), x_dir=(0, 1, 0), z_dir=(1, 0, 0)
        )
        pad = plane * Cylinder(3, 1.6, align=BASE)
        # A 45-degree crown grows the pad out from the beam when printed roof down.
        ramp = section(
            [
                (self.edge - 1.6, self.pad_z - 4),
                (self.edge + 0.1, self.pad_z - 4),
                (self.edge + 0.1, self.pad_z + 1.3),
                (self.edge - 1.6, self.pad_z + 3),
            ],
            self.y,
            7,
        )
        return cast(Part, pad & ramp)

    @cached_property
    def hook(self) -> Part:
        x, z = self.edge, self.shoulder
        beam = Pos(self.inner + BEAM_THICKNESS / 2, self.y, z - 0.45) * Box(
            BEAM_THICKNESS, 8, self.root + 0.2 - z + 0.45, align=BASE
        )
        toe = section(
            [
                (x - 1.6, z - 0.4),
                (x - 1.35, z - 0.4),
                (x - 0.95, z),
                (x - 1.35, z),
                (x - 1.35, z + 0.4),
                (x - 1.6, z + 0.65),
            ],
            self.y,
            8,
        )
        return cast(Part, beam + toe + self.pad)

    @cached_property
    def opening(self) -> Part:
        plane = Plane(origin=(self.edge - 3, self.y, self.pad_z), x_dir=(0, 1, 0), z_dir=(1, 0, 0))
        return plane * Cylinder(3.25, 4, align=BASE)

    @cached_property
    def catch(self) -> Part:
        x, z = self.edge, self.shoulder
        catch = Pos(x - 0.6, self.y, self.split - 0.2) * Box(1.2, 10, 2.3, align=BASE)
        # The blind pocket leaves a continuous back wall beneath its small overhang.
        catch -= Pos(x - 1.4, self.y, z - 0.8) * Box(1.2, 8 + 2 * FIT, 0.8 + LATCH_PLAY, align=BASE)
        return cast(Part, catch - self.opening)

    def tray_parts(self) -> list[Part]:
        return paired(self.catch)

    def guides(self) -> list[Part]:
        parts = []
        x, z = self.edge, self.split
        for y in (-32, 32):
            guide = section(
                [
                    (x - 1, z + 1),
                    (x - 1.8, z + 1),
                    (x - 1.8, z - 0.7),
                    (x - 1.5, z - 1),
                    (x - 1, z - 1),
                ],
                y,
                4,
            )
            parts += paired(guide)
        return parts

    def tray_cuts(self) -> list[Part]:
        cuts: list[Part] = [
            Pos(self.edge - 1.5, y, self.split - 1 - FIT)
            * Box(1 + 2 * FIT, 4 + 2 * FIT, 2, align=BASE)
            for y in (-32, 32)
        ]
        cuts.append(self.opening)
        return [half for cut in cuts for half in paired(cut)]

    def cover_parts(self) -> list[Part]:
        stop = Pos(self.inner - STOP - 1, self.y, self.pad_z) * Box(
            2, 6, self.roof - self.pad_z + 0.5, align=BASE
        )
        return paired(self.hook) + paired(stop) + self.guides()

    def cover_cuts(self) -> list[Part]:
        x, z = self.edge, self.split
        cuts = [
            Pos(x - 0.6, self.y, z - 1) * Box(3, 10 + 2 * FIT, 3.1 + FIT, align=BASE),
            self.opening,
            Pos(x - 2, self.y, self.roof - 0.1) * Box(1.6, 8.8, 0.9, align=BASE),
        ]
        return [half for cut in cuts for half in paired(cut)]


@dataclass(frozen=True)
class DisplayFrame:
    x: float
    rear: float
    roof: float
    holes: tuple[tuple[float, float], ...]

    thickness: float = 1.2
    play: float = 0.2
    release: float = 0.6

    @property
    def base(self) -> float:
        return self.rear - 4.73

    @property
    def contact(self) -> float:
        return self.rear - self.play

    @cached_property
    def tip(self) -> Part:
        return Pos(self.x + 7, -16.65, self.base) * Box(6, 4.3, self.thickness, align=BASE)

    @cached_property
    def part(self) -> Part:
        z = self.base
        frame = Pos(self.x, 0, z) * Box(42, 37.6, self.thickness, align=BASE)
        frame -= Pos(self.x, 0, z - 1) * Box(36, 33.6, self.thickness + 2, align=BASE)
        # An 18 mm planar arm releases the front keeper; its pad stops after 0.8 mm.
        frame -= Pos(self.x + 1.4, -17.2, z - 1) * Box(18.8, 0.8, 4, align=BASE)
        frame -= Pos(self.x + 10.4, -18, z - 1) * Box(0.8, 3, 4, align=BASE)
        frame += self.tip
        frame += Pos(self.x + 13.25, -13.2, z) * Box(12.5, 1, self.thickness, align=BASE)
        for x, y in self.holes:
            side = 1 if x > 0 else -1
            frame += Pos(self.x + (x + side * 19.5) / 2, y, z) * Box(
                19.5 - abs(x), 4, self.thickness, align=BASE
            )
            frame += Pos(self.x + x, y, z) * Cylinder(2, self.contact - z, align=BASE)
            frame -= Pos(self.x + x, y, z - 1) * Cylinder(1.4, self.contact - z + 2, align=BASE)
        frame.label = "display retainer"
        return cast(Part, frame)

    @cached_property
    def keepers(self) -> list[Part]:
        # Horizontal bearing faces retain the frame without spring preload.
        profile = [
            (20, self.base - 0.6),
            (20, self.roof + 0.5),
            (19, self.roof + 0.5),
            (19, self.base),
            (18.4, self.base),
        ]
        rear = Rot(Z=90) * section(profile, 0, 6)
        return [Pos(self.x + x, 0, 0) * rear for x in (-15.24, 15.24)] + [
            Pos(self.x + 7, 0, 0) * rear.mirror(Plane.XZ)
        ]
