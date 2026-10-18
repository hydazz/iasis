"""Iasis enclosure: a plate that carries the board and a cover shell that drops over it.

Six M3 screws come up through the plate and the board's M3 holes into heat-set
inserts in the cover's pillars, clamping the board between the plate's standoffs and
the pillars. The cover carries every opening, so the seam runs round the underside,
except where the plate rises into a jack's slot. Covers come passive, with a 40 mm fan
over the CM5 cooler, or with an OLED module under a window in the top. The plate is the
same for all of them.

Geometry is in board coordinates (board.py). The board STEP supplies the outline, the
holes, every connector, light and button; the numbers here are fit, form and the parts
the STEP does not contain.
"""

import math
import re
from dataclasses import dataclass
from functools import cached_property

from build123d import (
    Align,
    Axis,
    BoundBox,
    Box,
    Circle,
    Compound,
    Cylinder,
    Face,
    FontStyle,
    GeomType,
    Part,
    Plane,
    Pos,
    Rectangle,
    RectangleRounded,
    Rot,
    SlotOverall,
    Text,
    Vector,
    chamfer,
    extrude,
    fillet,
)

from board import Board

# Fit
CLEARANCE = 0.55  # board edge to the cover's inner wall; USB-C faces stand 0.22 proud
PLATE_FIT = 0.25  # plate edge to the cover's inner wall
HEADROOM = 2.0  # tallest part or envelope to the cover's inner top
MIN_GAP = 0.3  # smallest gap between the case and a part
FIT = 0.15  # a jack to the wall round it, and a gate to the slot it fills

# Form
WALL = 1.6
TOP = 2.4
PLATE = 3.0
STANDOFF = 5.0  # plate to board; through-hole tails reach 2.3 mm below the board
EDGE_R = 3.0  # cover top perimeter
BASE_CHAMFER = 0.8

# Fasteners: M3 x 12 ISO 7380 button heads from below into M3 x 5.7 heat-set inserts
HOLE_D = 3.2  # the board's M3 drill, which identifies the case holes
SCREW_CLEAR = 3.4
HEAD_D, HEAD_DEPTH = 6.2, 2.0
INSERT_D, INSERT_DEPTH = 4.0, 6.5
PILLAR_D = 6.6  # inside the mounting holes' 7 mm courtyard
STANDOFF_D = 7.0
FOOT_D, FOOT_POCKET = 12.4, 1.0  # stick-on 12 mm feet

# Openings in the front and rear walls. A Type-C plug seats only when its overmold
# (spec maximum 12.35 x 6.5) reaches the receptacle mouth, and these receptacles stop
# 0.22 mm past the board edge, so each gets an overmold-sized opening. The RJ45 and the
# power DIN reach the outer face through a slot that hugs the jack (the DIN's round
# boss). The cover drops past them, so each slot runs down to the rim and a gate on the
# plate fills it under the jack.
OVERMOLD = (12.35, 6.5, 1.5)  # width, height, corner radius
OVERMOLD_FIT = 0.25
GATE_TIP = 0.6  # narrowest gate corner beside a round boss
PORTS = {  # reference: (kind, label)
    "J401": ("usb-c", "1"),
    "J501": ("usb-c", "2"),
    "J601": ("usb-c", "3"),
    "J701": ("usb-c", "4"),
    "J801": ("usb-c", "5"),
    "J901": ("usb-c", "HOST"),
    "J151": ("jack", "LAN"),
    "J1001": ("round", "24V"),
}
GATED = ("jack", "round")

# Side-emitting LEDs at the board edge shine through windows no larger than their
# emitting face. The RPIBOOT button is pressed through a pinhole; the confirm button
# through a tongue in the top.
LIGHTS = ("D1170", "D1171", "D1172", "D1173", "D1174", "D1175", "D111")  # host, targets, CM5
WINDOW = (3.0, 1.6)
BOOT, PINHOLE_D = "SW111", 1.6
BUTTON = "SW1171"
BUTTON_TONGUE = (16.0, 9.0)  # length (hinge to free end), width
BUTTON_SLOT, BUTTON_SKIN, BUTTON_GAP, BUTTON_PIN = 0.8, 1.4, 0.4, 4.0

# Parts the board STEP does not contain
CM5 = "J101"  # carries the CM5 module model
# Raspberry Pi Cooler for CM5: footprint x, y and height above the module PCB. It has
# no published model; its underside is taken to clear the module's tallest part.
COOLER = (41.0, 56.0, 12.7)


@dataclass(frozen=True)
class Fan:
    name: str
    size: float
    depth: float
    pitch: float  # mounting-hole spacing
    screw: float  # hole for the fan's screws through the top


FAN_GAP = 2.0  # cooler top to fan frame
FAN = Fan("Noctua NF-A4x10 5V PWM", 40, 10, 32, 3.4)


@dataclass(frozen=True)
class Display:
    """An I2C OLED module hung glass up under the top from screw posts, on a cable to the
    board's JST-SH socket. Positions are from the module PCB's centre, x along the case,
    y toward the rear."""

    name: str
    pcb: tuple[float, float, float]  # x, y, thickness
    glass: tuple[float, float, float, float, float]  # centre x, y; size x, y; thickness
    active: tuple[float, float, float, float]  # visible pixels: centre x, y; size x, y
    back: float  # tallest part under the PCB, connector included
    holes: tuple[tuple[float, float], ...]
    pilot: float  # for self-tapping screws through the module's holes


# Adafruit 938, from its board file, fab print and STEP model. The flex edge faces the
# front, so the picture reads from in front of the box.
OLED = Display(
    name="Adafruit 938 1.3in 128x64 OLED",
    pcb=(35.56, 33.02, 1.57),
    glass=(0.0, 0.06, 34.5, 23.0, 1.5),
    active=(0.0, 2.11, 29.42, 14.70),
    back=3.0,  # the JST-SH sockets
    holes=tuple((sx * 15.24, sy * 13.97) for sx in (-1, 1) for sy in (-1, 1)),
    pilot=1.7,  # M2 x 5
)
DISPLAY_OVER = ("J501", "J601")  # centred between ports 2 and 3
DISPLAY_INSET = 2.0  # module PCB to the inside of the front wall
WINDOW_MARGIN = 0.5  # window beyond the active area, each side
WINDOW_BEVEL = 1.4  # 45 degree chamfer round the window's outer edge
GLASS_GAP = 0.4  # glass face to the inside of the top; clears the tape over the flex
POST_D = 4.0  # leaves 0.4 mm to the glass beside the holes
POST_SKIN = 0.8  # top left above the pilot holes


@dataclass(frozen=True)
class Variant:
    fan: Fan | None = None
    display: Display | None = None


COVERS = {"passive": Variant(), "fan": Variant(fan=FAN), "display": Variant(display=OLED)}

# Ventilation: slot fields in the top over the heat, vertical slots low in the end and
# rear walls for intake. The front stays clear for the ports.
SLOT_W, SLOT_PITCH = 2.0, 4.0
WALL_SLOT_W, WALL_SLOT_PITCH, WALL_SLOT_H = 2.0, 5.0, 10.0
WALL_SLOT_CLEAR = 3.0  # rear slots to any other opening or label
POWER_PARTS = re.compile(r"^[LC]1[2-6]\d\d$")  # the five port power stages' tall parts
NVME = "J171"  # M.2 socket, carrying the 2280 card envelope

# Marks
TEXT_DEPTH = 0.6
WORDMARK = "iasis"

BASE = (Align.CENTER, Align.CENTER, Align.MIN)  # centred, standing on the given point
Z_UP = (0, 0, 1)  # the outline comes from the board's underside and faces down


@dataclass(frozen=True)
class Opening:
    """A hole in the front or rear wall for the part `ref`, centred on (x, z). y_face is
    the part's face toward that wall, y_wall the wall's outer face."""

    ref: str
    kind: str  # usb-c, jack or round (PORTS), light or pin
    label: str | None
    x: float
    z: float
    width: float
    height: float
    radius: float
    y_face: float
    y_wall: float

    @property
    def front(self) -> bool:
        return self.y_wall < 0

    @property
    def inward(self) -> int:
        """+1 or -1: the y direction from this wall into the box."""
        return 1 if self.front else -1

    def plane(self, y: float) -> Plane:
        """A plane at depth y on the opening's centre, facing out of the box, with x
        reading left to right from outside and y up."""
        return Plane(origin=(self.x, y, self.z), x_dir=(self.inward, 0, 0),
                     z_dir=(0, -self.inward, 0))  # fmt: skip

    def profile(self) -> Face:
        if self.kind in ("round", "pin"):
            return Circle(self.width / 2)
        if self.kind == "light":
            return SlotOverall(self.width, self.height)
        return RectangleRounded(self.width, self.height, self.radius)


@dataclass
class Layout:
    """Everything the parts are built from, derived once from the board."""

    board: Board
    seat: float  # top surface the parts sit on
    cavity: Face  # cover inner outline, z = 0
    length: float  # outer
    width: float
    z_bottom: float  # plate underside and cover rim
    corner_r: float  # outer vertical corners: the board's plus clearance and wall
    holes: list[Vector]
    cooler: Part
    parts_top: float  # tallest placed part

    @property
    def y_front(self) -> float:
        return -self.width / 2

    @property
    def y_rear(self) -> float:
        return self.width / 2

    @cached_property
    def openings(self) -> list[Opening]:
        out = [_port(self, ref, kind, label) for ref, (kind, label) in PORTS.items()]
        out += [_light(self, ref) for ref in LIGHTS]
        out.append(_pinhole(self))
        return out

    def top_inside(self, fan: Fan | None) -> float:
        top = max(self.parts_top, self.cooler.bounding_box().max.Z) + HEADROOM
        if fan:
            top = max(top, self.cooler.bounding_box().max.Z + FAN_GAP + fan.depth)
        return round(top * 2 + 0.49) / 2  # up to the next 0.5 mm


@dataclass
class Cover:
    part: Part
    intrusions: dict[str, Part]  # solids that reach into the cavity, for the fit check
    fan: Part | None
    display: Part | None  # the module's envelope


def layout(board: Board) -> Layout:
    placed = board.placed()
    seats = sorted(board.rough(ref).min.Z for ref in placed)
    module_solids = board.parts[CM5].solids()
    module = max(module_solids, key=lambda s: s.bounding_box().size.X).bounding_box()
    underside = max(s.bounding_box().max.Z for s in module_solids)
    w, d, h = COOLER
    cooler = Pos(module.center().X, module.center().Y, underside) * Box(
        w, d, module.max.Z + h - underside, align=BASE
    )
    cooler.label = "cooler envelope"
    size = board.size
    corner = min(e.radius for e in board.outline().edges() if e.geom_type == GeomType.CIRCLE)
    return Layout(
        board=board,
        seat=seats[len(seats) // 2],
        cavity=Face(board.outline().outer_wire().offset_2d(CLEARANCE)),
        length=size.X + 2 * (CLEARANCE + WALL),
        width=size.Y + 2 * (CLEARANCE + WALL),
        z_bottom=-(STANDOFF + PLATE),
        corner_r=corner + CLEARANCE + WALL,
        holes=board.holes(HOLE_D),
        cooler=cooler,
        parts_top=max(board.rough(ref).max.Z for ref in placed),
    )


def _facing(lay: Layout, ref: str) -> tuple[BoundBox, float, float]:
    """A part's exact box, the y of its face toward the nearer long wall, and the y of
    that wall's outer face."""
    bb = lay.board.box(ref)
    if bb.center().Y > 0:
        return bb, bb.max.Y, lay.y_rear
    return bb, bb.min.Y, lay.y_front


def _port(lay: Layout, ref: str, kind: str, label: str) -> Opening:
    bb, y_face, y_wall = _facing(lay, ref)
    if kind == "usb-c":
        w, h, r = OVERMOLD
        f = OVERMOLD_FIT
        z = (lay.seat + bb.max.Z) / 2
        return Opening(ref, kind, label, bb.center().X, z, w + 2 * f, h + 2 * f, r + f,
                       y_face, y_wall)  # fmt: skip
    if kind == "round":
        boss = max((f for f in lay.board.parts[ref].faces()
                    if f.geom_type == GeomType.CYLINDER
                    and abs(f.axis_of_rotation.direction.Y) > 0.99
                    and min(abs(f.bounding_box().max.Y - y_face),
                            abs(f.bounding_box().min.Y - y_face)) < 0.05),
                   key=lambda f: f.radius)  # fmt: skip
        c, d = boss.axis_of_rotation.position, 2 * (boss.radius + FIT)
        return Opening(ref, kind, label, c.X, c.Z, d, d, d / 2, y_face, y_wall)
    z_lo, z_hi = lay.seat - FIT, bb.max.Z + FIT
    return Opening(ref, kind, label, bb.center().X, (z_lo + z_hi) / 2, bb.size.X + 2 * FIT,
                   z_hi - z_lo, FIT, y_face, y_wall)  # fmt: skip


def _light(lay: Layout, ref: str) -> Opening:
    bb, y_face, y_wall = _facing(lay, ref)
    w, h = min(WINDOW[0], bb.size.X), min(WINDOW[1], bb.size.Z)
    return Opening(ref, "light", None, bb.center().X, bb.center().Z, w, h, h / 2, y_face, y_wall)


def _pinhole(lay: Layout) -> Opening:
    """Centred on the actuator: the faces at the button's front."""
    _, y_face, y_wall = _facing(lay, BOOT)
    tip = [f for f in lay.board.parts[BOOT].faces()
           if min(abs(f.bounding_box().max.Y - y_face), abs(f.bounding_box().min.Y - y_face)) < 0.01
           and abs(f.bounding_box().center().Y - y_face) < 0.2]  # fmt: skip
    c = Compound(tip).bounding_box().center()
    return Opening(BOOT, "pin", "BOOT", c.X, c.Z, PINHOLE_D, PINHOLE_D, PINHOLE_D / 2,
                   y_face, y_wall)  # fmt: skip


def gates(lay: Layout) -> dict[str, Part]:
    """Plate pieces that fill each jack's slot under the jack, flush with the cover."""
    out = {}
    for o in lay.openings:
        if o.kind not in GATED:
            continue
        width = o.width - 2 * FIT
        if o.kind == "jack":
            top = o.z - o.height / 2
        else:  # stop where the corners beside the boss would thin below GATE_TIP
            r = o.width / 2
            top = o.z - math.sqrt(r * r - (width / 2 - GATE_TIP) ** 2)
        edge = o.y_wall + o.inward * (WALL + PLATE_FIT)  # the plate's edge
        y0, y1 = sorted((o.y_wall, edge))
        gate = Pos(o.x, (y0 + y1) / 2, lay.z_bottom) * Box(width, y1 - y0, top - lay.z_bottom,
                                                           align=BASE)  # fmt: skip
        gate = chamfer(gate.edges().filter_by(Axis.X).group_by(Axis.Z)[0]
                       .sort_by(Axis.Y)[0 if o.front else -1], BASE_CHAMFER)  # fmt: skip
        gate += Pos(o.x, edge + o.inward * 0.25, lay.z_bottom) * Box(width, 0.5, PLATE,
                                                                    align=BASE)  # fmt: skip
        if o.kind == "round":
            gate -= Pos(o.x, (y0 + y1) / 2, o.z) * Rot(X=90) * Cylinder(o.width / 2, y1 - y0 + 2)
        gate.label = f"gate {o.ref}"
        out[o.ref] = gate
    return out


def plate(lay: Layout) -> Part:
    z0 = lay.z_bottom
    outline = Face(lay.cavity.outer_wire().offset_2d(-PLATE_FIT))
    body = extrude(Pos(0, 0, z0) * outline, amount=PLATE, dir=Z_UP)
    body += Compound([Pos(h.X, h.Y, z0 + 1) * Cylinder(STANDOFF_D / 2, STANDOFF + PLATE - 1,
                                                       align=BASE) for h in lay.holes])  # fmt: skip
    body += Compound(list(gates(lay).values()))
    cuts = []
    for h in lay.holes:
        cuts.append(Pos(h.X, h.Y, z0) * Cylinder(SCREW_CLEAR / 2, -z0, align=BASE))
        cuts.append(Pos(h.X, h.Y, z0) * Cylinder(HEAD_D / 2, HEAD_DEPTH, align=BASE))
    for sx in (-1, 1):
        for sy in (-1, 1):
            at = Pos(sx * (lay.length / 2 - 18), sy * (lay.width / 2 - 14), z0)
            cuts.append(at * Cylinder(FOOT_D / 2, FOOT_POCKET, align=BASE))
    cuts += _labels(lay)  # those under a jack land on its gate
    body -= Compound(cuts)
    body.label = "plate"
    return body


def cover(lay: Layout, name: str, variant: Variant) -> Cover:
    fan = variant.fan
    top_in = lay.top_inside(fan)
    top_out = top_in + TOP
    z0 = lay.z_bottom

    shell = extrude(Pos(0, 0, z0) * RectangleRounded(lay.length, lay.width, lay.corner_r),
                    amount=top_out - z0)  # fmt: skip
    shell = fillet(shell.edges().group_by(Axis.Z)[-1], EDGE_R)
    shell = chamfer(shell.edges().group_by(Axis.Z)[0], BASE_CHAMFER)
    shell -= extrude(Pos(0, 0, z0 - 1) * lay.cavity, amount=top_in - z0 + 1, dir=Z_UP)

    # Everything that reaches into the cavity, then everything cut, then the button
    # pin, which has to join the tongue after the tongue is thinned.
    intrusions: dict[str, Part] = {}
    cuts: list[Part] = []
    for i, h in enumerate(lay.holes):
        intrusions[f"pillar {i + 1}"] = Pos(h.X, h.Y, lay.seat) * Cylinder(
            PILLAR_D / 2, top_in - lay.seat + 0.5, align=BASE
        )
        cuts.append(Pos(h.X, h.Y, lay.seat) * Cylinder(INSERT_D / 2, INSERT_DEPTH, align=BASE))
    shell += Compound(list(intrusions.values()))

    wall = [_wall_cut(lay, o) for o in lay.openings] + _labels(lay)
    cuts += wall
    cuts += _wall_slots(lay, [c.bounding_box() for c in wall])
    cuts += [_slots(f, top_in - 1, TOP + 2) for f in _vent_fields(lay, fan)]
    cuts += _wordmark(lay, top_out)
    tongue, pin = _button(lay, top_in, top_out)
    cuts += tongue

    fan_part = None
    if fan:
        c = lay.cooler.bounding_box().center()
        fan_part = Pos(c.X, c.Y, top_in) * Box(
            fan.size, fan.size, fan.depth, align=(Align.CENTER, Align.CENTER, Align.MAX)
        )
        fan_part.label = f"{fan.name} envelope"
        cuts.append(_fan_grille(c, fan, top_in - 1, TOP + 2))

    display_part = None
    if variant.display:
        window, posts, pilots, display_part = _display(lay, variant.display, top_in, top_out)
        cuts += [window, *pilots]
        shell += Compound(posts)
        intrusions |= {f"display post {i + 1}": p for i, p in enumerate(posts)}

    shell -= Compound(cuts)
    shell += pin
    intrusions["button pin"] = pin
    shell.label = f"cover {name}"
    return Cover(part=shell, intrusions=intrusions, fan=fan_part, display=display_part)


def _display(
    lay: Layout, d: Display, top_in: float, top_out: float
) -> tuple[Part, list[Part], list[Part], Part]:
    """The window with its outer bevel, the posts the module hangs from with their pilot
    holes, and the module's envelope."""
    pw, pd, pt = d.pcb
    x = sum(lay.board.box(ref).center().X for ref in DISPLAY_OVER) / len(DISPLAY_OVER)
    y = lay.y_front + WALL + DISPLAY_INSET + pd / 2
    ax, ay, aw, ah = d.active
    w, h = aw + 2 * WINDOW_MARGIN, ah + 2 * WINDOW_MARGIN
    window = extrude(Pos(x + ax, y + ay, top_in - 1) * Rectangle(w, h), amount=TOP + 2)
    bevel = Pos(x + ax, y + ay, top_out - WINDOW_BEVEL) * Rectangle(w, h)
    window += extrude(bevel, amount=WINDOW_BEVEL + 1, taper=-45)

    gx, gy, gw, gh, gt = d.glass
    face = top_in - GLASS_GAP - gt  # the module PCB's top face
    posts, pilots = [], []
    for hx, hy in d.holes:
        at = Pos(x + hx, y + hy, face)
        posts.append(at * Cylinder(POST_D / 2, top_in - face + 0.5, align=BASE))
        pilots.append(Pos(0, 0, -1) * at * Cylinder(d.pilot / 2, top_out - POST_SKIN - face + 1,
                                                     align=BASE))  # fmt: skip

    module = Pos(x, y, face) * Box(pw, pd, pt, align=(Align.CENTER, Align.CENTER, Align.MAX))
    module += Pos(x, y, face - pt) * Box(
        pw, pd, d.back, align=(Align.CENTER, Align.CENTER, Align.MAX)
    )
    module += Pos(x + gx, y + gy, face) * Box(gw, gh, gt, align=BASE)
    module.label = f"{d.name} envelope"
    return window, posts, pilots, module


def _wall_cut(lay: Layout, o: Opening) -> Part:
    start = o.plane(o.y_wall - o.inward)  # 1 mm outside
    profile = o.profile()
    if o.kind in GATED:  # open to the rim so the cover drops past the jack
        below = o.z - lay.z_bottom + 1
        profile += Rectangle(o.width, below, align=(Align.CENTER, Align.MAX))
    return extrude(start * profile, amount=-(WALL + 2))


def _labels(lay: Layout) -> list[Part]:
    """Port names above front openings; under rear ones, which reach high, and under
    the pinhole, which sits beside a named port. A label underneath is centred between
    the base chamfer and the lowest opening edge above its span."""
    cuts = []
    for o in lay.openings:
        if not o.label:
            continue
        above = o.front and o.kind != "pin"
        size = 5.0 if above else 3.5
        text = Text(o.label, font_size=size, font_style=FontStyle.BOLD)
        if above:
            dz = o.height / 2 + 1.5 + size / 2
        else:
            half = text.bounding_box().size.X / 2
            bottom = min(
                p.z - p.height / 2
                for p in lay.openings
                if p.front == o.front and abs(p.x - o.x) < half + p.width / 2
            )
            dz = (lay.z_bottom + BASE_CHAMFER + bottom) / 2 - o.z
        cuts.append(extrude(o.plane(o.y_wall) * Pos(0, dz) * text, amount=-TEXT_DEPTH))
    return cuts


def _button(lay: Layout, top_in: float, top_out: float) -> tuple[list[Part], Part]:
    """A tongue cut free in the top on three sides and thinned to flex, hinged towards
    -x, with a pin from under its free end down to the switch."""
    sb = lay.board.box(BUTTON)
    sw = sb.center()
    length, width = BUTTON_TONGUE
    s = BUTTON_SLOT
    free_x = sw.X + 3.0
    hinge_x = free_x - length
    u = Pos((hinge_x + free_x + s) / 2, sw.Y, top_in - 1) * Rectangle(length + s, width + 2 * s)
    u -= Pos((hinge_x - 1 + free_x) / 2, sw.Y, top_in - 1) * Rectangle(length + 1, width)
    skin = Pos((hinge_x - 1 + free_x) / 2, sw.Y, top_in - 1) * Box(
        length + 1, width, TOP - BUTTON_SKIN + 1, align=BASE
    )
    pin_bottom = sb.max.Z + BUTTON_GAP
    pin = Pos(sw.X, sw.Y, pin_bottom) * Cylinder(
        BUTTON_PIN / 2, top_out - BUTTON_SKIN - pin_bottom + 0.5, align=BASE
    )
    return [extrude(u, amount=TOP + 2), skin], pin


def _vent_fields(lay: Layout, fan: Fan | None) -> list[tuple[float, float, float, float]]:
    """Top vent rectangles (x0, y0, x1, y1): over the cooler (unless a fan is there),
    the five power stages and the SSD."""
    fields = []
    if not fan:
        cb = lay.cooler.bounding_box()
        fields.append((cb.min.X + 2, cb.min.Y + 3, cb.max.X - 2, cb.max.Y - 3))
    power = [lay.board.rough(ref) for ref in lay.board.placed()
             if POWER_PARTS.match(ref) and lay.board.rough(ref).max.Z - lay.seat > 4]  # fmt: skip
    if power:
        fields.append((min(b.min.X for b in power), min(b.min.Y for b in power),
                       max(b.max.X for b in power), max(b.max.Y for b in power)))  # fmt: skip
    nb = lay.board.box(NVME)
    fields.append((nb.min.X + 3, nb.min.Y + 3, nb.max.X - 3, nb.max.Y - 3))
    return fields


def _slots(field, z: float, depth: float) -> Part:
    x0, y0, x1, y1 = field
    n = int((x1 - x0 + SLOT_PITCH - SLOT_W) // SLOT_PITCH)
    span = (n - 1) * SLOT_PITCH
    xc, yc = (x0 + x1) / 2, (y0 + y1) / 2
    slot = Rot(Z=90) * SlotOverall(y1 - y0, SLOT_W)
    faces = [Pos(xc - span / 2 + i * SLOT_PITCH, yc, z) * slot for i in range(n)]
    return Compound([extrude(f, amount=depth) for f in faces])


def _fan_grille(c: Vector, fan: Fan, z: float, depth: float) -> Part:
    rings = []
    r = fan.size / 2 - 2.5
    while r > 4:
        rings.append(Circle(r) - Circle(r - 2.0))
        r -= 4.0
    grille = rings[0]
    for ring in rings[1:]:
        grille += ring
    for angle in (45, 135):
        grille -= Rot(Z=angle) * Rectangle(fan.size, 2.4)
    for sx in (-1, 1):
        for sy in (-1, 1):
            grille += Pos(sx * fan.pitch / 2, sy * fan.pitch / 2) * Circle(fan.screw / 2)
    return extrude(Pos(c.X, c.Y, z) * grille, amount=depth)


def _wall_slots(lay: Layout, avoid: list[BoundBox]) -> list[Part]:
    """Vertical intake slots low in the end walls, and along the rear wall wherever it
    is clear of openings and labels."""
    z = lay.seat + 1.5 + WALL_SLOT_H / 2
    slot = Rot(Z=90) * SlotOverall(WALL_SLOT_H, WALL_SLOT_W)
    cuts = []
    for sx in (-1, 1):
        plane = Plane.YZ.offset(sx * lay.length / 2)
        cuts += [extrude(plane * Pos(i * WALL_SLOT_PITCH, z) * slot, amount=WALL + 2, both=True)
                 for i in range(-6, 7)]  # fmt: skip
    rear = [b for b in avoid if b.center().Y > 0]
    end = lay.length / 2 - lay.corner_r - WALL_SLOT_CLEAR
    plane = Plane.XZ.offset(-lay.y_rear)
    half = WALL_SLOT_W / 2 + WALL_SLOT_CLEAR
    for i in range(int(2 * end // WALL_SLOT_PITCH) + 1):
        x = -end + i * WALL_SLOT_PITCH
        if all(x + half < b.min.X or x - half > b.max.X for b in rear):
            cuts.append(extrude(plane * Pos(x, z) * slot, amount=WALL + 2, both=True))
    return cuts


def _wordmark(lay: Layout, top_out: float) -> list[Part]:
    """The name, centred near the front left corner."""
    mark = Text(WORDMARK, font_size=8, font_style=FontStyle.BOLD)
    c = mark.bounding_box().center()
    at = Pos(-lay.length / 2 + 32 - c.X, lay.y_front + 13 - c.Y, top_out)
    return [extrude(at * mark, amount=-TEXT_DEPTH)]
