"""Fit checks for the enclosure against the board.

Exact solids check walls, pillars, controls and the display against nearby components.
USB checks distinguish the inserted metal plug from its exterior plastic shoulder and
reserve room for board movement and printing error.
"""

from itertools import combinations
from math import degrees

from build123d import BoundBox, Box, Cylinder, Part, Pos, Rectangle, Rot, SlotOverall, extrude
from build123d.topology.shape_core import Shape

from . import case, retention

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
        r.ok(f"{len(lay.holes)} mounting holes")
    else:
        r.fail(f"{len(lay.holes)} mounting holes, expected {EXPECTED_HOLES}")

    r.lines.append("solids")
    printed = [tray, *(c.part for c in covers.values()), *(c.button for c in covers.values())]
    printed += [c.retainer.part for c in covers.values() if c.retainer is not None]
    for part in printed:
        if part.is_valid and len(part.solids()) == 1 and part.volume > 0:
            bb = part.bounding_box()
            r.ok(f"{part.label}: one valid solid, {part.volume / 1000:.1f} cm3, "
                 f"{bb.size.X:.1f} x {bb.size.Y:.1f} x {bb.size.Z:.1f} mm")  # fmt: skip
        else:
            r.fail(f"{part.label}: valid={part.is_valid}, {len(part.solids())} solids")
        if (overlap := _overlap(part, board.body)) > 1e-3:
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

    r.ok(f"board locators: {(case.HOLE_D - case.LOCATOR_D) / 2:.2f} mm radial clearance")
    movement = (case.HOLE_D - case.LOCATOR_D) / 2 + retention.FIT
    if movement > case.CLEARANCE + 1e-6:
        r.fail(f"board/cover movement {movement:.2f} mm exceeds USB seating allowance")
    else:
        r.ok(f"board/cover movement bounded to {movement:.2f} mm")
    for i, feature in enumerate(next(iter(covers.values())).joint.tray_parts()):
        _clear(r, f"tray catch/guide {i + 1}", feature, parts)
    if any(_overlap(Pos(0, 0, z / 2) * board.body, tray) > 1e-3 for z in range(1, 13)):
        r.fail("PCB cannot lower vertically onto the tray pegs")
    else:
        r.ok("PCB lowers vertically onto the tray pegs")

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
        if c.button.distance_to(switch) > 1e-4 or _overlap(c.button, switch) > 1e-3:
            r.fail("button stem does not rest on switch actuator")
        else:
            r.ok("button stem rests on switch actuator without preload")
        for travel in (0.0, case.BUTTON_TRAVEL):
            pressed = Pos(0, 0, -travel) * c.button
            if _overlap(pressed, c.part) > 1e-3:
                r.fail(f"button catches cover at {travel:.2f} mm travel")
            _clear(
                r,
                f"button at {travel:.2f} mm travel",
                pressed,
                [p for p in parts if p[0] != case.BUTTON],
            )
        if _overlap(Pos(0, 0, 1) * c.button, c.part) > 1e-3:
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
                [("button", c.button, c.button.bounding_box())],
            )
            _display_retention(r, c, parts)
        _case_retention(r, c, tray, parts)
        _paths(r, lay, c.part, tray)
        _side_link(r, lay, c.part, tray)
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
        vents = case._top_slots(lay, top - 1, case.TOP + 2, case.COVERS[name])
        if vents:
            area = sum(v.volume / (case.TOP + 2) for v in vents)
            r.ok(f"{name} {len(vents)} top vents, {area:.0f} mm2 open area")
        else:
            r.fail(f"{name} has no top exhaust")
    return r


def _case_retention(r: Report, c: case.Cover, tray: Part, parts: Candidates) -> None:
    joint = c.joint
    hook = joint.hook
    root_cut = Pos(joint.inner + retention.BEAM_THICKNESS / 2, joint.y, joint.root - 0.02) * Box(
        1.6, 8.8, 0.02, align=case.BASE
    )
    # Only the roof root should connect the spring to the fixed shell.
    for cut, pad in zip(retention.paired(root_cut), retention.paired(joint.pad)):
        detached = (c.part - cut).solids()
        if not any(
            solid.bounding_box().max.Z <= joint.root and _overlap(solid, pad) > 1e-3
            for solid in detached
        ):
            r.fail("latch is joined to the fixed shell below its root")
    if _overlap(Pos(0, 0, retention.LATCH_PLAY + 0.1) * hook, tray) < 1e-3:
        r.fail("cover hook does not retain the cover")
    else:
        r.ok(f"cover retained with {retention.LATCH_PLAY:.2f} mm axial play")
    released = Pos(-retention.RELEASE, 0, 0) * hook
    for side in retention.paired(released):
        _clear(r, "pressed latch", side, parts)
        if any(_overlap(Pos(0, 0, dz / 4) * side, tray) > 1e-3 for dz in range(25)):
            r.fail("pressed latch cannot lift clear of tray")
            break
    else:
        r.ok(f"latches release at {retention.RELEASE:.2f} mm inward hook travel")
    arm = joint.root - joint.pad_z
    tip = joint.root - joint.shoulder
    pad_travel = retention.RELEASE * 2 * arm / (3 * tip - arm)
    if pad_travel >= retention.STOP:
        r.fail("latch stop prevents release")
    pivot = Pos(joint.inner + retention.BEAM_THICKNESS / 2, joint.y, joint.pad_z)
    # Approximate pad rotation from the cantilever slope.
    fixed = c.part
    for side in retention.paired(hook):
        fixed -= side
    for fraction in (0.25, 0.5, 0.75, 1.0):
        travel = retention.STOP * fraction
        angle = degrees(3 * travel / (2 * arm))
        moved_pad = Pos(-travel, 0, 0) * pivot * Rot(Y=angle) * pivot.inverse() * joint.pad
        for side in retention.paired(moved_pad):
            _clear(r, f"latch pad at {travel:.2f} mm", side, parts)
            if _overlap(side, tray) > 1e-3 or _overlap(side, fixed) > 1e-3:
                r.fail("pressed latch pad catches shell")
    strain = 1.5 * retention.BEAM_THICKNESS * retention.STOP / arm**2
    r.ok(f"beam strain estimate at stop {strain:.2%}; requires physical validation")
    r.ok(
        f"PCB axial clearance {case.BOARD_HOLD_GAP:.2f}–"
        f"{case.BOARD_HOLD_GAP + retention.LATCH_PLAY:.2f} mm"
    )


def _display_retention(r: Report, c: case.Cover, parts: Candidates) -> None:
    mount, module = c.retainer, c.display
    assert mount is not None and module is not None
    frame = mount.part
    _clear(r, "display retainer", frame, parts)
    if _overlap(frame, c.part) > 1e-3 or _overlap(frame, module) > 1e-3:
        r.fail("display retainer overlaps cover or module")
    else:
        r.ok("display retainer fits cover and module")
    if _overlap(Pos(0, 0, -0.1) * frame, c.part) < 1e-3:
        r.fail("display retainer can drop out")
    else:
        r.ok("three keepers retain the display frame")
    if _overlap(Pos(0, mount.release, -0.1) * mount.tip, c.part) > 1e-3:
        r.fail("display finger pad does not release")
    else:
        r.ok(f"display finger pad releases at {mount.release:.2f} mm")
    # The front pad is held in while the frame swings out of the rear keepers.
    rigid = frame - mount.tip
    blocked = []
    pivot = Pos(mount.x, 18.4, mount.base)
    poses = [pivot * Rot(X=a) * pivot.inverse() for a in (0, 1, 2, 3, 4, 5, 6, 8)]
    poses += [Pos(0, -y / 4, 0) * poses[-1] for y in range(1, 5)]
    poses += [Pos(0, 0, -z) * poses[-1] for z in (1, 2, 4, 8)]
    for i, pose in enumerate(poses):
        moved = pose * rigid
        if _overlap(moved, c.part) > 1e-3 or _overlap(moved, module) > 1e-3:
            blocked.append(i)
    if blocked:
        r.fail(f"display frame removal blocked at steps {blocked}")
    else:
        r.ok("display frame tilts, slides forward and lifts out of its keepers")


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
        if any(_overlap(path, cover) > 1e-3 or _overlap(path, tray) > 1e-3 for path in paths):
            blocked.append(o.label or o.ref)
    if blocked:
        r.fail("blocked from outside: " + ", ".join(blocked))
    else:
        r.ok("maximum plug metal passes; seated overmolds and LED paths clear")


def _side_link(r: Report, lay: case.Layout, cover: Part, tray: Part) -> None:
    if case.SIDE_LINK not in lay.board.parts:
        return
    b = lay.board.box(case.SIDE_LINK)
    path = Pos(b.min.X - 5, b.center().Y, b.center().Z) * Box(10, b.size.Y, b.size.Z)
    opened = Pos(b.center().X, b.center().Y, b.min.Z) * Box(
        b.size.X, b.size.Y, 3.52, align=case.BASE
    )
    if any(_overlap(shape, body) > 1e-3 for shape in (path, opened) for body in (cover, tray)):
        r.fail("side-link flex passage or open ZIF envelope blocked")
    else:
        r.ok(
            f"side-link {b.size.Y + 2 * case.PORT_FIT:.2f} x "
            f"{b.size.Z + 2 * case.PORT_FIT:.2f} mm passage; 3.52 mm open ZIF envelope clear"
        )


def main() -> int:
    import argparse
    from pathlib import Path

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--logo", type=Path, help="filled SVG placed beside the name")
    args = parser.parse_args()
    report = run(*case.assemble(args.logo))
    print("\n".join(report.lines))
    print(f"{len(report.failures)} failures" if report.failures else "all checks passed")
    return int(bool(report.failures))


if __name__ == "__main__":
    raise SystemExit(main())
