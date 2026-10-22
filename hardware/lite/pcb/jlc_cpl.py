"""Write a JLCPCB CPL from KiCad's position file so that JLC's footprints land on our pads.

    jlc_cpl.py POS.csv BOM.csv OUT.csv

JLC draws each part's EasyEDA footprint with its origin at Mid X/Y, turned by Rotation.
FIX lists, by LCSC code, the parts whose EasyEDA footprint differs from the KiCad one: the
counter-clockwise turn to add, and where the EasyEDA origin sits in the unrotated KiCad
footprint (mm, x right, y down). Each was found by matching the EasyEDA footprint's pads
to the KiCad pads by number; JLC's Align then changes nothing.
"""

import csv
import math
import sys

FIX = {
    "C555456": (270, None),  # TCA9548ARGER, VQFN-24
    "C2673275": (270, None),  # TMUX1511RSVR, UQFN-16
    "C5948": (270, None),  # 74HC595PW, TSSOP-16
    "C548572": (270, None),  # 74LVC2G08DC-Q100H, VSSOP-8
    "C2070918": (270, None),  # AP3441SHE-7B, DFN-8
    "C544517": (270, None),  # HD3SS3212IRKSR, DHVQFN-20
    "C106794": (270, None),  # TPD4E02B04DQAR, USON-10
    "C132291": (270, None),  # FUSB302BMPX, WQFN-14
    "C3236215": (270, None),  # LM74502DDFR, SOT-8
    "C544399": (270, None),  # TPS26600PWPR, HTSSOP-16
    "C181499": (270, None),  # TCA6408ARGTR, VQFN-16
    "C8545": (180, None),  # 2N7002, SOT-23
    "C7484": (180, None),  # SN74AHCT1G125DBVR, SOT-23-5
    "C507223": (180, None),  # SN74AUP1G08DCKR, SC-70-5
    "C43698": (180, None),  # TPS3808G33DBVR, SOT-23-6
    "C2158037": (180, None),  # AP22653W6-7, SOT-23-6
    "C5184243": (0, (0.0, -1.302)),  # GCT USB4105-GF-A-120
    "C134092": (0, (0.0, -0.334)),  # Molex 105450-0101
}


def quoted(text: str) -> str:
    return '"' + text.replace('"', '""') + '"'


def main(pos: str, bom: str, out: str) -> None:
    code = {}
    with open(bom, newline="") as f:
        for row in csv.DictReader(f):
            for ref in row["Designator"].split(","):
                code[ref] = row["LCSC Part #"]
    with open(pos, newline="") as f:
        rows = list(csv.reader(f))[1:]
    lines = ["Designator,Value,Package,Mid X,Mid Y,Rotation,Layer"]
    for ref, value, package, x, y, rot, side in rows:
        x, y, rot = float(x), float(y), float(rot)
        if fix := FIX.get(code.get(ref)):
            assert side == "top", f"{ref}: the corrections assume a top-side part"
            turn, origin = fix
            if origin:
                # KiCad's Y points up here; the footprint frame's points down.
                t = math.radians(rot)
                x += origin[0] * math.cos(t) + origin[1] * math.sin(t)
                y += origin[0] * math.sin(t) - origin[1] * math.cos(t)
            rot = (rot + turn) % 360
        lines.append(",".join([quoted(ref), quoted(value), quoted(package),
                               f"{x:.6f}", f"{y:.6f}", f"{rot:.6f}", side]))  # fmt: skip
    with open(out, "w") as f:
        f.write("\n".join(lines) + "\n")


if __name__ == "__main__":
    main(*sys.argv[1:])
