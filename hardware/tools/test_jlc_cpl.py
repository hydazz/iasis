"""Placement contracts: origin transforms, footprint identity and assembly selection."""

import csv
import tempfile
import unittest
from pathlib import Path

from jlc_cpl import convert, placement


class Placements(unittest.TestCase):
    def test_rotated_offset(self):
        correction = {"x": "0", "y": "-1.302", "rotation": "0"}
        for angle, expected in [
            (0, (10, 21.302)),
            (90, (8.698, 20)),
            (180, (10, 18.698)),
            (270, (11.302, 20)),
        ]:
            with self.subTest(angle=angle):
                x, y, rotation = placement(10, 20, angle, "top", correction)
                self.assertAlmostEqual(x, expected[0])
                self.assertAlmostEqual(y, expected[1])
                self.assertEqual(rotation, angle)

    def test_bottom_correction_needs_measurement(self):
        with self.assertRaisesRegex(ValueError, "top-side"):
            placement(0, 0, 90, "bottom", {"x": "0", "y": "0", "rotation": "270"})
        self.assertEqual(placement(1, 2, 90, "bottom", None), (1, 2, 90))

    def test_correction_matches_part_and_footprint(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            bom, pos, out = (root / name for name in ("bom.csv", "pos.csv", "out.csv"))
            bom.write_text(
                "Designator,LCSC Part #,Footprint\n"
                '"Q1, Q2",C8545,Package_TO_SOT_SMD:SOT-23\n'
                "Q3,C8545,Custom:DifferentOrientation\n"
            )
            pos.write_text(
                "Ref,Val,Package,PosX,PosY,Rot,Side\n"
                "Q1,2N7002,SOT-23,10,20,270,top\n"
                "Q2,2N7002,SOT-23,11,20,0,top\n"
                "Q3,2N7002,DifferentOrientation,12,20,270,top\n"
                "FID1,Fiducial,Fiducial,0,0,0,top\n"
            )
            convert(pos, bom, out)
            with out.open(newline="") as file:
                rows = list(csv.DictReader(file))
            self.assertEqual([row["Designator"] for row in rows], ["Q1", "Q2", "Q3"])
            self.assertEqual([float(row["Rotation"]) for row in rows], [90, 180, 270])


if __name__ == "__main__":
    unittest.main()
