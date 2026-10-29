"""Convert KiCad positions to JLCPCB placements using measured footprint corrections."""

import csv
import math
import sys
from pathlib import Path

CORRECTIONS = Path(__file__).with_name("jlc_corrections.csv")


def placement(x, y, rotation, side, correction):
    if correction:
        if side != "top":
            raise ValueError("measured placement corrections currently cover top-side parts only")
        dx, dy, turn = (float(correction[key]) for key in ("x", "y", "rotation"))
        angle = math.radians(rotation)
        # Position-file Y points up; a footprint's local Y points down.
        x += dx * math.cos(angle) + dy * math.sin(angle)
        y += dx * math.sin(angle) - dy * math.cos(angle)
        rotation = (rotation + turn) % 360
    return x, y, rotation


def convert(pos, bom, output):
    with CORRECTIONS.open(newline="") as file:
        corrections = {(row["lcsc"], row["footprint"]): row for row in csv.DictReader(file)}
    parts = {}
    with open(bom, newline="") as file:
        for row in csv.DictReader(file):
            for ref in row["Designator"].split(","):
                parts[ref.strip()] = (row["LCSC Part #"], row["Footprint"])
    with open(pos, newline="") as file:
        rows = list(csv.reader(file))[1:]
    with open(output, "w", newline="") as file:
        writer = csv.writer(file, lineterminator="\n")
        writer.writerow(["Designator", "Value", "Package", "Mid X", "Mid Y", "Rotation", "Layer"])
        for ref, value, package, x, y, rotation, side in rows:
            # KiCad can include footprints excluded from the schematic BOM.
            if ref not in parts:
                continue
            correction = corrections.get(parts[ref])
            try:
                x, y, rotation = placement(float(x), float(y), float(rotation), side, correction)
            except ValueError as error:
                raise ValueError(f"{ref}: {error}") from error
            writer.writerow([ref, value, package, f"{x:.6f}", f"{y:.6f}", f"{rotation:.6f}", side])


if __name__ == "__main__":
    convert(*sys.argv[1:])
