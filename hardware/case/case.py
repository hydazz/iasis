"""Iasis enclosure: a plate that carries the board and a cover shell that drops over it.

Four M3 screws come up through the plate and the board's M3 holes into heat-set
inserts in the cover's pillars, clamping the board between the plate's standoffs and
the pillars. The cover carries every opening, so the seam runs round the underside,
with seven USB-C openings. A passive cover and an optional OLED cover share the same
plate.

Geometry is in board coordinates (board.py). The board STEP supplies the outline, the
holes, every connector, light and button; the numbers here are fit, form and the parts
the STEP does not contain.
"""

import math
from dataclasses import dataclass
from functools import cached_property

from build123d import (
    Align,
    Axis,
    BoundBox,
    Box,
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

# USB-C openings pass the plug overmold through the wall to the receptacle face.
OVERMOLD = (12.35, 6.5, 1.5)  # width, height, corner radius
OVERMOLD_FIT = 0.25
PORTS = {  # reference: (kind, label)
    "J401": ("usb-c", "1"),
    "J501": ("usb-c", "2"),
    "J601": ("usb-c", "3"),
    "J701": ("usb-c", "4"),
    "J801": ("usb-c", "5"),
    "J901": ("usb-c", "HOST"),
    "J1001": ("usb-c", "POWER"),
}

# Side-emitting LEDs at the board edge shine through windows no larger than their
# emitting face. The confirm button is pressed through a tongue in the top.
LIGHTS = ("D1170", "D1171", "D1172", "D1173", "D1174", "D1175")  # host, targets
WINDOW = (3.0, 1.6)
RECOVERY = {"SW1101": "BOOTSEL", "SW1102": "RESET"}
RECOVERY_HOLE_D = 6.0
BUTTON = "SW1171"
BUTTON_TONGUE = (16.0, 9.0)  # length (hinge to free end), width
BUTTON_SLOT, BUTTON_SKIN, BUTTON_GAP, BUTTON_PIN = 0.8, 1.4, 0.4, 4.0


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
WINDOW_MARGIN = 0.5  # window beyond the active area, each side
WINDOW_BEVEL = 1.4  # 45 degree chamfer round the window's outer edge
GLASS_GAP = 0.4  # glass face to the inside of the top; clears the tape over the flex
POST_D = 4.0  # leaves 0.4 mm to the glass beside the holes
POST_SKIN = 0.8  # top left above the pilot holes


@dataclass(frozen=True)
class Variant:
    display: Display | None = None


COVERS = {"passive": Variant(), "display": Variant(display=OLED)}

# Passive ventilation in the top and low in the end and rear walls.
SLOT_W, SLOT_PITCH = 2.0, 4.0
WALL_SLOT_W, WALL_SLOT_PITCH, WALL_SLOT_H = 2.0, 5.0, 10.0
WALL_SLOT_CLEAR = 3.0  # rear slots to any other opening or label

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

    def profile(self) -> Face:
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
        return out

    def top_inside(self, display: Display | None = None) -> float:
        top = max(self.parts_top + HEADROOM, self.seat + INSERT_DEPTH + 1.0)
        if display:
            # Reserve the module's full underside envelope above the populated board.
            top += GLASS_GAP + display.glass[4] + display.pcb[2] + display.back
        return math.ceil(top * 2) / 2


@dataclass
class Cover:
    part: Part
    intrusions: dict[str, Part]  # solids that reach into the cavity, for the fit check
    display: Part | None  # the module's envelope


def layout(board: Board) -> Layout:
    required = set(PORTS) | set(LIGHTS) | set(RECOVERY) | {BUTTON}
    missing = required - board.parts.keys()
    if missing:
        raise ValueError("board STEP lacks enclosure references: " + ", ".join(sorted(missing)))
    placed = board.placed()
    size = board.size
    radii = [e.radius for e in board.outline().edges() if e.geom_type == GeomType.CIRCLE]
    corner = min(radii, default=0.0)
    return Layout(
        board=board,
        seat=board.body.bounding_box().max.Z,
        cavity=Face(board.outline().outer_wire().offset_2d(CLEARANCE)),
        length=size.X + 2 * (CLEARANCE + WALL),
        width=size.Y + 2 * (CLEARANCE + WALL),
        z_bottom=-(STANDOFF + PLATE),
        corner_r=corner + CLEARANCE + WALL,
        holes=board.holes(HOLE_D),
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
    w, h, r = OVERMOLD
    f = OVERMOLD_FIT
    z = (lay.seat + bb.max.Z) / 2
    return Opening(ref, kind, label, bb.center().X, z, w + 2 * f, h + 2 * f, r + f,
                   y_face, y_wall)  # fmt: skip


def _light(lay: Layout, ref: str) -> Opening:
    bb, y_face, y_wall = _facing(lay, ref)
    w, h = min(WINDOW[0], bb.size.X), min(WINDOW[1], bb.size.Z)
    return Opening(ref, "light", None, bb.center().X, bb.center().Z, w, h, h / 2, y_face, y_wall)


def plate(lay: Layout) -> Part:
    z0 = lay.z_bottom
    outline = Face(lay.cavity.outer_wire().offset_2d(-PLATE_FIT))
    body = extrude(Pos(0, 0, z0) * outline, amount=PLATE, dir=Z_UP)
    body += Compound([Pos(h.X, h.Y, z0 + 1) * Cylinder(STANDOFF_D / 2, STANDOFF + PLATE - 1,
                                                       align=BASE) for h in lay.holes])  # fmt: skip
    cuts = []
    for h in lay.holes:
        cuts.append(Pos(h.X, h.Y, z0) * Cylinder(SCREW_CLEAR / 2, -z0, align=BASE))
        cuts.append(Pos(h.X, h.Y, z0) * Cylinder(HEAD_D / 2, HEAD_DEPTH, align=BASE))
    for sx in (-1, 1):
        for sy in (-1, 1):
            at = Pos(sx * (lay.length / 2 - 18), sy * (lay.width / 2 - 14), z0)
            cuts.append(at * Cylinder(FOOT_D / 2, FOOT_POCKET, align=BASE))
    body -= Compound(cuts)
    body.label = "plate"
    return body


def cover(lay: Layout, name: str, variant: Variant) -> Cover:
    top_in = lay.top_inside(variant.display)
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
    cuts += _wall_slots(lay, [c.bounding_box() for c in wall], top_in)
    if not variant.display:
        cuts += _top_slots(lay, top_in - 1, TOP + 2)
    cuts += _wordmark(lay, top_out)
    for ref, label in RECOVERY.items():
        sw = lay.board.box(ref)
        cuts.append(
            Pos(sw.center().X, sw.center().Y, top_in - 1)
            * Cylinder(RECOVERY_HOLE_D / 2, TOP + 2, align=BASE)
        )
        text = Text(label, font_size=2.5)
        cuts.append(
            extrude(
                Pos(sw.center().X, sw.center().Y + (-5.5 if sw.center().Y > 0 else 5.5), top_out)
                * text,
                amount=-TEXT_DEPTH,
            )
        )
    tongue, pin = _button(lay, top_in, top_out)
    cuts += tongue

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
    return Cover(part=shell, intrusions=intrusions, display=display_part)


def _display(
    lay: Layout, d: Display, top_in: float, top_out: float
) -> tuple[Part, list[Part], list[Part], Part]:
    """The window with its outer bevel, the posts the module hangs from with their pilot
    holes, and the module's envelope."""
    pw, pd, pt = d.pcb
    x = sum(lay.board.box(ref).center().X for ref in DISPLAY_OVER) / len(DISPLAY_OVER)
    y = 0.0  # board centre; clears the front mounting pillars
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
    return extrude(start * profile, amount=-(WALL + 2))


def _labels(lay: Layout) -> list[Part]:
    """Port names centred between the base chamfer and openings above their span."""
    cuts = []
    for o in lay.openings:
        if not o.label:
            continue
        text = Text(o.label, font_size=3.5, font_style=FontStyle.BOLD)
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
    """A top tongue hinged toward the case centre, with a pin over the switch."""
    sb = lay.board.box(BUTTON)
    sw = sb.center()
    length, width = BUTTON_TONGUE
    s = BUTTON_SLOT
    at = Pos(sw.X, sw.Y, top_in - 1) * Rot(Z=180 if sw.X < 0 else 0)
    free_x = 3.0
    hinge_x = free_x - length
    u = Pos((hinge_x + free_x + s) / 2, 0) * Rectangle(length + s, width + 2 * s)
    u -= Pos((hinge_x - 1 + free_x) / 2, 0) * Rectangle(length + 1, width)
    skin = (
        at
        * Pos((hinge_x - 1 + free_x) / 2, 0)
        * Box(length + 1, width, TOP - BUTTON_SKIN + 1, align=BASE)
    )
    pin_bottom = sb.max.Z + BUTTON_GAP
    pin = Pos(sw.X, sw.Y, pin_bottom) * Cylinder(
        BUTTON_PIN / 2, top_out - BUTTON_SKIN - pin_bottom + 0.5, align=BASE
    )
    return [extrude(at * u, amount=TOP + 2), skin], pin


def _top_slots(lay: Layout, z: float, depth: float) -> list[Part]:
    """Rear top vents, with solid material around the controls and their labels."""
    margin = lay.corner_r + WALL_SLOT_CLEAR
    field = (-lay.length / 2 + margin, 4.0, lay.length / 2 - margin, lay.y_rear - margin)
    slots = _slots(field, z, depth)
    for ref in (*RECOVERY, BUTTON):
        bb = lay.board.box(ref)
        w, d = (36.0, 14.0) if ref == BUTTON else (24.0, 18.0)
        slots -= Pos(
            bb.center().X,
            bb.center().Y + (0 if ref == BUTTON else (-3 if bb.center().Y > 0 else 3)),
            z,
        ) * Box(w, d, depth, align=BASE)
    return [slots]


def _slots(field, z: float, depth: float) -> Part:
    x0, y0, x1, y1 = field
    if x1 - x0 < SLOT_W or y1 - y0 <= SLOT_W:
        return Compound([])
    n = int((x1 - x0 + SLOT_PITCH - SLOT_W) // SLOT_PITCH)
    span = (n - 1) * SLOT_PITCH
    xc, yc = (x0 + x1) / 2, (y0 + y1) / 2
    slot = Rot(Z=90) * SlotOverall(y1 - y0, SLOT_W)
    faces = [Pos(xc - span / 2 + i * SLOT_PITCH, yc, z) * slot for i in range(n)]
    return Compound([extrude(f, amount=depth) for f in faces])


def _wall_slots(lay: Layout, avoid: list[BoundBox], top_in: float) -> list[Part]:
    """Vertical intake slots low in the end walls, and along the rear wall wherever it
    is clear of openings and labels."""
    height = min(WALL_SLOT_H, top_in - lay.seat - 3.0)
    if height <= WALL_SLOT_W:
        return []
    z = lay.seat + 1.5 + height / 2
    slot = Rot(Z=90) * SlotOverall(height, WALL_SLOT_W)
    cuts = []
    for sx in (-1, 1):
        plane = Plane.YZ.offset(sx * lay.length / 2)
        cuts += [extrude(plane * Pos(i * WALL_SLOT_PITCH, z) * slot, amount=WALL + 2, both=True)
                 for i in range(-int((lay.width / 2 - lay.corner_r - WALL_SLOT_CLEAR) // WALL_SLOT_PITCH),
                                int((lay.width / 2 - lay.corner_r - WALL_SLOT_CLEAR) // WALL_SLOT_PITCH) + 1)]  # fmt: skip
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
