"""Export Gerbers, drills, and the Hub/HubPD assembly files."""

import argparse
import os
import shutil
import subprocess
import tempfile
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
LAYERS = "F.Cu,In1.Cu,In2.Cu,In3.Cu,In4.Cu,B.Cu,F.Paste,F.Silkscreen,B.Silkscreen,F.Mask,B.Mask,Edge.Cuts"


def run(*args):
    subprocess.run([str(a) for a in args], check=True, cwd=HERE)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--logo", type=Path, help="filled SVG for the back-silkscreen logo")
    parser.add_argument("--output", type=Path, help="fabrication output directory")
    args = parser.parse_args()
    output = args.output.resolve() if args.output else HERE / "build"
    cli = os.environ["KICAD_CLI"]
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=output) as directory:
        work = Path(directory)
        board = HERE / "iasis.kicad_pcb"
        if args.logo:
            board = work / "iasis.kicad_pcb"
            run(
                os.environ["KICAD_PYTHON"],
                HERE / "artwork.py",
                HERE / board.name,
                args.logo.resolve(),
                board,
            )
        shutil.copyfile(HERE / "iasis.kicad_pro", work / "iasis.kicad_pro")
        shutil.copyfile(HERE / "iasis.kicad_dru", work / "iasis.kicad_dru")
        run(
            cli,
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
            output / "bottom.svg",
            board,
        )
        gerbers = work / "gerbers"
        run(
            cli,
            "pcb",
            "export",
            "gerbers",
            "--check-zones",
            "-o",
            gerbers,
            "-l",
            LAYERS,
            board,
        )
        run(
            cli,
            "pcb",
            "export",
            "drill",
            "--excellon-separate-th",
            "-o",
            gerbers,
            board,
        )
        with zipfile.ZipFile(work / "gerbers.zip", "w", zipfile.ZIP_DEFLATED) as archive:
            for file in sorted(gerbers.iterdir()):
                if file.suffix != ".gbrjob":
                    archive.write(file, file.name)
        for variant in ("Hub", "HubPD"):
            bom = work / f"bom-{variant}.csv"
            run(
                cli,
                "sch",
                "export",
                "bom",
                "--variant",
                variant,
                "--exclude-dnp",
                "--group-by",
                "LCSC",
                "--ref-range-delimiter",
                "",
                "-o",
                bom,
                "--fields",
                "Value,Reference,Footprint,LCSC,MPN,Manufacturer,${QUANTITY}",
                "--labels",
                "Comment,Designator,Footprint,LCSC Part #,MPN,Manufacturer,Quantity",
                HERE / "iasis.kicad_sch",
            )
            run(
                cli,
                "pcb",
                "export",
                "pos",
                "--variant",
                variant,
                "--exclude-dnp",
                "--format",
                "csv",
                "--units",
                "mm",
                "-o",
                work / "pos.csv",
                board,
            )
            run(
                "python3",
                HERE / "jlc_cpl.py",
                work / "pos.csv",
                bom,
                work / f"cpl-{variant}.csv",
            )
        for pattern in ("gerbers.zip", "bom-*.csv", "cpl-*.csv"):
            for file in work.glob(pattern):
                file.replace(output / file.name)
        shutil.copyfile(board, output / board.name)
        shutil.copyfile(work / "iasis.kicad_pro", output / "iasis.kicad_pro")
        shutil.copyfile(work / "iasis.kicad_dru", output / "iasis.kicad_dru")


if __name__ == "__main__":
    main()
