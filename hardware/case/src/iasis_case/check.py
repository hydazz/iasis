"""Fit checks for the enclosure against the board.

Exact solids check walls, pillars, controls and the display against nearby components.
USB checks distinguish the inserted metal plug from its exterior plastic shoulder and
reserve room for board movement and printing error.
"""

from itertools import combinations

from build123d import (
    BoundBox,
    Box,
    Circle,
    Cylinder,
    Part,
    Pos,
    Rectangle,
    SlotOverall,
    extrude,
)
from build123d.topology.shape_core import Shape

from . import battery, case, retention

USB_C_METAL = (8.28, 2.43)  # USB Type-C R2.5 Fig 3-3, maximum metal plug envelope
USB_C_OVERMOLD = (12.85, 7.0)  # maximum straight cable body, stays outside the wall
MIN_WEB = 1.0  # wall left between neighbouring openings
PIN_TOOL = 1.0  # straightened paper clip through the nRPIBOOT pinhole
MIN_CORNER_FLAT = 1.0  # flat wall between an opening's edge round and a corner radius

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

    r.lines.append("solids")
    feet = case.feet(lay)
    printed = [
        tray,
        *(c.part for c in covers.values()),
        lay.button,
    ]
    for part in printed:
        if part.is_valid and len(part.solids()) == 1 and part.volume > 0:
            bb = part.bounding_box()
            r.ok(f"{part.label}: one valid solid, {part.volume / 1000:.1f} cm3, "
                 f"{bb.size.X:.1f} x {bb.size.Y:.1f} x {bb.size.Z:.1f} mm")  # fmt: skip
        else:
            r.fail(f"{part.label}: valid={part.is_valid}, {len(part.solids())} solids")
        if (overlap := _overlap(part, board.body)) > 1e-3:
            r.fail(f"{part.label} overlaps the board by {overlap:.0f} mm3")

    for foot in feet:
        if _overlap(foot, tray) > 1e-3:
            r.fail("adhesive foot overlaps the tray")
        if any(_overlap(foot, f.driver) > 1e-3 for f in lay.fasteners):
            r.fail("adhesive foot blocks a case screw")

    for f in lay.fasteners:
        inset = f.screw.bounding_box().min.Z - lay.z_bottom
        if inset < 0.19:
            r.fail("case screw head projects beyond its 0.2 mm recess allowance")
        if any(_overlap(Pos(0, 0, -d) * f.screw, tray) > 1e-3 for d in (1, 3, 6, 12)):
            r.fail("case screw cannot withdraw through its counterbore")

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
            case.STANDOFF_D / 2, lay.support_z - floor_top, align=case.BASE
        )
        _clear(r, f"standoff {i + 1}", standoff, parts)
    for i, f in enumerate(lay.fasteners, 1):
        support = tray & (
            Pos(f.x, f.y, floor_top)
            * Cylinder(retention.HEAD_CLEAR / 2 + 1, lay.support_z - floor_top, align=case.BASE)
        )
        _clear(r, f"counterbore support {i}", support, parts)

    movement = (case.HOLE_D - case.LOCATOR_D + case.LOCATOR_CLEARANCE) / 2
    if movement > case.CLEARANCE + 1e-6:
        r.fail(f"board/cover movement {movement:.2f} mm exceeds USB seating allowance")
    else:
        r.ok(f"board/cover movement bounded to {movement:.2f} mm")
    if any(_overlap(Pos(0, 0, z / 2) * board.body, tray) > 1e-3 for z in range(1, 13)):
        r.fail("PCB cannot lower vertically onto the tray pegs")
    else:
        r.ok("PCB lowers vertically onto the tray pegs")
    if lay.battery is not None:
        _battery(r, lay, tray, covers, parts)

    r.lines.append("openings")
    _webs(r, lay)
    _corners(r, lay)
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
        top = lay.top_inside
        roof = c.part & (
            Pos(0, 0, top) * Box(lay.length + 2, lay.width + 2, case.TOP + 1, align=case.BASE)
        )
        _clear(r, "roof and exposed-port edges", roof, parts)
        for ref, (kind, _) in lay.ports.items():
            if kind == "rj45":
                jack = board.parts[ref]
                if any(_overlap(Pos(0, 0, dz) * c.part, jack) > 1e-3 for dz in (0, 1, 3, 6)):
                    r.fail(f"cover cannot lift clear of {ref}")
                else:
                    r.ok(f"{ref} clears the roof notch during cover removal")
        for label, solid in c.intrusions.items():
            _clear(r, label, solid, parts)
        switch = board.parts[case.BUTTON]
        if lay.button.distance_to(switch) > 1e-4 or _overlap(lay.button, switch) > 1e-3:
            r.fail("button does not rest on switch actuator")
        else:
            r.ok("button rests on switch actuator without preload")
        guide = c.intrusions["button guide"]
        guide_box = guide.bounding_box()
        if guide_box.size.Z - 0.5 < 2.5:
            r.fail("button lacks a substantial cylindrical guide below the roof")
        for travel in (0.0, case.BUTTON_TRAVEL):
            pressed = Pos(0, 0, -travel) * lay.button
            if _overlap(pressed, c.part) > 1e-3:
                r.fail(f"button catches cover at {travel:.2f} mm travel")
            _clear(
                r,
                f"button at {travel:.2f} mm travel",
                pressed,
                [p for p in parts if p[0] != case.BUTTON],
            )
            sb = board.box(case.BUTTON)
            allowed_contact = Pos(
                sb.center().X, sb.center().Y, sb.max.Z - travel - 0.01
            ) * Cylinder(case.BUTTON_ACTUATOR_D / 2, travel + 0.02, align=case.BASE)
            if _overlap(pressed, switch - allowed_contact) > 1e-3:
                r.fail(f"button catches the stationary switch body at {travel:.2f} mm travel")
        for dx, dy in ((-0.15, 0), (0.15, 0), (0, -0.15), (0, 0.15)):
            if _overlap(Pos(dx, dy, 0) * lay.button, guide) < 0.01:
                r.fail(f"button guide does not limit lateral motion {dx},{dy} mm")
        if _overlap(Pos(0, 0, 1) * lay.button, c.part) > 1e-3:
            r.ok("button flange retains cap beneath roof")
        else:
            r.fail("button cap can escape through cover")
        if c.display is not None:
            d = case.COVERS[name]
            assert d is not None
            others: Candidates = [(k, v, v.bounding_box()) for k, v in c.intrusions.items()
                      if not k.startswith("display")]  # fmt: skip
            _clear(r, "display envelope", c.display, parts)
            _clear(r, "display envelope to pillars", c.display, others)
            _clear(
                r,
                "display envelope to button",
                c.display,
                [("button", lay.button, lay.button.bounding_box())],
            )
            _display_retention(r, c)
            assert c.display_mount is not None
            for i, pin in enumerate(c.display_mount.pins, 1):
                _clear(r, f"display pin {i}", pin, parts)
        _case_retention(r, lay, c, tray, parts)
        _paths(r, lay, c.part, tray)
        if (overlap := _overlap(c.part, tray)) > 1e-3:
            r.fail(f"{name} overlaps tray by {overlap:.2f} mm3")
        else:
            r.ok(f"{name} meets tray without overlap")
        if (overlap := _overlap(c.part, c.markings)) > 1e-3:
            r.fail(f"{name} body/colour overlap {overlap:.2f} mm3")
        elif len((c.part + c.markings).solids()) != 1:
            r.fail(f"{name} lettering is not attached")
        else:
            r.ok(f"{name} flush colour inlays fill their pockets")
    return r


def _battery(
    r: Report, lay: case.Layout, tray: Part, covers: dict[str, case.Cover], parts: Candidates
) -> None:
    b = lay.battery
    assert b is not None
    r.lines.append("RTC battery")
    for name, shape in (("pack", b.pack), ("leads", b.lead), ("plug", b.plug)):
        if _overlap(shape, tray) > 1e-3:
            r.fail(f"RTC {name} intersects the tray")
        else:
            r.ok(f"RTC {name} fits its tray space")
        _clear(r, f"RTC {name}", shape, [p for p in parts if p[0] != "S:J104"])
        if _overlap(shape, lay.board.body) > 1e-3:
            r.fail(f"RTC {name} intersects a PCB")
        for cover in covers.values():
            if _overlap(shape, cover.part) > 1e-3:
                r.fail(f"RTC {name} intersects {cover.part.label}")
    clearance = lay.board.body.distance_to(b.pack)
    if clearance < case.MIN_GAP:
        r.fail(f"RTC pack only {clearance:.2f} mm below PCB")
    else:
        r.ok(f"RTC pack/adhesive allowance {battery.PACK[2]:.1f} mm, {clearance:.2f} mm below PCB")
    # A real floor slab catches intersections with underside feet as well as the pocket.
    pocket_box = (b.pocket + b.channel).bounding_box()
    web = Pos(pocket_box.center().X, pocket_box.center().Y, b.bottom - 0.999) * Box(
        pocket_box.size.X, pocket_box.size.Y, 0.998, align=case.BASE
    )
    if (web - tray).volume > 1e-3:
        r.fail("RTC recess leaves less than 1 mm of solid floor")
    else:
        r.ok("RTC recess retains 1 mm of solid floor")
    if b.lead_length + 10 > battery.LEAD_MIN:
        r.fail(f"RTC lead route {b.lead_length:.1f} mm leaves less than 10 mm slack")
    else:
        r.ok(
            f"RTC lead route {b.lead_length:.1f} mm leaves {battery.LEAD_MIN - b.lead_length:.1f} mm at 90 mm lead length"
        )
    if any(_overlap(Pos(0, 0, dz) * lay.board.body, b.pack) > 1e-3 for dz in (0, 1, 3, 6)):
        r.fail("boards cannot lift clear of the seated RTC battery")
    else:
        r.ok("boards lift clear of the seated RTC battery")


def _fastener(r: Report, label: str, f: retention.Fastener, receiver: Part, clamped: Part) -> None:
    for name, hardware in (("screw", f.screw), ("nut", f.nut)):
        if _overlap(hardware, receiver) > 1e-3 or _overlap(hardware, clamped) > 1e-3:
            r.fail(f"{label}: {name} overlaps printed material")
    if any(_overlap(f.nut_at(d / 2), receiver) > 1e-3 for d in range(17)):
        r.fail(f"{label}: nut cannot slide into its pocket")
    else:
        r.ok(f"{label}: nut loads through its side opening")
    if _overlap(f.driver, receiver) > 1e-3 or _overlap(f.driver, clamped) > 1e-3:
        r.fail(f"{label}: screwdriver access is blocked")
    # Full-height sections below the nut and above the head carry axial load.
    nut_land = _overlap(Pos(0, 0, -1.2) * f.nut, receiver) / 1.2
    head_land = _overlap(Pos(0, 0, 1.2) * f.screw, clamped) / 1.2
    if min(nut_land, head_land) < 12:
        r.fail(
            f"{label}: insufficient 1.2 mm-deep bearing land ({nut_land:.2f}/{head_land:.2f} mm2)"
        )
    else:
        r.ok(f"{label}: nut/head bearing lands {nut_land:.1f}/{head_land:.1f} mm2 over 1.2 mm")
    if f.tip - (f.nut_z + retention.NUT_H + retention.NUT_PLAY) < 0.5:
        r.fail(f"{label}: screw does not pass fully through the nut at maximum axial play")
    if receiver.bounding_box().max.Z - f.tip < 0.8:
        r.fail(f"{label}: screw tip leaves less than 0.8 mm of roof")


def _case_retention(
    r: Report, lay: case.Layout, c: case.Cover, tray: Part, parts: Candidates
) -> None:
    if len(c.fasteners) != 4 or len({(f.x, f.y) for f in c.fasteners}) != 4:
        r.fail("case needs four distinct screw joints")
    for i, f in enumerate(c.fasteners):
        _fastener(r, f"case screw {i + 1}", f, c.part, tray)
        _clear(r, f"case hardware {i + 1}", f.hardware, parts)
        if _overlap(f.hardware, lay.board.body) > 1e-3:
            r.fail(f"case screw {i + 1} intersects PCB material")
    if _overlap(Pos(0, 0, -0.1) * c.part, tray) < 10:
        r.fail("case lacks a substantial perimeter closing stop")
    else:
        r.ok("case closes against its perimeter seam")
    if any(_overlap(Pos(0, 0, dz / 4) * c.part, tray) > 1e-3 for dz in range(1, 17)):
        r.fail("unscrewed cover cannot lift straight off the tray")
    for h in lay.holes:
        locator = lay.fastener_at(h) is None
        top = lay.locator_face if locator else lay.seat
        # Gauge the annulus that actually bears on the PCB, clear of screw bores and pegs.
        ring = (Circle(3.0) - Circle(2.5)).face()
        gauge = extrude(Pos(h.X, h.Y, lay.support_z) * ring, amount=top - lay.support_z)
        if _overlap(gauge, tray) > 1e-3 or _overlap(gauge, c.part) > 1e-3:
            r.fail(f"printed post intrudes into the PCB space at {h.X:.1f},{h.Y:.1f}")
        if (
            min(_overlap(Pos(0, 0, -0.1) * gauge, tray), _overlap(Pos(0, 0, 0.1) * gauge, c.part))
            < 0.4
        ):
            r.fail(f"PCB mounting hole at {h.X:.1f},{h.Y:.1f} lacks a bearing face")
        if locator:
            tip_space = Pos(h.X, h.Y, top + case.LOCATOR_INSERT) * Cylinder(
                case.LOCATOR_D / 2, case.LOCATOR_END_GAP - 0.01, align=case.BASE
            )
            if _overlap(tip_space, c.part) > 1e-3:
                r.fail(f"locating peg at {h.X:.1f},{h.Y:.1f} lacks clearance beyond its tip")
    for dx, dy in ((-0.12, 0), (0.12, 0), (0, -0.12), (0, 0.12)):
        if _overlap(Pos(dx, dy, 0) * c.part, tray) < 0.01:
            r.fail(f"case halves are not located against movement {dx},{dy} mm")
    if case.LOCATOR_INSERT - case.LOCATOR_TAPER < 1:
        r.fail("case locating pegs have less than 1 mm straight socket engagement")
    r.ok(
        f"PCB capture {case.PCB_CAPTURE:.2f} mm at screws, "
        f"{case.LOCATOR_RELIEF:.2f} mm extra at locating pegs"
    )


def _display_retention(r: Report, c: case.Cover) -> None:
    mount, module = c.display_mount, c.display
    assert mount is not None and module is not None
    if len(mount.posts) != 4 or len(mount.pins) != 4:
        r.fail("display needs four supported pin joints")
    if _overlap(module, c.part) > 1e-3:
        r.fail("OLED overlaps its seats")
    for i, (post, pin) in enumerate(zip(mount.posts, mount.pins, strict=True), 1):
        if _overlap(Pos(0, 0, 0.1) * module, post) < 0.6:
            r.fail(f"display corner {i} lacks a substantial PCB seating face")
        if _overlap(pin, module) > 1e-3 or _overlap(pin, c.part) > 1e-3:
            r.fail(f"display pin {i} intersects the OLED or its socket")
        contact = (Pos(0, 0, -0.1) * module) & pin
        if contact is None or contact.volume < 0.6:
            r.fail(f"display pin {i} does not capture the PCB against falling out")
        elif ((Pos(0, 0, -1.0) * contact) - pin).volume > 1e-3:
            r.fail(f"display pin {i} lacks 1 mm of material behind its bearing face")
        if any(_overlap(Pos(0, 0, -d / 2) * pin, c.part) > 1e-3 for d in range(1, 10)):
            r.fail(f"display pin {i} cannot insert straight into its socket")
    if any(_overlap(Pos(0, 0, -d / 2) * module, c.part) > 1e-3 for d in range(1, 10)):
        r.fail("OLED cannot lift straight off its seats with the pins removed")
    r.ok("OLED has flat seating and pin-head capture; pin grip requires a physical fit test")


def _overlap(a: Shape, b: Shape) -> float:
    intersection = a & b
    return sum(solid.volume for solid in intersection.solids()) if intersection is not None else 0.0


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


def _corners(r: Report, lay: case.Layout) -> None:
    """Openings, edge rounds included, stay on the flat of the wall clear of the corner radii."""
    flat = lay.length / 2 - lay.corner_r
    near = []
    for o in lay.openings:
        edge = abs(o.x) + o.width / 2 + (case.PORT_EDGE_R if o.kind == "usb-c" else 0.0)
        if flat - edge < MIN_CORNER_FLAT:
            near.append(f"{o.ref} {flat - edge:.2f} mm")
    if near:
        r.fail("openings reach the corner radius: " + ", ".join(near))
    else:
        r.ok(f"openings leave {MIN_CORNER_FLAT} mm or more of flat wall before the corners")


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
        elif o.kind == "rj45":  # the whole jack face: plug, latch and lights
            face = Rectangle(o.width - 2 * case.PORT_FIT, o.height - 2 * case.PORT_FIT)
            paths = [extrude(o.plane(o.y_face) * face, amount=10)]
        elif o.kind == "pin":
            paths = [extrude(o.plane(o.y_face) * Circle(PIN_TOOL / 2), amount=10)]
        else:
            profile = SlotOverall(o.width - 0.1, o.height - 0.1)
            paths = [extrude(o.plane(o.y_face) * profile, amount=10)]
        if any(_overlap(path, cover) > 1e-3 or _overlap(path, tray) > 1e-3 for path in paths):
            blocked.append(o.label or o.ref)
    if blocked:
        r.fail("blocked from outside: " + ", ".join(blocked))
    else:
        r.ok("maximum plug metal passes; seated overmolds and LED paths clear")


def main() -> int:
    import argparse

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cm5", action="store_true", help="check the CM5 build's enclosure")
    args = parser.parse_args()
    if args.cm5:
        report = run(*case.assemble_cm5())
    else:
        report = run(*case.assemble())
    print("\n".join(report.lines))
    print(f"{len(report.failures)} failures" if report.failures else "all checks passed")
    return int(bool(report.failures))


if __name__ == "__main__":
    raise SystemExit(main())
