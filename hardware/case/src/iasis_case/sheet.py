"""Folded aluminium case: a base that carries the board and both port walls, and a cover that
wraps the top and both ends.

Both parts are U-shaped with parallel bends only. The board drops into the base onto pressed
standoffs; the cover lowers over it and takes four side screws into nuts pressed into short
tabs at the base's ends. The front and rear walls sit inside the USB-C plug shoulders, which
fixes the base at 1.0 mm.

Geometry is in board coordinates (board.py) and reuses the printed case's layout (case.py).
The numbers here are the fit, the fabrication limits and the pressed hardware.
"""

import argparse
import math
from collections.abc import Sequence
from dataclasses import dataclass, replace
from functools import cached_property
from pathlib import Path
from typing import cast

from build123d import (
    Align,
    Box,
    Circle,
    Compound,
    Cylinder,
    Face,
    Part,
    Plane,
    Polyline,
    Pos,
    Rectangle,
    RectangleRounded,
    Sketch,
    SlotOverall,
    Text,
    Vector,
    export_step,
    extrude,
    make_face,
)
from build123d.topology.shape_core import Shape
from ezdxf import units
from ezdxf.filemanagement import new as new_dxf
from OCP.OCP.BRepAdaptor import BRepAdaptor_CompCurve
from OCP.OCP.GCPnts import GCPnts_QuasiUniformDeflection

from . import case, printing
from .artwork import icon
from .board import Board, load

BUILD = Path(__file__).resolve().parents[4] / "build" / "case" / "iasis" / "sheet"
BASE = (Align.CENTER, Align.CENTER, Align.MIN)


@dataclass(frozen=True)
class Sheet:
    """Aluminium 5052 sheet with one bend radius for its thickness."""

    t: float
    ri: float  # inside bend radius

    @property
    def ro(self) -> float:
        return self.ri + self.t


BASE_SHEET = Sheet(1.0, 0.8)  # outside R1.8
COVER_SHEET = Sheet(1.2, 0.8)  # outside R2.0


@dataclass(frozen=True)
class Insert:
    """A self-clinching fastener by part number. The model fuses it to the sheet with its thread
    as a plain bore."""

    code: str
    hole: float  # mounting hole
    body_d: float
    body_h: float  # proud of the sheet
    bore: float
    edge: float  # minimum distance from its centre to an edge or bend


STANDOFF = Insert("SO-M3-3", 4.2, 4.75, 3.0, 2.5, 6.0)
NUT = Insert("S-M3-1", 4.25, 6.3, 1.5, 2.5, 4.8)

# Fabrication limits for 1.0 and 1.2 mm sheet
MIN_FLANGE = {1.0: 3.8, 1.2: 5.0}
HOLE_TO_BEND = {1.0: (3.3, 3.8), 1.2: (3.6, 5.0)}  # round, square; edge to the inside face
MIN_HOLE, MIN_WEB = 1.0, 1.0
BEND_TOL = 0.3  # position of a bent flange, either way
MARK_STROKE, MARK_HEIGHT, MARK_CLEAR = 0.2, 2.0, 1.0

# Fit
PORT_GAP = 0.3  # receptacle mouth to the wall's inside
END_GAP = 0.3  # board end to a nut
TAB_GAP = 0.1  # tab outside to cover inside
REVEAL = 0.3  # the cover's front and rear edges stand this far past the base
LIFT = 0.5  # the cover's ends stop short of the base's underside
WALL_TRIM = 1.2  # 45 degree corner off each wall top, clear of the cover's bend
TAB_W, TAB_Y = 12.0, 35.0
NUT_Z = 4.0
RELIEF = 1.0  # bend relief beside each tab
SCREW_SLOT = (3.4, 4.6)  # M3 through the cover; vertical to absorb the bend stack
PCB_SCREW, COVER_SCREW = "M3 x 5 pan head", "M3 x 4 button head"

# Front panel lights, the button and the OLED
LIGHT_WINDOW = (3.0, 1.4, 0.3)  # behind diffuser film
LIGHT_EMITTER = (2.6, 1.0)  # window each LED must see through, placement error included
USB_C_METAL = (8.28, 2.43)  # USB Type-C R2.5 Fig 3-3, maximum metal plug envelope
BUTTON_HOLE_D = 11.0
CAP_D, CAP_FLANGE_D, CAP_FLANGE_EDGE = 10.0, 13.0, 0.5
CAP_LIFT, CAP_PROUD = 0.5, 0.2
SWITCH_BODY = (6.0, 3.5)  # C&K PTS645: square body and its height
SKIRT_FIT, SKIRT_WALL, SKIRT_DEPTH = 0.2, 1.0, 1.5
WINDOW_MARGIN = 0.5  # beyond the OLED's active area

SLOT_W = 2.6
INTAKES = ((-18.0, 0.0, 18.0), 5.0, 10.0, 2.8)  # y centres, z, length, width
BLACK, WHITE = (44, 46, 50), (236, 237, 239)


@dataclass
class Frame:
    """The case's outer dimensions, derived from the board."""

    lay: case.Layout
    x_tab: float  # base ends: the tabs' outside faces
    y_out: float  # base front and rear outside faces
    z_bottom: float
    top_in: float
    standoffs: list[Vector]
    screws: list[Vector]

    @property
    def board(self) -> Board:
        return self.lay.board

    @property
    def plate_top(self) -> float:
        return self.z_bottom + BASE_SHEET.t

    @property
    def top_out(self) -> float:
        return self.top_in + COVER_SHEET.t

    @property
    def x_cover(self) -> float:
        return self.x_tab + TAB_GAP + COVER_SHEET.t

    @property
    def y_cover(self) -> float:
        return self.y_out + REVEAL

    @property
    def tab_top(self) -> float:
        return NUT_Z + NUT.edge + 0.2

    @property
    def tabs(self) -> list[tuple[int, float]]:
        return [(sx, sy * TAB_Y) for sx in (-1, 1) for sy in (-1, 1)]

    @cached_property
    def print_lay(self) -> case.Layout:
        """The printed layout resized to this case, for its artwork positions."""
        return replace(self.lay, length=2 * self.x_cover, width=2 * self.y_cover)


def frame(lay: case.Layout) -> Frame:
    t = BASE_SHEET
    x_board = lay.board.outline().bounding_box().max.X
    y_in = max(abs(o.y_face) for o in lay.openings) + PORT_GAP
    y_out = y_in + t.t
    plate_top = lay.support_z - STANDOFF.body_h
    # A standoff needs its edge distance to the wall bends; holes closer go unsupported.
    standoffs = [h for h in lay.holes if (y_out - t.ro) - abs(h.Y) >= STANDOFF.edge]
    x_tab = max(
        x_board + END_GAP + NUT.body_h + t.t,
        max(abs(h.X) for h in standoffs) + STANDOFF.edge,
    )
    screws = []
    for side in (-1, 1):
        end = [h for h in standoffs if h.X * side > 0]
        edge = max(abs(h.X) for h in end)
        end = [h for h in end if abs(abs(h.X) - edge) < 1]
        screws += [min(end, key=lambda h: h.Y), max(end, key=lambda h: h.Y)]
    return Frame(lay, x_tab, y_out, plate_top - t.t, lay.top_inside, standoffs, screws)


# Geometry


def _bend(origin: tuple[float, float, float], along: tuple[float, float, float],
          out1: tuple[float, float, float], out2: tuple[float, float, float], length: float,
          s: Sheet) -> Part:  # fmt: skip
    """A 90 degree bend: the quarter tube about an axis through origin, filling the quadrant
    between unit directions out1 and out2."""
    plane = Plane(origin=origin, x_dir=out1, z_dir=along)
    sign = 1 if Vector(out2).dot(plane.y_dir) > 0 else -1
    ring = Circle(s.ro) - Circle(s.ri)
    quarter = Sketch((ring & Pos(s.ro / 2, sign * s.ro / 2) * Rectangle(s.ro, s.ro)).faces())
    return cast(Part, extrude(plane * quarter, amount=length))


def _wall_profile(f: Frame, front: bool) -> Sketch:
    """A port wall seen from outside, as it stands above its bend, with its top corners off."""
    x, z0, z1, c = f.x_tab, f.z_bottom + BASE_SHEET.ro, f.top_in, WALL_TRIM
    pts = [(-x, z0), (x, z0), (x, z1 - c), (x - c, z1), (-x + c, z1), (-x, z1 - c)]
    return Sketch(make_face(Polyline(*pts, close=True)).faces())


def _on_wall(f: Frame, front: bool, sketch: Sketch) -> Part:
    """A cut through a port wall, sketched in board x and z."""
    start = -f.y_out + BASE_SHEET.t + 1 if front else f.y_out + 1
    plane = Plane(origin=(0, start, 0), x_dir=(1, 0, 0), z_dir=(0, -1, 0))
    return cast(Part, extrude(plane * sketch, amount=BASE_SHEET.t + 2))


def _openings(f: Frame) -> list[tuple[bool, Sketch]]:
    """Port openings and light windows on the port walls, in each wall's XZ coordinates."""
    cuts = []
    for o in f.lay.openings:
        cuts.append((o.front, Pos(o.x, o.z) * o.profile()))
    w, h, r = LIGHT_WINDOW
    for ref in case.LIGHTS:
        c = f.board.box(ref).center()
        cuts.append((c.Y < 0, Pos(c.X, c.Z) * RectangleRounded(w, h, r)))
    return cuts


def base(f: Frame) -> Part:
    s = BASE_SHEET
    t, ro = s.t, s.ro
    zb, x = f.z_bottom, f.x_tab
    body = Pos(0, 0, zb) * Box(2 * x, 2 * (f.y_out - ro), t, align=BASE)
    for front in (True, False):
        sy = -1 if front else 1
        body += _bend((-x, sy * (f.y_out - ro), zb + ro), (1, 0, 0), (0, sy, 0), (0, 0, -1),
                      2 * x, s)  # fmt: skip
        plane = Plane(origin=(0, sy * (f.y_out - t / 2), 0), x_dir=(1, 0, 0), z_dir=(0, -1, 0))
        body += extrude(plane * _wall_profile(f, front), amount=t / 2, both=True)
    for sx, yc in f.tabs:
        # The bend replaces the plate's edge under the tab; reliefs separate it either side.
        body -= Pos(sx * (x - ro / 2), yc, zb - 1) * Box(ro, TAB_W, t + 2, align=BASE)
        for sy in (-1, 1):
            body -= Pos(sx * (x - (ro + 0.5) / 2), yc + sy * (TAB_W + RELIEF) / 2, zb - 1) * Box(
                ro + 0.5, RELIEF, t + 2, align=BASE
            )
        body += _bend((sx * (x - ro), yc - TAB_W / 2, zb + ro), (0, 1, 0), (sx, 0, 0),
                      (0, 0, -1), TAB_W, s)  # fmt: skip
        body += Pos(sx * (x - t / 2), yc, zb + ro) * Box(t, TAB_W, f.tab_top - zb - ro, align=BASE)
    for front, sketch in _openings(f):
        body -= _on_wall(f, front, sketch)
    body = cast(Part, body + _hardware(f))
    body -= _bores(f)
    body.label = "iasis base"
    return cast(Part, body)


def _hardware(f: Frame) -> Compound:
    solids = [
        Pos(h.X, h.Y, f.plate_top - 0.01)
        * Cylinder(STANDOFF.body_d / 2, STANDOFF.body_h + 0.01, align=BASE)
        for h in f.standoffs
    ]
    for sx, yc in f.tabs:
        inner = sx * (f.x_tab - BASE_SHEET.t)
        plane = Plane(origin=(inner + sx * 0.01, yc, NUT_Z), z_dir=(-sx, 0, 0))
        solids.append(plane * Cylinder(NUT.body_d / 2, NUT.body_h + 0.01, align=BASE))
    return Compound(solids)


def _bores(f: Frame) -> Compound:
    bores = [
        Pos(h.X, h.Y, f.z_bottom - 1)
        * Cylinder(STANDOFF.bore / 2, BASE_SHEET.t + STANDOFF.body_h + 2, align=BASE)
        for h in f.standoffs
    ]
    for sx, yc in f.tabs:
        plane = Plane(origin=(sx * (f.x_tab + 1), yc, NUT_Z), z_dir=(-sx, 0, 0))
        bores.append(plane * Cylinder(NUT.bore / 2, BASE_SHEET.t + NUT.body_h + 2, align=BASE))
    return Compound(bores)


@dataclass
class Cover:
    part: Part
    marks: list[Sketch]  # on the top face, z = 0
    holes: list[Sketch]  # through the top, z = 0
    display: Part | None


def cover(f: Frame, display: case.Display | None) -> Cover:
    s = COVER_SHEET
    t, ro = s.t, s.ro
    xo, yc = f.x_cover, f.y_cover
    body = Pos(0, 0, f.top_in) * Box(2 * (xo - ro), 2 * yc, t, align=BASE)
    z_low = f.z_bottom + LIFT
    for sx in (-1, 1):
        body += _bend((sx * (xo - ro), -yc, f.top_out - ro), (0, 1, 0), (sx, 0, 0), (0, 0, 1),
                      2 * yc, s)  # fmt: skip
        body += Pos(sx * (xo - t / 2), 0, z_low) * Box(
            t, 2 * yc, f.top_out - ro - z_low, align=BASE
        )
    holes = _top_holes(f, display)
    body -= Compound([extrude(Pos(0, 0, f.top_in - 1) * h, amount=t + 2) for h in holes])
    body -= Compound(_end_cuts(f))
    module = None
    if display is not None:
        _, module = case._display(f.print_lay, display, f.top_in, f.top_out)
    body.label = f"iasis cover{' display' if display else ''}"
    return Cover(cast(Part, body), _marks(f, display), holes, module)


def _end_cuts(f: Frame) -> list[Part]:
    """Screw slots and intakes in the cover's ends."""
    cuts = []
    ys, z, length, width = INTAKES
    for sx in (-1, 1):
        plane = Plane(origin=(sx * (f.x_cover + 1), 0, 0), x_dir=(0, -sx, 0), z_dir=(-sx, 0, 0))
        depth = COVER_SHEET.t + 2
        for y in ys:
            cuts.append(extrude(plane * Pos(-sx * y, z) * SlotOverall(length, width), amount=depth))
        w, h = SCREW_SLOT
        for _, yc in (tab for tab in f.tabs if tab[0] == sx):
            slot = Pos(-sx * yc, NUT_Z) * SlotOverall(h, w, rotation=90)
            cuts.append(extrude(plane * slot, amount=depth))
    return cuts


def _display_keepout(f: Frame, display: case.Display) -> Sketch:
    return Pos(f.lay.display_x, 0) * Rectangle(display.pcb[0] + 4, display.pcb[1] + 4)


def _window(f: Frame, display: case.Display) -> Sketch:
    ax, ay, aw, ah = display.active
    return Pos(f.lay.display_x + ax, ay) * RectangleRounded(
        aw + 2 * WINDOW_MARGIN, ah + 2 * WINDOW_MARGIN, 0.3
    )


def _button_xy(f: Frame) -> Vector:
    c = f.board.box(case.BUTTON).center()
    return Vector(c.X, c.Y)


def _marks(f: Frame, display: case.Display | None) -> list[Sketch]:
    """The printed case's top marks, kept clear of the holes as laser marking needs."""
    lay = f.print_lay
    board = f.board
    host, power, first = (board.box(ref).center() for ref in ("J901", "J1001", "J401"))
    status = board.box("D1170").center()
    sw = _button_xy(f)
    ring_r = BUTTON_HOLE_D / 2 + MARK_CLEAR
    marks = [
        Pos(host.X, lay.y_rear - 7) * icon("laptop-minimal"),
        Pos(power.X, lay.y_rear - 7) * icon("plug"),
        Pos(first.X, lay.y_front + 7) * Circle(1),
        Pos(status.X, lay.y_front + 7) * icon("activity", size=5.5),
        Pos(sw.X, sw.Y) * Sketch((Circle(ring_r + 0.7) - Circle(ring_r)).faces()),
        Pos(lay.control_left + 15, sw.Y) * Text(case.WORDMARK, font_size=case.WORDMARK_SIZE),
    ]
    left, right = lay.control_left, f.x_cover - 10
    for y in (2, -21):
        line = Pos((left + right) / 2, y) * SlotOverall(right - left, 0.8)
        if display is not None:
            line = Sketch((line - _display_keepout(f, display)).faces())
        marks.append(line)
    return marks


def _top_holes(f: Frame, display: case.Display | None) -> list[Sketch]:
    """Button hole, display window and vent bands round the control strip."""
    sw = _button_xy(f)
    holes: list[Sketch] = [Pos(sw.X, sw.Y) * Circle(BUTTON_HOLE_D / 2)]
    avoid = [m.bounding_box() for m in _marks(f, display)]
    avoid.append((Pos(sw.X, sw.Y) * Circle(BUTTON_HOLE_D / 2 + 2)).bounding_box())
    if display is not None:
        holes.append(_window(f, display))
        avoid.append(_display_keepout(f, display).bounding_box())
    left, right = -f.x_cover + 12, f.x_cover - 12
    count = math.ceil((right - left) / 18)
    length = (right - left - 2 * (count - 1)) / count
    for y in (12, 18, 24, 30, 36, -32, -37):
        for i in range(count):
            slot = Pos(left + length / 2 + i * (length + 2), y) * SlotOverall(length, SLOT_W)
            b = slot.bounding_box()
            if not any(
                b.min.X < a.max.X + 2 and b.max.X > a.min.X - 2
                and b.min.Y < a.max.Y + 2 and b.max.Y > a.min.Y - 2
                for a in avoid
            ):  # fmt: skip
                holes.append(slot)
    return holes


def button(f: Frame) -> Part:
    """Printed cap: a square skirt locates it on the switch body, the flange keeps it under
    the lid and the hole round it is clearance only."""
    sb = f.board.box(case.BUTTON)
    c = sb.center()
    body_top = sb.min.Z + SWITCH_BODY[1]
    pocket = SWITCH_BODY[0] + 2 * SKIRT_FIT
    outer = pocket + 2 * SKIRT_WALL
    # The flange's 45 degree top meets the hole's edge after CAP_LIFT, and lets the cap print
    # shaft down without supports.
    cone = (CAP_FLANGE_D - CAP_D) / 2
    cone_bottom = f.top_in - CAP_LIFT - (CAP_FLANGE_D - BUTTON_HOLE_D) / 2
    flange_bottom = cone_bottom - CAP_FLANGE_EDGE
    skirt_bottom = body_top - SKIRT_DEPTH
    cap = Pos(c.X, c.Y, skirt_bottom) * Box(outer, outer, flange_bottom - skirt_bottom, align=BASE)
    cap -= Pos(c.X, c.Y, skirt_bottom - 0.1) * Box(pocket, pocket, sb.max.Z - skirt_bottom + 0.1,
                                                     align=BASE)  # fmt: skip
    cap += Pos(c.X, c.Y, flange_bottom - 0.01) * Cylinder(
        CAP_FLANGE_D / 2, CAP_FLANGE_EDGE + 0.01, align=BASE
    )
    cap += Pos(c.X, c.Y, cone_bottom) * extrude(Circle(CAP_FLANGE_D / 2), amount=cone, taper=45)
    cap += Pos(c.X, c.Y, cone_bottom + cone - 0.01) * Cylinder(
        CAP_D / 2, f.top_out + CAP_PROUD - cone_bottom - cone + 0.01, align=BASE
    )
    cap.label = "button"
    return cast(Part, cap)


def feet(f: Frame) -> list[Part]:
    body = case.foot()
    x = f.x_cover - 17
    return [Pos(sx * x, sy * 42, f.z_bottom - case.FOOT_H) * body for sx in (-1, 1)
            for sy in (-1, 1)]  # fmt: skip


@dataclass
class Assembly:
    frame: Frame
    base: Part
    covers: dict[str, Cover]
    button: Part


def assemble() -> Assembly:
    f = frame(case.layout(load(case.STEP)))
    return Assembly(f, base(f), {n: cover(f, d) for n, d in case.COVERS.items()}, button(f))


# Fit and process checks


class Report:
    def __init__(self) -> None:
        self.lines: list[str] = []
        self.failures: list[str] = []

    def check(self, ok: bool, msg: str) -> None:
        self.lines.append(f"  {'ok  ' if ok else 'FAIL'}  {msg}")
        if not ok:
            self.failures.append(msg)


def _overlap(a: Shape, b: Shape) -> float:
    inter = a & b
    return sum(s.volume for s in inter.solids()) if inter is not None else 0.0


def _gap(solid: Shape, others: Sequence[tuple[str, Shape]], gap: float) -> list[str]:
    sb = solid.bounding_box()
    hits = []
    for name, other in others:
        ob = other.bounding_box()
        if (sb.min.X - gap < ob.max.X and ob.min.X - gap < sb.max.X
                and sb.min.Y - gap < ob.max.Y and ob.min.Y - gap < sb.max.Y
                and sb.min.Z - gap < ob.max.Z and ob.min.Z - gap < sb.max.Z):  # fmt: skip
            d = solid.distance_to(other)
            if d < gap - 1e-6:
                hits.append(f"{name} {d:.2f}")
    return hits


def check(a: Assembly) -> Report:
    r = Report()
    f, board = a.frame, a.frame.board
    parts = [(ref, p) for ref, p in board.parts.items()]
    r.lines.append("parts")
    for part in (a.base, *(c.part for c in a.covers.values()), a.button):
        bb = part.bounding_box()
        r.check(part.is_valid and len(part.solids()) == 1,
                f"{part.label}: {len(part.solids())} valid solid, "
                f"{bb.size.X:.1f} x {bb.size.Y:.1f} x {bb.size.Z:.1f} mm, "
                f"{part.volume * 2.68e-3:.0f} g")  # fmt: skip
    case_bb = Compound([a.base, a.covers["passive"].part]).bounding_box()
    r.lines.append(f"  case {case_bb.size.X:.1f} x {case_bb.size.Y:.1f} x {case_bb.size.Z:.1f} mm")

    r.lines.append("fit")
    r.check(_overlap(a.base, board.body) < 1e-3, "board body clears the base")
    hits = _gap(a.base, parts, 0.15)
    r.check(not hits, "parts clear the base by 0.15 mm" + (": " + ", ".join(hits) if hits else ""))
    for name, c in a.covers.items():
        hits = _gap(c.part, parts, 0.3)
        r.check(not hits and _overlap(c.part, a.base) < 1e-3,
                f"cover {name} sits on the base and clears the board by 0.3 mm"
                + (": " + ", ".join(hits) if hits else ""))  # fmt: skip
        lid = c.part & (Pos(0, 0, f.top_in) * Box(400, 200, 10, align=BASE))
        hits = _gap(lid, parts, 1.0)
        r.check(
            not hits,
            f"cover {name} top is 1 mm above every part" + (": " + ", ".join(hits) if hits else ""),
        )
    # The cover's ends are slabs outside everything else, so it lowers straight on.
    inner = f.x_cover - COVER_SHEET.t
    widest = max(abs(v) for s in (a.base, *(p for _, p in parts))
                 for v in (s.bounding_box().min.X, s.bounding_box().max.X))  # fmt: skip
    r.check(widest < inner, f"covers lower straight on: {inner - widest:.2f} mm inside their ends")
    # The board lowers in if nothing of the base stands over any part's footprint.
    over = []
    for ref, p in [("pcb", board.body), *parts]:
        b = p.bounding_box()
        prism = Pos(b.center().X, b.center().Y, b.min.Z) * Box(b.size.X, b.size.Y, 50, align=BASE)
        if _overlap(prism, a.base) > 1e-3:
            over.append(ref)
    r.check(
        not over, "board lowers vertically into the base" + (": " + ", ".join(over) if over else "")
    )
    r.lines.append("openings")
    for o in f.lay.openings:
        size = USB_C_METAL if o.kind == "usb-c" else (o.width - 0.2, o.height - 0.2)
        plug = Pos(o.x, o.y_face + (5 if o.y_face > 0 else -5) / 2, o.z) * Box(size[0], 5, size[1])
        r.check(_overlap(plug, a.base) < 1e-3, f"{o.label or o.ref}: plug metal passes the wall")
    for ref in case.LIGHTS:
        b = f.board.box(ref)
        beam = Pos(b.center().X, b.min.Y - 2.5, b.center().Z) * Box(
            LIGHT_EMITTER[0], 5, LIGHT_EMITTER[1]
        )
        r.check(_overlap(beam, a.base) < 1e-3, f"{ref} shines through its window")

    r.lines.append("ports")
    for o in f.lay.openings:
        if o.kind != "usb-c":
            continue
        outside = f.y_out - abs(o.y_face)
        shoulder = case.MATED_SHOULDER[o.ref]
        r.check(outside + BEND_TOL <= shoulder + 0.05,
                f"{o.label}: wall face {outside:.2f} mm past the mouth, {outside + BEND_TOL:.2f} at "
                f"+{BEND_TOL} bend error; plug shoulder {shoulder:.2f}")  # fmt: skip

    r.lines.append("button and display")
    switch = board.parts[case.BUTTON]
    r.check(abs(a.button.distance_to(switch)) < 1e-3 and _overlap(a.button, switch) < 1e-3,
            "button cap rests on the switch")  # fmt: skip
    others = [(ref, p) for ref, p in parts if ref != case.BUTTON]
    for travel in (0.0, 0.5):
        hits = _gap(Pos(0, 0, -travel) * a.button, others, 0.2)
        r.check(not hits, f"button at {travel} mm travel clears parts by 0.2 mm"
                + (": " + ", ".join(hits) if hits else ""))  # fmt: skip
    for name, c in a.covers.items():
        r.check(_overlap(a.button, c.part) < 1e-3 and _overlap(Pos(0, 0, CAP_LIFT + 0.05) * a.button, c.part) > 1e-3,
                f"cover {name} keeps the cap captive with {CAP_LIFT} mm free lift")  # fmt: skip
        if c.display is not None:
            hits = _gap(c.display, [*parts, ("cover", c.part), ("button", a.button)], 0.3)
            r.check(not hits, "OLED module clears parts, cover and button by 0.3 mm"
                    + (": " + ", ".join(hits) if hits else ""))  # fmt: skip

    r.lines.append("Fabrication limits")
    _process(r, a)
    return r


def _process(r: Report, a: Assembly) -> None:
    f = a.frame
    bt, ct = BASE_SHEET, COVER_SHEET
    r.check(f.top_in - f.z_bottom >= MIN_FLANGE[bt.t] and f.tab_top - f.z_bottom >= MIN_FLANGE[bt.t],
            f"base flanges {f.tab_top - f.z_bottom:.1f} mm and longer")  # fmt: skip
    r.check(
        f.top_out - (f.z_bottom + LIFT) >= MIN_FLANGE[ct.t], "cover ends meet the minimum flange"
    )
    worst = min(o.z - o.height / 2 for o in f.lay.openings) - f.plate_top
    lights = (
        min(f.board.box(ref).center().Z for ref in case.LIGHTS) - LIGHT_WINDOW[1] / 2 - f.plate_top
    )
    r.check(min(worst, lights) >= HOLE_TO_BEND[bt.t][1],
            f"wall cutouts {min(worst, lights):.2f} mm above the base's inside face")  # fmt: skip
    nut_hole = NUT_Z - NUT.hole / 2 - f.plate_top
    r.check(nut_hole >= HOLE_TO_BEND[bt.t][0] and f.tab_top - NUT_Z >= NUT.edge
            and TAB_W / 2 >= NUT.edge, "tab nuts keep their bend and edge distances")  # fmt: skip
    _, z, _, width = INTAKES
    end_top = f.top_in - (z + width / 2)
    slot_top = f.top_in - (NUT_Z + SCREW_SLOT[1] / 2)
    r.check(min(end_top, slot_top) >= HOLE_TO_BEND[ct.t][0],
            f"cover end cutouts {min(end_top, slot_top):.2f} mm below the top's inside face")  # fmt: skip
    edge = min(min(f.x_tab - abs(h.X), (f.y_out - bt.ro) - abs(h.Y)) for h in f.standoffs)
    r.check(edge >= STANDOFF.edge, f"standoffs {edge:.2f} mm from the plate edge and wall bends")
    skipped = [h for h in f.lay.holes if h not in f.standoffs]
    if skipped:
        r.lines.append("  note  board holes without a standoff (too close to a wall bend): "
                       + ", ".join(f"({h.X:.1f}, {h.Y:.1f})" for h in skipped))  # fmt: skip
    # Laser marking: strokes, text height and distance from holes and bends.
    for name, c in a.covers.items():
        holes = Compound([Face(w) for h in c.holes for w in [h.face().outer_wire()]])
        near = [m for m in c.marks if m.distance_to(holes) < MARK_CLEAR - 1e-3]
        r.check(not near, f"cover {name} marks {MARK_CLEAR} mm from holes")
        flat = f.x_cover - ct.ro - MARK_CLEAR
        r.check(all(abs(v) <= flat for m in c.marks for v in (m.bounding_box().min.X,
                                                             m.bounding_box().max.X)),
                f"cover {name} marks {MARK_CLEAR} mm inside the bends")  # fmt: skip
        webs = [
            h for h in c.holes if min(h.bounding_box().size.X, h.bounding_box().size.Y) < MIN_HOLE
        ]
        r.check(not webs, f"cover {name} holes {MIN_HOLE} mm or larger")


# Outputs


def export(a: Assembly, out: Path) -> list[Path]:
    out.mkdir(parents=True, exist_ok=True)
    written = []
    for name, c in a.covers.items():
        suffix = "" if name == "passive" else f"-{name}"
        step = out / f"iasis-sheet{suffix}.step"
        export_step(Compound([a.base, c.part], label=f"iasis sheet{suffix}"), step)
        dxf = out / f"iasis-cover{suffix}-marking.dxf"
        _marking_dxf(a.frame, c, dxf)
        written += [step, dxf]
    drawing = out / "iasis-base-hardware.png"
    _drawing(a, drawing)
    printing.export("button", a.button, out)
    return [*written, drawing, out / "button.stl"]


def _marking_dxf(f: Frame, c: Cover, path: Path) -> None:
    """The cover's top as it looks once bent: outline and holes, and each mark as closed
    polylines to fill solid."""
    doc = new_dxf("R2010", units=units.MM)
    doc.layers.add("outline", color=7)
    doc.layers.add("marking", color=1)
    space = doc.modelspace()
    outline = Rectangle(2 * f.x_cover, 2 * f.y_cover)
    for layer, sketches in (("outline", (outline, *c.holes)), ("marking", c.marks)):
        for wire in (w for s in sketches for w in s.wires()):
            curve = BRepAdaptor_CompCurve(wire.wrapped)
            points = GCPnts_QuasiUniformDeflection(curve, 0.01)
            xy = [
                (p.X(), p.Y()) for p in (points.Value(i) for i in range(1, points.NbPoints() + 1))
            ]
            space.add_lwpolyline(xy[:-1] if xy[0] == xy[-1] else xy, close=True,
                                 dxfattribs={"layer": layer})  # fmt: skip
    doc.saveas(path)


def _drawing(a: Assembly, path: Path) -> None:
    """Hardware callouts: the base from above and from its right end, each fastener marked
    where it sits."""
    import textwrap

    from PIL import Image, ImageDraw, ImageFont

    f = a.frame
    scale, margin = 10.0, 110
    font = ImageFont.load_default(size=30)
    small = ImageFont.load_default(size=26)
    views = (
        ("From above", (0, 0, 500), (0, 1, 0)),
        ("Right end, from outside (nuts)", (500, 0, 0), (0, 0, 1)),
    )

    def flat(shape: Part, origin: tuple[float, float, float], up: tuple[float, float, float]):
        visible, _ = shape.project_to_viewport(origin, up, (0, 0, 0))
        return list(visible)

    def marker(point: tuple[float, float, float], origin, up) -> Vector:
        return Compound(flat(Pos(*point) * Box(0.2, 0.2, 0.2), origin, up)).bounding_box().center()

    drawn = [(title, origin, up, flat(a.base, origin, up)) for title, origin, up in views]
    boxes = [Compound(edges).bounding_box() for *_, edges in drawn]
    width = int(max(b.size.X for b in boxes) * scale + 2 * margin)
    tops, y = [], margin + 50
    for b in boxes:
        tops.append(y)
        y += int(b.size.Y * scale) + margin + 50
    img = Image.new("RGB", (width, y + 560), "white")
    d = ImageDraw.Draw(img)
    for (title, origin, up, edges), b, top in zip(drawn, boxes, tops, strict=True):

        def px(v: Vector, b=b, top=top) -> tuple[float, float]:
            return (margin + (v.X - b.min.X) * scale, top + (b.max.Y - v.Y) * scale)

        d.text((margin, top - 50), title, fill="black", font=font)
        for e in edges:
            d.line([px(e.position_at(i / 24)) for i in range(25)], fill="black", width=2)
        from_above = origin[2] > 0
        for s in f.standoffs if from_above else ():
            x, yy = px(marker((s.X, s.Y, f.plate_top + STANDOFF.body_h), origin, up))
            d.ellipse((x - 24, yy - 24, x + 24, yy + 24), outline="red", width=4)
            d.text((x + 30, yy - 30), STANDOFF.code, fill="red", font=small)
        # The end view shows the near end's nuts only.
        for sx, yc in (t for t in f.tabs if from_above or t[0] > 0):
            x, yy = px(marker((sx * f.x_tab, yc, NUT_Z), origin, up))
            d.rectangle((x - 16, yy - 16, x + 16, yy + 16), outline="blue", width=4)
            d.text((x + 24, yy + 10), NUT.code, fill="blue", font=small)
    notes = [
        "iasis base. Aluminium 5052, 1.0 mm. All bends inside R0.8 (outside R1.8).",
        (
            f"{len(f.standoffs)} x {STANDOFF.code} through-hole standoff, steel, in "
            f"{STANDOFF.hole} mm holes: flush on the underside, body inside."
        ),
        (
            f"{len(f.tabs)} x {NUT.code} press nut, steel, in {NUT.hole} mm holes in the end "
            f"tabs, {NUT_Z - f.z_bottom:.1f} mm above the underside: flush outside, head inside."
        ),
        "Press the hardware after anodizing. The STEP fuses each fastener to the part.",
        "Cover: aluminium 5052, 1.2 mm, no hardware. Laser marking on its top per the DXF.",
    ]
    lines = [w for n in notes for w in textwrap.wrap(n, 92)]
    for i, line in enumerate(lines):
        d.text((margin, y + i * 44), line, fill="black", font=font)
    img.save(path)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, help="output directory")
    parser.add_argument("--check-only", action="store_true", help="run the checks, write nothing")
    args = parser.parse_args()
    a = assemble()
    report = check(a)
    print("\n".join(report.lines))
    if report.failures:
        raise SystemExit(f"{len(report.failures)} check(s) failed")
    if not args.check_only:
        out = args.output.resolve() if args.output else BUILD
        for p in export(a, out):
            print(p)


if __name__ == "__main__":
    main()
