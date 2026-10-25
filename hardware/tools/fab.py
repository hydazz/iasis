"""Export JLCPCB Gerbers, drills, and assembly files for a KiCad project."""

import shutil
import tempfile
import zipfile
from pathlib import Path

from board import cli, parser, paths, variant_args, variants
from jlc_cpl import convert


def main():
    ap = parser(__doc__)
    ap.add_argument("--no-assembly", action="store_true", help="export a bare PCB only")
    ap.add_argument(
        "--layers",
        default="F.Paste,B.Paste,F.Silkscreen,B.Silkscreen,F.Mask,B.Mask,Edge.Cuts",
        help="non-copper fabrication layers; enabled copper layers are always included",
    )
    args = ap.parse_args()
    source, output = paths(args)
    if output == source.parent:
        ap.error("output must be separate from the source directory")
    output.mkdir(parents=True, exist_ok=True)
    layers = f"*.Cu,{args.layers}"
    with tempfile.TemporaryDirectory(dir=output) as directory:
        work = Path(directory)
        for suffix in (".kicad_pro", ".kicad_dru"):
            file = source.with_suffix(suffix)
            if file.exists():
                shutil.copy2(file, work / file.name)
        cli(
            "pcb",
            "export",
            "svg",
            "--mode-single",
            "--layers",
            "B.Mask,B.Silkscreen,Edge.Cuts",
            "--mirror",
            "--page-size-mode",
            "2",
            "--exclude-drawing-sheet",
            "--output",
            work / "bottom.svg",
            source,
        )
        gerbers = work / "gerbers"
        cli("pcb", "export", "gerbers", "--check-zones", "-o", gerbers, "-l", layers, source)
        cli("pcb", "export", "drill", "--excellon-separate-th", "-o", gerbers, source)
        with zipfile.ZipFile(work / "gerbers.zip", "w", zipfile.ZIP_DEFLATED) as archive:
            for file in sorted(gerbers.iterdir()):
                if file.suffix != ".gbrjob":
                    archive.write(file, file.name)
        if not args.no_assembly:
            for variant in variants(source):
                suffix = f"-{variant}" if variant else ""
                bom, pos = work / f"bom{suffix}.csv", work / "pos.csv"
                cli(
                    "sch",
                    "export",
                    "bom",
                    *variant_args(variant),
                    "--exclude-dnp",
                    "--group-by",
                    "LCSC,Footprint",
                    "--ref-range-delimiter",
                    "",
                    "-o",
                    bom,
                    "--fields",
                    "Value,Reference,Footprint,LCSC,MPN,Manufacturer,${QUANTITY}",
                    "--labels",
                    "Comment,Designator,Footprint,LCSC Part #,MPN,Manufacturer,Quantity",
                    source.with_suffix(".kicad_sch"),
                )
                cli(
                    "pcb",
                    "export",
                    "pos",
                    *variant_args(variant),
                    "--exclude-dnp",
                    "--format",
                    "csv",
                    "--units",
                    "mm",
                    "-o",
                    pos,
                    source,
                )
                convert(pos, bom, work / f"cpl{suffix}.csv")
        for pattern in ("bom.csv", "bom-*.csv", "cpl.csv", "cpl-*.csv"):
            for old in output.glob(pattern):
                old.unlink()
        for pattern in (
            "gerbers.zip",
            "bottom.svg",
            "bom*.csv",
            "cpl*.csv",
            "*.kicad_pro",
            "*.kicad_dru",
        ):
            for file in work.glob(pattern):
                file.replace(output / file.name)
        shutil.copy2(source, output / source.name)


if __name__ == "__main__":
    main()
