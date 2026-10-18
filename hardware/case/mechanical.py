"""Write STEP companion metadata using KiCad's Python, without modifying the board."""

import json
import sys
from pathlib import Path

import pcbnew
import wx

# Maximum package heights. XY envelopes use the board's placed front courtyards.
# PROD Tech PSPMAA series A3.0, 1040 package drawing: C = 4.0 mm max.
# https://datasheet.lcsc.com/datasheet/pdf/08c11c8d5a8f832fc44fdee6ce5245a5.pdf
# TI LMR51450 DRR0012G and LM74502 DDF0008A package drawings.
# WCH CH224 datasheet V2.0, section 9.2 DFN10: 0.75 +/- 0.05 mm.
HEIGHTS = {
    "PSPMAA1040-100M-ANP": 4.0,
    "LMR51450SDRRR": 0.8,
    "LM74502DDFR": 1.1,
    "CH224Q": 0.8,
}


def inventory(pcb: Path) -> dict:
    app = wx.App(False)  # noqa: F841
    board = pcbnew.LoadBoard(str(pcb))
    parts = []
    for fp in board.GetFootprints():
        ref = fp.GetReference()
        if ref.startswith(("H", "TP", "FID")):
            continue  # bare mounting holes and copper marks have no component body
        fields = {f.GetName(): f.GetText() for f in fp.GetFields()}
        mpn = fields.get("MPN", "")
        item = {"ref": ref, "mpn": mpn}
        if not list(fp.Models()) and mpn in HEIGHTS:
            boxes = [
                g.GetBoundingBox() for g in fp.GraphicalItems() if g.GetLayer() == pcbnew.F_CrtYd
            ]
            if not boxes:
                raise ValueError(f"{ref} has no front courtyard for its clearance envelope")
            item["envelope"] = {
                "xy": [
                    pcbnew.ToMM(min(b.GetX() for b in boxes)),
                    pcbnew.ToMM(min(b.GetY() for b in boxes)),
                    pcbnew.ToMM(max(b.GetRight() for b in boxes)),
                    pcbnew.ToMM(max(b.GetBottom() for b in boxes)),
                ],
                "height": HEIGHTS[mpn],
            }
        parts.append(item)
    return {"parts": parts}


if __name__ == "__main__":
    Path(sys.argv[2]).write_text(json.dumps(inventory(Path(sys.argv[1])), indent=2) + "\n")
