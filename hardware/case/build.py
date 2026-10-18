"""Build the Iasis enclosure from the board STEP: parts, fit checks and outputs.

    uv run build.py [--step build/board.step]

Needs only the STEP that export_board.py writes, not KiCad. Writes build/: an assembly
STEP for review, print-oriented STL and 3MF parts and report.txt.
"""

import argparse
import hashlib
import sys
import time
from pathlib import Path

from build123d import Color, Location, Mesher, Rot, export_stl

import board as kicad
import case
import check

HERE = Path(__file__).resolve().parent
BUILD = HERE / "build"
STEP = BUILD / "board.step"

CASE_COLORS = {"plate": (0.20, 0.20, 0.22), "cover": (0.86, 0.86, 0.84)}
ENVELOPE_COLOR = (0.35, 0.55, 0.85, 0.35)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--step", type=Path, default=STEP)
    args = ap.parse_args()
    t0 = time.monotonic()

    def step(msg: str) -> None:
        print(f"[{time.monotonic() - t0:5.1f}s] {msg}", flush=True)

    if not args.step.exists():
        sys.exit(f"{args.step} not found; export it with: mise run board-step")
    if warning := stale(args.step):
        print(f"warning: {warning}")
    BUILD.mkdir(exist_ok=True)
    board = kicad.load(args.step)
    lay = case.layout(board)
    step(f"board {board.size.X:.1f} x {board.size.Y:.1f} mm, {len(board.placed())} placed parts")

    plate = case.plate(lay)
    covers = {name: case.cover(lay, name, v) for name, v in case.COVERS.items()}
    step("built plate and covers")

    report = check.run(lay, plate, covers)
    step("checked fit")

    write_print_parts(plate, covers)
    write_assembly(lay, plate, covers)
    step("wrote STEP, STL and 3MF")

    text = summary(lay, plate, covers, report)
    (BUILD / "report.txt").write_text(text)
    print(text)
    return 1 if report.failures else 0


def stale(step: Path) -> str | None:
    """Whether the board has changed since the STEP was exported (from its .sha256)."""
    stamp = step.with_suffix(".sha256")
    if not stamp.exists():
        return None
    digest, pcb = stamp.read_text().split(maxsplit=1)
    pcb = Path(pcb.strip())
    if pcb.exists() and hashlib.sha256(pcb.read_bytes()).hexdigest() != digest:
        return f"{pcb.name} has changed since {step.name} was exported; run: mise run board-step"
    return None


def write_print_parts(plate, covers) -> None:
    out = BUILD / "print"
    out.mkdir(exist_ok=True)
    parts = {"plate": plate}
    for name, c in covers.items():  # covers print top down
        flipped = Rot(X=180) * c.part
        parts[f"cover-{name}"] = flipped.moved(_to_bed(flipped))
    parts["plate"] = plate.moved(_to_bed(plate))
    for name, part in parts.items():
        export_stl(part, out / f"{name}.stl", tolerance=0.01, angular_tolerance=0.1)
        mesher = Mesher()
        mesher.add_shape(part, linear_deflection=0.01, angular_deflection=0.1, part_number=name)
        mesher.write(str(out / f"{name}.3mf"))


def _to_bed(part):
    bb = part.bounding_box()
    return Location((-bb.center().X, -bb.center().Y, -bb.min.Z))


def write_assembly(lay, plate, covers) -> None:
    """The board's STEP with the plate, every cover and the cooler, fan and display envelopes."""
    plate.color = Color(*CASE_COLORS["plate"])
    extra = [plate]
    for c in covers.values():
        c.part.color = Color(*CASE_COLORS["cover"])
        extra.append(c.part)
    envelopes = [lay.cooler, *(e for c in covers.values() for e in (c.fan, c.display) if e)]
    for envelope in envelopes:
        envelope.color = Color(*ENVELOPE_COLOR)
        extra.append(envelope)
    kicad.export_assembly(lay.board, extra, BUILD / "iasis-case.step")


def summary(lay, plate, covers, report) -> str:
    lines = ["Iasis enclosure", ""]
    for name, c in covers.items():
        bb = c.part.bounding_box()
        v = case.COVERS[name]
        what = ", ".join(
            [v.fan.name if v.fan else "no fan", *([v.display.name] if v.display else [])]
        )
        lines.append(f"  {name:8s} {bb.size.X:.1f} x {bb.size.Y:.1f} x {bb.size.Z:.1f} mm ({what})")
    lines += ["", *report.lines, ""]
    lines.append(f"{len(report.failures)} failures" if report.failures else "all checks passed")
    return "\n".join(lines) + "\n"


if __name__ == "__main__":
    sys.exit(main())
