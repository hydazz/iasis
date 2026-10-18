"""Export the populated board STEP with kicad-cli, the enclosure's only input from KiCad.

    uv run export_board.py [--pcb ../iasis/iasis.kicad_pcb] [--out build/board.step]

kicad-cli rewrites a project's .kicad_prl when it loads a board, so it reads a copy;
the copy sees the project's .3dshapes folders through symlinks. The PCB's SHA-256 is
written beside the STEP so the case build can tell when the export is stale.
"""

import argparse
import hashlib
import itertools
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
PCB = HERE.parent / "iasis" / "iasis.kicad_pcb"
OUT = HERE / "build" / "board.step"
KICAD_CLI_MACOS = "/Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli"


def kicad_cli() -> str:
    return os.environ.get("KICAD_CLI") or shutil.which("kicad-cli") or KICAD_CLI_MACOS


def export(pcb: Path, out: Path) -> list[str]:
    """Write out from pcb; returns kicad-cli's complaints about missing models."""
    out.parent.mkdir(parents=True, exist_ok=True)
    digest = hashlib.sha256(pcb.read_bytes()).hexdigest()
    with tempfile.TemporaryDirectory() as tmp:
        snap = Path(tmp)
        for suffix in (".kicad_pcb", ".kicad_pro"):
            src = pcb.with_suffix(suffix)
            if src.exists():
                shutil.copy2(src, snap / src.name)
        for models in pcb.parent.glob("*.3dshapes"):
            (snap / models.name).symlink_to(models.resolve())
        cmd = [kicad_cli(), "pcb", "export", "step", "--subst-models", "--force"]
        cmd += ["-o", str(out), str(snap / pcb.name)]
        run = subprocess.run(cmd, check=False, capture_output=True, text=True)
    if run.returncode != 0 or not out.exists():
        sys.exit(f"kicad-cli STEP export failed:\n{run.stdout}\n{run.stderr}")
    out.with_suffix(".sha256").write_text(f"{digest}  {pcb}\n")
    lines = run.stdout.splitlines()
    return [f"{a} {b}" for a, b in itertools.pairwise(lines) if a.startswith("Could not add")]


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--pcb", type=Path, default=PCB)
    ap.add_argument("--out", type=Path, default=OUT)
    args = ap.parse_args()
    missing = export(args.pcb.resolve(), args.out.resolve())
    print(f"wrote {args.out}")
    for line in missing:
        print(f"  {line}")


if __name__ == "__main__":
    main()
