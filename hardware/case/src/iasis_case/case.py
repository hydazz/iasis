"""Iasis enclosure: a lower tray and upper cover split through the USB-C ports.

The board sits on locating posts between a tray and removable cover. Passive and OLED
covers share the tray. Flush top markings are separate solids for multicolour printing.

Geometry is in board coordinates (board.py). The board STEP supplies the outline, the
holes, every connector, light and button; the numbers here are fit, form and the parts
the STEP does not contain.
"""

import math
from dataclasses import dataclass, field
from functools import cached_property
from pathlib import Path
from typing import cast

from build123d import (
    Align,
    Axis,
    BoundBox,
    Box,
    Circle,
    Compound,
    Cone,
    Cylinder,
    Face,
    FontStyle,
    GeomType,
    Kind,
    Part,
    Plane,
    Pos,
    Rectangle,
    RectangleRounded,
    Rot,
    Sketch,
    SlotOverall,
    Text,
    Vector,
    chamfer,
    extrude,
    fillet,
)

from .artwork import icon, wordmark
from .board import Board, load
from .retention import DisplayFrame, Joint

# Fit
CLEARANCE = 0.3  # board edge to inner wall
PORT_FIT = 0.30  # each side of the metal shell
PORT_ERROR = 0.15  # additional axial print and connector-placement allowance
SHOULDER_GAP = 0.05  # minimum fully seated plug shoulder to exterior wall
HEADROOM = 1.2  # tallest part or envelope to the cover's inner top
MIN_GAP = 0.3  # smallest gap between the case and a part

# Form
WALL = 1.2
TOP = 1.6
FLOOR = 2.0
STANDOFF = 3.0  # clear space under the board
TOP_CHAMFER = 0.8  # 45 degrees, supported when the cover prints top down
BASE_CHAMFER = 0.6
PORT_EDGE_R = 0.5  # outer edge of each USB-C opening

# Board support
HOLE_D = 3.2  # the board's M3 drill, which identifies the case holes
LOCATOR_D, LOCATOR_OVER = 2.9, 0.8
BOARD_HOLD_GAP = 0.15
PILLAR_D = 6.6  # inside the mounting holes' 7 mm courtyard
STANDOFF_D = 7.0

PORTS = {  # reference: (kind, label)
    "J401": ("usb-c", "1"),
    "J501": ("usb-c", "2"),
    "J601": ("usb-c", "3"),
    "J701": ("usb-c", "4"),
    "J801": ("usb-c", "5"),
    "J901": ("usb-c", "HOST"),
    "J1001": ("usb-c", "POWER"),
}

# Minimum fully mated shoulder-to-mouth distances, not generic receptacle depths.
# Molex SD-105450-001: USB-IF exposed shell 6.51 min - mouth depth (4.70 + 0.20).
# GCT USB4105 drawing: 1.85 min in the plug/receptacle mating view.
MATED_SHOULDER = {ref: 1.85 if ref == "J1001" else 6.51 - 4.90 for ref in PORTS}

# Side-emitting LEDs at the board edge shine through windows no larger than their
# emitting face. A captive round cap operates the target-control switch.
LIGHTS = ("D1170", "D1171", "D1172", "D1173", "D1174", "D1175")  # status, targets
WINDOW = (3.0, 1.6)
BUTTON = "SW1171"
BUTTON_D, BUTTON_FIT = 10.0, 0.25  # radial fit in the cover
BUTTON_FLANGE_D = 13.0
BUTTON_FLANGE_H = (BUTTON_FLANGE_D - BUTTON_D) / 2  # printable 45 degree shoulder
BUTTON_ROOF_GAP, BUTTON_PROUD, BUTTON_PIN = 0.2, 0.2, 4.0
BUTTON_TRAVEL = 0.6  # case clearance envelope, not the switch's permitted stroke


@dataclass(frozen=True)
class Display:
    """An I2C OLED module held glass up under the top, on a cable to the
    board's JST-SH socket. Positions are from the module PCB's centre, x along the case,
    y toward the rear."""

    name: str
    pcb: tuple[float, float, float]  # x, y, thickness
    glass: tuple[float, float, float, float, float]  # centre x, y; size x, y; thickness
    active: tuple[float, float, float, float]  # visible pixels: centre x, y; size x, y
    back: float  # tallest part under the PCB, connector included
    holes: tuple[tuple[float, float], ...]


# Adafruit 938, from its board file, fab print and STEP model. The flex edge faces the
# front, so the picture reads from in front of the box.
OLED = Display(
    name="Adafruit 938 1.3in 128x64 OLED",
    pcb=(35.56, 33.02, 1.57),
    glass=(0.0, 0.06, 34.5, 23.0, 1.5),
    active=(0.0, 2.11, 29.42, 14.70),
    back=3.0,  # the JST-SH sockets
    holes=tuple((sx * 15.24, sy * 13.97) for sx in (-1, 1) for sy in (-1, 1)),
)
DISPLAY_OVER = ("J501", "J601")  # centred between ports 2 and 3
WINDOW_MARGIN = 0.5  # window beyond the active area, each side
WINDOW_BEVEL = 1.4  # 45 degree chamfer round the window's outer edge
GLASS_GAP = 0.4  # glass face to the inside of the top; clears the tape over the flex
POST_D = 4.0  # leaves 0.4 mm to the glass beside the holes
DISPLAY_HOLE_D, DISPLAY_PIN_D = 2.5, 2.1
DISPLAY_PIN_OVER = 0.2


COVERS = {"passive": None, "display": OLED}

# Top ventilation.
SLOT_W = 2.0

# Marks
TEXT_DEPTH = 0.6  # three 0.20 mm layers, flush with the top
TEXT_SIZE = 4.0
WORDMARK = "iasis"
WORDMARK_SIZE = 8.0

BASE = (Align.CENTER, Align.CENTER, Align.MIN)  # centred, standing on the given point
Z_UP = (0, 0, 1)  # the outline comes from the board's underside and faces down


@dataclass(frozen=True)
class Opening:
    """A hole in the front or rear wall for the part `ref`, centred on (x, z). y_face is
    the part's face toward that wall, y_wall the wall's outer face."""

    ref: str
    kind: str  # usb-c or light
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

    def profile(self) -> Sketch:
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
    port_wall: float
    z_bottom: float  # tray underside
    corner_r: float  # outer vertical corners: the board's plus clearance and wall
    holes: list[Vector]
    parts_top: float  # tallest placed part
    split: float
    logo: Path | None = None
    _roofs: dict[Display | None, float] = field(default_factory=dict, init=False, repr=False)

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
        return out

    @cached_property
    def display_x(self) -> float:
        return sum(self.board.box(ref).center().X for ref in DISPLAY_OVER) / len(DISPLAY_OVER)

    def top_inside(self, display: Display | None = None) -> float:
        if display in self._roofs:
            return self._roofs[display]
        top = self.parts_top + HEADROOM
        if display:
            # Only components beneath the module determine its required height.
            w, h, thickness = display.pcb
            boxes = [self.board.box(ref, exact=False) for ref in self.board.parts]
            local_top = max(
                [self.seat]
                + [
                    b.max.Z
                    for b in boxes
                    if b.min.X < self.display_x + w / 2 + MIN_GAP
                    and b.max.X > self.display_x - w / 2 - MIN_GAP
                    and b.min.Y < h / 2 + MIN_GAP
                    and b.max.Y > -h / 2 - MIN_GAP
                ]
            )
            top = max(
                top, local_top + MIN_GAP + display.back + thickness + display.glass[4] + GLASS_GAP
            )
            rear = -GLASS_GAP - display.glass[4] - thickness
            frame = DisplayFrame(self.display_x, rear, 0, display.holes)
            # Frame rails and keeper toes hang below the display's rear components.
            for shape in (frame.part, *frame.keepers):
                envelope = shape.bounding_box()
                under = [
                    b.max.Z
                    for b in boxes
                    if b.min.X < envelope.max.X + MIN_GAP
                    and b.max.X > envelope.min.X - MIN_GAP
                    and b.min.Y < envelope.max.Y + MIN_GAP
                    and b.max.Y > envelope.min.Y - MIN_GAP
                ]
                top = max(top, max(under, default=self.seat) + MIN_GAP - envelope.min.Z)
        self._roofs[display] = math.ceil(top * 2) / 2
        return self._roofs[display]


@dataclass
class Cover:
    part: Part
    intrusions: dict[str, Part]  # solids that reach into the cavity, for the fit check
    display: Part | None  # the module's envelope
    markings: Compound
    button: Part
    joint: Joint
    retainer: DisplayFrame | None


def layout(board: Board, logo: Path | None = None) -> Layout:
    required = set(PORTS) | set(LIGHTS) | {BUTTON}
    missing = required - board.parts.keys()
    if missing:
        raise ValueError("board STEP lacks enclosure references: " + ", ".join(sorted(missing)))
    size = board.size
    radii = [e.radius for e in board.outline().edges() if e.geom_type == GeomType.CIRCLE]
    corner = min(radii, default=0.0)
    # Both the nominal board gap and its full sideways movement consume recess.
    # Round down to a printable wall thickness, retaining the seating/error budget.
    wall_limit = min(
        MATED_SHOULDER[ref]
        - SHOULDER_GAP
        + abs(_mouth_y(board, ref))
        - size.Y / 2
        - 2 * CLEARANCE
        - PORT_ERROR
        for ref in PORTS
    )
    port_wall = min(WALL, math.floor((wall_limit + 1e-6) * 10) / 10)
    if port_wall < 0.8:
        raise ValueError(f"USB seating budget leaves only {port_wall:.1f} mm wall")
    return Layout(
        board=board,
        seat=board.body.bounding_box().max.Z,
        # Arc joins add zero-length arcs at the outline's tangent points, which mesh badly.
        cavity=Face(board.outline().outer_wire().offset_2d(CLEARANCE, kind=Kind.INTERSECTION)),
        length=size.X + 2 * (CLEARANCE + WALL),
        width=size.Y + 2 * (CLEARANCE + port_wall),
        port_wall=port_wall,
        z_bottom=-(STANDOFF + FLOOR),
        corner_r=corner + CLEARANCE + min(WALL, port_wall),
        holes=board.holes(HOLE_D),
        parts_top=max(board.box(ref, exact=False).max.Z for ref in board.parts),
        split=board.body.bounding_box().max.Z + 1.6,
        logo=logo,
    )


def _facing(lay: Layout, ref: str) -> tuple[BoundBox, float, float]:
    """A part's exact box, the y of its face toward the nearer long wall, and the y of
    that wall's outer face."""
    bb = lay.board.box(ref)
    if bb.center().Y > 0:
        return bb, bb.max.Y, lay.y_rear
    return bb, bb.min.Y, lay.y_front


def _mouth_y(board: Board, ref: str) -> float:
    bb = board.box(ref)
    return bb.max.Y if bb.center().Y > 0 else bb.min.Y


def _nose(board: Board, ref: str) -> BoundBox:
    bb = board.box(ref)
    y_face = _mouth_y(board, ref)
    inward = -1 if y_face > 0 else 1
    # The nose section excludes shell anchors and solder tails from the opening.
    section = board.parts[ref] & (
        Pos(bb.center().X, y_face + inward * 0.05, bb.center().Z)
        * Box(bb.size.X + 2, 0.10, bb.size.Z + 2)
    )
    return section.bounding_box()


def _port(lay: Layout, ref: str, kind: str, label: str) -> Opening:
    _, y_face, y_wall = _facing(lay, ref)
    nose = _nose(lay.board, ref)
    w, h = nose.size.X + 2 * PORT_FIT, nose.size.Z + 2 * PORT_FIT
    return Opening(
        ref,
        kind,
        label,
        nose.center().X,
        nose.center().Z,
        w,
        h,
        min(h / 2 - 0.01, 1.5),
        y_face,
        y_wall,
    )


def _light(lay: Layout, ref: str) -> Opening:
    bb, y_face, y_wall = _facing(lay, ref)
    w, h = min(WINDOW[0], bb.size.X), min(WINDOW[1], bb.size.Z)
    return Opening(ref, "light", None, bb.center().X, bb.center().Z, w, h, h / 2, y_face, y_wall)


def _outline(lay: Layout) -> Sketch:
    return RectangleRounded(lay.length, lay.width, lay.corner_r)


def tray(lay: Layout) -> Part:
    """Lower tray, printed floor down, with open-top halves of the port openings."""
    z0 = lay.z_bottom
    body = extrude(Pos(0, 0, z0) * _outline(lay), amount=lay.split - z0)
    body = chamfer(body.edges().group_by(Axis.Z)[0], BASE_CHAMFER)
    body -= extrude(Pos(0, 0, z0 + FLOOR) * lay.cavity, amount=lay.split - z0 - FLOOR + 1, dir=Z_UP)
    body += Compound(
        [
            Pos(h.X, h.Y, z0 + FLOOR - 0.1) * Cylinder(STANDOFF_D / 2, STANDOFF + 0.1, align=BASE)
            for h in lay.holes
        ]
    )
    for h in lay.holes:
        body += Pos(h.X, h.Y, -0.1) * Cylinder(
            LOCATOR_D / 2, lay.seat + LOCATOR_OVER - 0.3, align=BASE
        )
        body += Pos(h.X, h.Y, lay.seat + LOCATOR_OVER - 0.4) * Cone(
            LOCATOR_D / 2, LOCATOR_D / 2 - 0.4, 0.4, align=BASE
        )
    body -= Compound([_wall_cut(o) for o in lay.openings])
    joint = Joint(lay.length / 2, lay.split, lay.top_inside())
    body += Compound(joint.tray_parts())
    body -= Compound(joint.tray_cuts())
    body.label = "tray"
    # build123d's boolean annotations lose the result's dimensional subtype.
    return cast(Part, body)


def cover(lay: Layout, name: str, display: Display | None) -> Cover:
    top_in = lay.top_inside(display)
    top_out = top_in + TOP
    z0 = lay.split

    shell = extrude(Pos(0, 0, z0) * _outline(lay),
                    amount=top_out - z0)  # fmt: skip
    shell = chamfer(shell.edges().group_by(Axis.Z)[-1], TOP_CHAMFER)
    shell -= extrude(Pos(0, 0, z0 - 1) * lay.cavity, amount=top_in - z0 + 1, dir=Z_UP)

    # Pillars and display posts project from the roof into the cavity.
    intrusions: dict[str, Part] = {}
    joint = Joint(lay.length / 2, lay.split, top_in)
    shell -= Compound(joint.cover_cuts())
    intrusions |= {f"latch {i + 1}": p for i, p in enumerate(joint.cover_parts())}
    cuts: list[Part] = []
    for i, h in enumerate(lay.holes):
        bottom = lay.seat + BOARD_HOLD_GAP
        pillar = Pos(h.X, h.Y, bottom) * Cylinder(PILLAR_D / 2, top_in - bottom + 0.5, align=BASE)
        pillar -= Pos(h.X, h.Y, bottom - 0.1) * Cylinder(
            (LOCATOR_D + 2 * CLEARANCE) / 2, LOCATOR_OVER + CLEARANCE, align=BASE
        )
        intrusions[f"pillar {i + 1}"] = cast(Part, pillar)
    shell += Compound(list(intrusions.values()))

    cuts += [_wall_cut(o) for o in lay.openings]
    cuts += _top_slots(lay, top_in - 1, TOP + 2, display)
    sw = lay.board.box(BUTTON).center()
    cuts.append(
        Pos(sw.X, sw.Y, top_in - 1) * Cylinder(BUTTON_D / 2 + BUTTON_FIT, TOP + 2, align=BASE)
    )

    display_part = None
    retainer = None
    if display:
        window, posts, display_part = _display(lay, display, top_in, top_out)
        cuts.append(window)
        shell += Compound(posts)
        intrusions |= {f"display post {i + 1}": p for i, p in enumerate(posts)}
        rear = top_in - GLASS_GAP - display.glass[4] - display.pcb[2]
        retainer = DisplayFrame(lay.display_x, rear, top_in, display.holes)
        shell += Compound(retainer.keepers)
        intrusions |= {f"display keeper {i + 1}": p for i, p in enumerate(retainer.keepers)}

    markings = _markings(lay, top_out)
    shell -= Compound(cuts)
    shell -= markings
    shell.label = f"cover {name}"
    return Cover(
        part=cast(Part, shell),
        intrusions=intrusions,
        display=display_part,
        markings=markings,
        button=_button(lay, name, top_in, top_out),
        joint=joint,
        retainer=retainer,
    )


def _display(
    lay: Layout, d: Display, top_in: float, top_out: float
) -> tuple[Part, list[Part], Part]:
    """Window, locating posts and the module envelope, including clear mounting corners."""
    pw, pd, pt = d.pcb
    x = lay.display_x
    y = 0.0  # board centre; clears the front mounting pillars
    ax, ay, aw, ah = d.active
    w, h = aw + 2 * WINDOW_MARGIN, ah + 2 * WINDOW_MARGIN
    window = extrude(Pos(x + ax, y + ay, top_in - 1) * Rectangle(w, h), amount=TOP + 2)
    bevel = Pos(x + ax, y + ay, top_out - WINDOW_BEVEL) * Rectangle(w, h)
    window += extrude(bevel, amount=WINDOW_BEVEL + 1, taper=-45)

    gx, gy, gw, gh, gt = d.glass
    face = top_in - GLASS_GAP - gt  # the module PCB's top face
    posts = []
    for hx, hy in d.holes:
        at = Pos(x + hx, y + hy, face)
        post = at * Cylinder(POST_D / 2, top_in - face + 0.5, align=BASE)
        post += Pos(x + hx, y + hy, face - pt) * Cylinder(DISPLAY_PIN_D / 2, pt + 0.1, align=BASE)
        post += Pos(x + hx, y + hy, face - pt - DISPLAY_PIN_OVER) * Cone(
            DISPLAY_PIN_D / 2 - DISPLAY_PIN_OVER,
            DISPLAY_PIN_D / 2,
            DISPLAY_PIN_OVER,
            align=BASE,
        )
        posts.append(cast(Part, post))

    module = Pos(x, y, face) * Box(pw, pd, pt, align=(Align.CENTER, Align.CENTER, Align.MAX))
    for hx, hy in d.holes:
        module -= Pos(x + hx, y + hy, face - pt - 0.1) * Cylinder(
            DISPLAY_HOLE_D / 2, pt + 0.2, align=BASE
        )
    back = Pos(x, y, face - pt) * Box(pw, pd, d.back, align=(Align.CENTER, Align.CENTER, Align.MAX))
    # The vendor STEP leaves these mounting corners free of rear components and flex.
    for hx, hy in d.holes:
        back -= Pos(x + hx, y + hy, face - pt - d.back - 0.1) * Cylinder(
            3.8, d.back + 0.2, align=BASE
        )
    module += back
    module += Pos(x + gx, y + gy, face) * Box(gw, gh, gt, align=BASE)
    module.label = f"{d.name} envelope"
    return window, posts, cast(Part, module)


def _wall_cut(o: Opening) -> Part:
    start = o.plane(o.y_wall - o.inward)  # 1 mm outside
    cut = extrude(start * o.profile(), amount=-(WALL + 2))
    if o.kind != "usb-c":
        return cast(Part, cut)
    # Round the outer edge on an unsplit slab of wall, then cut what the slab leaves of a
    # block that stops inside it, so the tray and cover halves share one profile.
    w, h = o.width + 2 * PORT_EDGE_R + 2, o.height + 2 * PORT_EDGE_R + 2
    slab = extrude(o.plane(o.y_wall) * Rectangle(w, h), amount=-2 * PORT_EDGE_R) - cut
    lip = [
        e
        for e in slab.edges()
        if abs(e.bounding_box().min.Y - o.y_wall) < 1e-4
        and abs(e.bounding_box().max.Y - o.y_wall) < 1e-4
        and abs(e.center().X - o.x) < o.width / 2 + 1e-3
        and abs(e.center().Z - o.z) < o.height / 2 + 1e-3
    ]
    slab = fillet(lip, PORT_EDGE_R)
    block = extrude(start * Rectangle(w - 0.2, h - 0.2), amount=-(1 + 1.5 * PORT_EDGE_R))
    return cast(Part, cut + (block - slab))


def _artwork(lay: Layout) -> list[Sketch]:
    """Top-face marks for connectors, status, controls and name."""
    host, power, first = (lay.board.box(ref).center() for ref in ("J901", "J1001", "J401"))
    status = lay.board.box("D1170").center()
    sw = lay.board.box(BUTTON).center()
    y = sw.Y - 10.5
    return [
        Pos(host.X, lay.y_rear - 7) * icon("laptop-minimal"),
        Pos(power.X, lay.y_rear - 7) * icon("plug"),
        Pos(first.X, lay.y_front + 7) * Circle(1),
        Pos(status.X, lay.y_front + 7) * icon("activity", size=5.5),
        Pos(0, lay.y_front + 7) * wordmark(WORDMARK, WORDMARK_SIZE, lay.logo),
        Pos(sw.X - 10.3, y) * Circle(0.7),
        Pos(sw.X - 3.8, y) * Text("DFU", font_size=TEXT_SIZE, font_style=FontStyle.BOLD),
        Pos(sw.X + 4.7, y) * SlotOverall(4, 1.4),
        Pos(sw.X + 11.2, y) * icon("rotate-ccw", size=5.5),
    ]


def _markings(lay: Layout, top: float) -> Compound:
    marks = Compound([extrude(Pos(0, 0, top) * face, amount=-TEXT_DEPTH) for face in _artwork(lay)])
    marks.label = "markings"
    return marks


def _button(lay: Layout, name: str, top_in: float, top_out: float) -> Part:
    """Separate cap inserted from inside; the flange retains it and the switch returns it."""
    sb = lay.board.box(BUTTON)
    sw = sb.center()
    flange_top = top_in - BUTTON_ROOF_GAP
    flange_bottom = flange_top - BUTTON_FLANGE_H
    pin_bottom = sb.max.Z
    cap = Pos(sw.X, sw.Y, flange_top) * Cylinder(
        BUTTON_D / 2, top_out + BUTTON_PROUD - flange_top, align=BASE
    )
    cap += Pos(sw.X, sw.Y, flange_bottom) * Cone(
        BUTTON_FLANGE_D / 2, BUTTON_D / 2, BUTTON_FLANGE_H, align=BASE
    )
    cap += Pos(sw.X, sw.Y, pin_bottom) * Cylinder(
        BUTTON_PIN / 2, flange_bottom - pin_bottom + 0.1, align=BASE
    )
    cap.label = f"button {name}"
    return cast(Part, cap)


def _top_slots(lay: Layout, z: float, depth: float, display: Display | None) -> list[Part]:
    """Whole slots on a regular grid; no clipped slots around controls or the display."""
    avoid = [f.bounding_box() for f in _artwork(lay)]
    bb = lay.board.box(BUTTON)
    avoid.append(
        (Pos(bb.center().X, bb.center().Y) * Circle(BUTTON_FLANGE_D / 2 + MIN_GAP)).bounding_box()
    )
    if display:
        avoid.append(
            (
                Pos(lay.display_x, 0) * Rectangle(display.pcb[0] + 8, display.pcb[1] + 8)
            ).bounding_box()
        )
    slots = []
    rows: dict[int, list[tuple[int, Sketch]]] = {}
    # Short 12 mm vents leave cross ribs and bridge cleanly on the print bed.
    x_start, x_end = math.ceil((-lay.length / 2 + 10) / 4), math.floor((lay.length / 2 - 10) / 4)
    y_start, y_end = math.ceil((-lay.width / 2 + 14) / 16), math.floor((lay.width / 2 - 14) / 16)
    for x in range(4 * x_start, 4 * x_end + 1, 4):
        for y in range(16 * y_start, 16 * y_end + 1, 16):
            profile = Pos(x, y, z) * Rot(Z=90) * SlotOverall(12, SLOT_W)
            b = profile.bounding_box()
            if any(
                b.min.X < a.max.X + 2
                and b.max.X > a.min.X - 2
                and b.min.Y < a.max.Y + 2
                and b.max.Y > a.min.Y - 2
                for a in avoid
            ):
                continue
            rows.setdefault(y, []).append((x, profile))
    for row in rows.values():
        groups: list[list[tuple[int, Sketch]]] = []
        for x, profile in row:
            if not groups or x - groups[-1][-1][0] > 4:
                groups.append([])
            groups[-1].append((x, profile))
        for group in groups:
            if len(group) >= 3:
                slots.extend(extrude(profile, amount=depth) for _, profile in group)
    return slots


STEP = Path(__file__).resolve().parents[4] / "build" / "boards" / "iasis" / "board.step"


def assemble(logo: Path | None = None) -> tuple[Layout, Part, dict[str, Cover]]:
    lay = layout(load(STEP), logo)
    covers = {name: cover(lay, name, display) for name, display in COVERS.items()}
    return lay, tray(lay), covers


def main() -> None:
    import argparse

    from .viewer import show

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("cover", nargs="?", choices=COVERS, default="passive")
    parser.add_argument("--logo", type=Path, help="filled SVG placed beside the name")
    parser.add_argument("--cm5", action="store_true", help="show the CM5 build's boards instead")
    args = parser.parse_args()
    if args.cm5:
        from .side import assemble as side_board
        from .viewer import show_cm5

        board = load(STEP)
        show_cm5(board, side_board(board))
        return
    lay, bottom, covers = assemble(args.logo)
    show(lay, bottom, covers, active=args.cover)


if __name__ == "__main__":
    main()
