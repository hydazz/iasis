"""Fit checks for the enclosure against the board.

Exact solids check walls, pillars, controls and the display against nearby components.
USB checks distinguish the inserted metal plug from its exterior plastic shoulder and
reserve room for board movement and printing error.
"""

from itertools import combinations

from build123d import BoundBox, Cylinder, Part, Pos, Rectangle, SlotOverall, extrude
from build123d.topology.shape_core import Shape

import case

EXPECTED_HOLES = 4
USB_C_METAL = (8.28, 2.43)  # USB Type-C R2.5 Fig 3-3, maximum metal plug envelope
USB_C_OVERMOLD = (12.85, 7.0)  # maximum straight cable body, stays outside the wall
MIN_WEB = 1.0  # wall left between neighbouring openings

Candidates = list[tuple[str, Shape, BoundBox]]


class Report:
    def __init__(self) -> None:
        self.lines: list[str] = []
        self.failures: list[str] = []

    def ok(self, msg: str) -> None:
        self.lines.append(f"  ok    {msg}")

    def fail(self, msg: str) -> None:
        self.lines.append(f"  FAIL  {msg}")
        self.failures.append(msg)


def run(lay: case.Layout, tray: Part, covers: dict[str, case.Cover]) -> Report:
    r = Report()
    board = lay.board
    parts: Candidates = [(ref, p, board.box(ref, exact=False)) for ref, p in board.parts.items()]

    r.lines.append("board")
    if len(lay.holes) == EXPECTED_HOLES:
        r.ok(f"{len(lay.holes)} M3 holes")
    else:
        r.fail(f"{len(lay.holes)} M3 holes, expected {EXPECTED_HOLES}")

    r.lines.append("solids")
    for part in [tray, *(c.part for c in covers.values()), *(c.button for c in covers.values())]:
        if part.is_valid and len(part.solids()) == 1 and part.volume > 0:
            bb = part.bounding_box()
            r.ok(f"{part.label}: one valid solid, {part.volume / 1000:.1f} cm3, "
                 f"{bb.size.X:.1f} x {bb.size.Y:.1f} x {bb.size.Z:.1f} mm")  # fmt: skip
        else:
            r.fail(f"{part.label}: valid={part.is_valid}, {len(part.solids())} solids")
        if (overlap := (part & board.body).volume) > 1e-3:
            r.fail(f"{part.label} overlaps the board by {overlap:.0f} mm3")

    r.lines.append("walls and floor")
    outline = board.body.bounding_box()
    for name, solid in [("tray", tray), *((n, c.part) for n, c in covers.items())]:
        near_wall = [p for p in parts if _margin_xy(p[2], outline) < case.WALL + case.CLEARANCE]
        _clear(r, f"{name} walls", solid, near_wall, gap=case.PORT_FIT - 0.05)
    low = min(rough.min.Z for _, _, rough in parts)
    floor_top = lay.z_bottom + case.FLOOR
    if low - floor_top >= case.MIN_GAP:
        r.ok(f"lowest part tail {low - floor_top:.2f} mm above the floor")
    else:
        r.fail(f"part tails reach {low - floor_top:.2f} mm from the floor")
    for i, h in enumerate(lay.holes):
        standoff = Pos(h.X, h.Y, floor_top) * Cylinder(
            case.STANDOFF_D / 2, case.STANDOFF, align=case.BASE
        )
        _clear(r, f"standoff {i + 1}", standoff, parts)

    engagement = lay.z_bottom + case.HEAD_DEPTH + case.SCREW_LENGTH - lay.seat
    if 3.0 <= engagement <= case.INSERT_DEPTH - 0.2:
        r.ok(f"M3 x {case.SCREW_LENGTH:g}: {engagement:.2f} mm insert engagement")
    else:
        r.fail(f"M3 screw engagement {engagement:.2f} mm outside insert depth")

    r.lines.append("openings")
    r.ok(
        f"USB walls {lay.port_wall:.2f} mm; axial budget includes "
        f"{case.CLEARANCE:.2f} mm board movement + {case.PORT_ERROR:.2f} mm print/placement error"
    )
    _webs(r, lay)
    for o in lay.openings:
        if o.kind == "usb-c":
            recess = (o.y_face - o.y_wall) * o.inward
            gap = case.MATED_SHOULDER[o.ref] - recess - case.CLEARANCE - case.PORT_ERROR
            description = (
                f"{o.label} recess {recess:.3f} mm, worst-case shoulder gap {gap:.3f} mm, "
                f"opening {o.width:.2f} x {o.height:.2f} mm"
            )
            if gap >= case.SHOULDER_GAP - 1e-6:
                r.ok(description)
            else:
                r.fail(description + f"; needs {case.SHOULDER_GAP:.2f} mm")

    for name, c in covers.items():
        r.lines.append(f"cover {name}")
        top = lay.top_inside(case.COVERS[name])
        r.ok(f"inside height {top - lay.seat:.1f} mm above the board, "
             f"{top - lay.parts_top:.1f} mm over the tallest part")  # fmt: skip
        for label, solid in c.intrusions.items():
            _clear(r, label, solid, parts)
        switch = board.parts[case.BUTTON]
        if c.button.distance_to(switch) > 1e-4 or (c.button & switch).volume > 1e-3:
            r.fail("button stem does not rest on switch actuator")
        else:
            r.ok("button stem rests on switch actuator without preload")
        for travel in (0.0, case.BUTTON_TRAVEL):
            pressed = Pos(0, 0, -travel) * c.button
            if (pressed & c.part).volume > 1e-3:
                r.fail(f"button catches cover at {travel:.2f} mm travel")
            _clear(
                r,
                f"button at {travel:.2f} mm travel",
                pressed,
                [p for p in parts if p[0] != case.BUTTON],
            )
        if (Pos(0, 0, 1) * c.button & c.part).volume > 1e-3:
            r.ok("button flange retains cap beneath roof")
        else:
            r.fail("button cap can escape through cover")
        if c.display is not None:
            d = case.COVERS[name]
            assert d is not None
            pilot_depth = case.GLASS_GAP + d.glass[4] + case.TOP - case.POST_SKIN
            engagement = case.DISPLAY_SCREW_LENGTH - d.pcb[2]
            if 2.0 <= engagement <= pilot_depth - 0.2:
                r.ok(
                    f"M2 x {case.DISPLAY_SCREW_LENGTH:g}: {engagement:.2f} mm post engagement, "
                    f"{pilot_depth - engagement:.2f} mm before blind pilot end"
                )
            else:
                r.fail(f"display screw enters {engagement:.2f} mm into {pilot_depth:.2f} mm pilot")
            others: Candidates = [(k, v, v.bounding_box()) for k, v in c.intrusions.items()
                      if not k.startswith("display")]  # fmt: skip
            _clear(r, "display envelope", c.display, parts)
            _clear(r, "display envelope to pillars", c.display, others)
            _clear(
                r,
                "display envelope to button",
                c.display,
                [("button", c.button, c.button.bounding_box())],
            )
        _paths(r, lay, c.part, tray)
        if (overlap := (c.part & tray).volume) > 1e-3:
            r.fail(f"{name} overlaps tray by {overlap:.2f} mm3")
        else:
            r.ok(f"{name} meets tray without overlap")
        if (overlap := (c.part & c.markings).volume) > 1e-3:
            r.fail(f"{name} body/colour overlap {overlap:.2f} mm3")
        elif len((c.part + c.markings).solids()) != 1:
            r.fail(f"{name} lettering is not attached")
        else:
            r.ok(f"{name} flush colour inlays fill their pockets")
        vents = case._top_slots(lay, top - 1, case.TOP + 2, case.COVERS[name])
        if vents:
            area = sum(v.volume / (case.TOP + 2) for v in vents)
            r.ok(f"{name} {len(vents)} top vents, {area:.0f} mm2 open area")
        else:
            r.fail(f"{name} has no top exhaust")
    return r


def _margin_xy(b: BoundBox, outline: BoundBox) -> float:
    return min(b.min.X - outline.min.X, outline.max.X - b.max.X,
               b.min.Y - outline.min.Y, outline.max.Y - b.max.Y)  # fmt: skip


def _near(a: BoundBox, b: BoundBox, gap: float) -> bool:
    return not (a.max.X + gap < b.min.X or b.max.X + gap < a.min.X
                or a.max.Y + gap < b.min.Y or b.max.Y + gap < a.min.Y
                or a.max.Z + gap < b.min.Z or b.max.Z + gap < a.min.Z)  # fmt: skip


def _clear(r: Report, label: str, solid: Shape, candidates: Candidates, gap: float = case.MIN_GAP):
    sb = solid.bounding_box()
    hits = []
    for name, other, box in candidates:
        if _near(sb, box, gap) and (d := solid.distance_to(other)) < gap - 1e-6:
            hits.append(f"{name} {d:.2f} mm")
    if hits:
        r.fail(f"{label}: " + ", ".join(hits))
    else:
        r.ok(f"{label} clear")


def _webs(r: Report, lay: case.Layout) -> None:
    """Openings side by side in a wall leave at least MIN_WEB between them."""
    thin = []
    for a, b in combinations(sorted(lay.openings, key=lambda o: o.x), 2):
        overlap = (
            a.z - a.height / 2 <= b.z + b.height / 2 and b.z - b.height / 2 <= a.z + a.height / 2
        )
        web = (b.x - b.width / 2) - (a.x + a.width / 2)
        if a.front == b.front and overlap and web < MIN_WEB:
            thin.append(f"{a.ref}/{b.ref} {web:.2f} mm")
    if thin:
        r.fail("thin wall between openings: " + ", ".join(thin))
    else:
        r.ok(f"openings leave {MIN_WEB} mm or more of wall between them")


def _paths(r: Report, lay: case.Layout, cover: Part, tray: Part) -> None:
    """Sweep metal through each opening and the seated overmold outside the case."""
    blocked = []
    for o in lay.openings:
        if o.kind == "usb-c":
            metal = extrude(o.plane(o.y_face) * Rectangle(*USB_C_METAL), amount=20)
            gap = case.MATED_SHOULDER[o.ref] - case.CLEARANCE - case.PORT_ERROR
            shoulder = o.y_face - o.inward * gap
            plastic = extrude(o.plane(shoulder) * Rectangle(*USB_C_OVERMOLD), amount=20)
            paths = [metal, plastic]
        else:
            profile = SlotOverall(o.width - 0.1, o.height - 0.1)
            paths = [extrude(o.plane(o.y_face) * profile, amount=10)]
        if any((path & cover).volume > 1e-3 or (path & tray).volume > 1e-3 for path in paths):
            blocked.append(o.label or o.ref)
    if blocked:
        r.fail("blocked from outside: " + ", ".join(blocked))
    else:
        r.ok("maximum plug metal passes; seated overmolds and LED paths clear")
