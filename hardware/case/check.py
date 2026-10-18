"""Fit and validity checks for the enclosure against the board.

The cover's inside is empty except for what reaches into it (pillars, the button pin, a
display module and its posts), so those are measured exactly against nearby
parts; the walls, the top and the plate are tested against part bounding boxes. Each
opening is tested by pushing out from the part's face whatever has to pass through it.
"""

from dataclasses import dataclass, field
from itertools import combinations

from build123d import (
    BoundBox,
    Cylinder,
    Part,
    Pos,
    RectangleRounded,
    Shape,
    SlotOverall,
    extrude,
)

import case

EXPECTED_HOLES = 4
USB_C_PLUG = (12.35, 6.5, 1.5, 20.0)  # Type-C overmold maximum, corner radius, length checked
MIN_WEB = 1.0  # wall left between neighbouring openings


@dataclass
class Report:
    lines: list[str] = field(default_factory=list)
    failures: list[str] = field(default_factory=list)

    def ok(self, msg: str) -> None:
        self.lines.append(f"  ok    {msg}")

    def fail(self, msg: str) -> None:
        self.lines.append(f"  FAIL  {msg}")
        self.failures.append(msg)

    def note(self, msg: str) -> None:
        self.lines.append(f"  note  {msg}")

    def section(self, title: str) -> None:
        self.lines.append(title)


Candidates = list[tuple[str, Shape, BoundBox]]


def run(lay: case.Layout, plate: Part, covers: dict[str, case.Cover]) -> Report:
    r = Report()
    board = lay.board
    placed = board.placed()
    parts: Candidates = [(ref, p, board.rough(ref)) for ref, p in placed.items()]

    r.section("board")
    if board.envelopes:
        r.note("courtyard/maximum-height clearance envelopes: " + ", ".join(board.envelopes))
    if board.missing:
        r.fail("unmodelled component bodies: " + ", ".join(board.missing))
    else:
        r.ok("every component body has a STEP model or clearance envelope")
    unplaced = sorted(set(board.parts) - set(placed))
    if unplaced:
        shown = " ".join(unplaced[:10]) + (" ..." if len(unplaced) > 10 else "")
        r.note(f"{len(unplaced)} parts lie outside the outline and are ignored: {shown}")
    if len(lay.holes) == EXPECTED_HOLES:
        r.ok(f"{len(lay.holes)} M3 holes")
    else:
        r.fail(f"{len(lay.holes)} M3 holes, expected {EXPECTED_HOLES}")

    r.section("solids")
    for part in [plate, *(c.part for c in covers.values())]:
        if part.is_valid and len(part.solids()) == 1 and part.volume > 0:
            bb = part.bounding_box()
            r.ok(f"{part.label}: one valid solid, {part.volume / 1000:.1f} cm3, "
                 f"{bb.size.X:.1f} x {bb.size.Y:.1f} x {bb.size.Z:.1f} mm")  # fmt: skip
        else:
            r.fail(f"{part.label}: valid={part.is_valid}, {len(part.solids())} solids")

    for part in [plate, *(c.part for c in covers.values())]:
        overlap = (part & board.body).volume
        if overlap > 1e-3:
            r.fail(f"{part.label} overlaps the board by {overlap:.0f} mm3")

    r.section("walls and plate")
    outline = board.body.bounding_box()
    room = case.CLEARANCE - case.MIN_GAP
    margins = {}
    for ref, _, rough in parts:
        m = _margin_xy(rough, outline)
        margins[ref] = m if m >= -room else _margin_xy(board.box(ref), outline)
    out = sorted(ref for ref, m in margins.items() if m < -room)
    if out:
        r.fail("parts reach the cover walls: " + ", ".join(f"{x} {margins[x]:+.2f}" for x in out))
    else:
        r.ok(f"parts stay {case.CLEARANCE + min(margins.values()):.2f} mm or more from the walls")
    low = min(rough.min.Z for _, _, rough in parts)
    plate_top = lay.z_bottom + case.PLATE
    if low - plate_top >= case.MIN_GAP:
        r.ok(f"lowest part tail {low - plate_top:.2f} mm above the plate")
    else:
        r.fail(f"part tails reach {low - plate_top:.2f} mm from the plate")
    for i, h in enumerate(lay.holes):
        standoff = Pos(h.X, h.Y, plate_top) * Cylinder(case.STANDOFF_D / 2, case.STANDOFF,
                                                         align=case.BASE)  # fmt: skip
        _clear(r, f"standoff {i + 1}", standoff, parts)
    r.section("openings")
    _webs(r, lay)

    for name, c in covers.items():
        r.section(f"cover {name}")
        top = lay.top_inside(case.COVERS[name].display)
        r.ok(f"inside height {top - lay.seat:.1f} mm above the board, "
             f"{top - lay.parts_top:.1f} mm over the "
             "tallest part")  # fmt: skip
        for label, solid in c.intrusions.items():
            _clear(r, label, solid, parts)
        if c.display is not None:
            others = [(k, v, v.bounding_box()) for k, v in c.intrusions.items()
                      if not k.startswith("display")]  # fmt: skip
            _clear(r, "display envelope", c.display, parts)
            _clear(r, "display envelope to pillars and pin", c.display, others)
        _paths(r, lay, c.part, plate)
        for ref in case.RECOVERY:
            sw = board.box(ref)
            path = Pos(sw.center().X, sw.center().Y, sw.max.Z) * Cylinder(
                case.RECOVERY_HOLE_D / 2 - 0.1, top + case.TOP - sw.max.Z + 1, align=case.BASE
            )
            _clear(
                r, f"{ref} top access", path, [("cover", c.part, c.part.bounding_box())], gap=0.09
            )
            _clear(r, f"{ref} access to parts", path, [p for p in parts if p[0] != ref])
            if c.display is not None:
                _clear(
                    r,
                    f"{ref} access to display",
                    path,
                    [("display", c.display, c.display.bounding_box())],
                )
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
        if _near(sb, box, gap):
            d = solid.distance_to(other)
            if d < gap - 1e-6:
                hits.append(f"{name} {d:.2f} mm")
    if hits:
        r.fail(f"{label}: " + ", ".join(hits))
    else:
        r.ok(f"{label} clear")


def _webs(r: Report, lay: case.Layout) -> None:
    """Openings side by side in a wall leave at least MIN_WEB between them."""

    def span(o: case.Opening) -> tuple[float, float]:
        bottom = o.z - o.height / 2
        return bottom, o.z + o.height / 2

    thin = []
    for a, b in combinations(sorted(lay.openings, key=lambda o: o.x), 2):
        (a0, a1), (b0, b1) = span(a), span(b)
        if a.front != b.front or a1 < b0 or b1 < a0:
            continue
        web = (b.x - b.width / 2) - (a.x + a.width / 2)
        if web < MIN_WEB:
            thin.append(f"{a.ref}/{b.ref} {web:.2f} mm")
    if thin:
        r.fail("thin wall between openings: " + ", ".join(thin))
    else:
        r.ok(f"openings leave {MIN_WEB} mm or more of wall between them")


def _paths(r: Report, lay: case.Layout, cover: Part, plate: Part) -> None:
    """A maximum Type-C overmold and each LED's light reach their part's face."""
    blocked = []
    for o in lay.openings:
        if o.kind == "usb-c":
            w, h, rad, length = USB_C_PLUG
            profile = RectangleRounded(w, h, rad)
        elif o.kind == "light":
            profile, length = SlotOverall(o.width - 0.1, o.height - 0.1), 10.0
        path = extrude(o.plane(o.y_face) * profile, amount=length)
        if (path & cover).volume > 1e-3 or (path & plate).volume > 1e-3:
            blocked.append(o.label or o.ref)
    if blocked:
        r.fail("blocked from outside: " + ", ".join(blocked))
    else:
        r.ok("plugs and light reach every part face")
