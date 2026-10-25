"""Shared KiCad project paths, STEP export and native design checks."""

import argparse
import json
import os
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def parser(description):
    result = argparse.ArgumentParser(description=description)
    result.add_argument(
        "board", nargs="?", type=Path, help="PCB; defaults to this directory's project"
    )
    result.add_argument("--output", type=Path, help="output directory")
    return result


def paths(args):
    if args.board:
        board = args.board.resolve()
    else:
        projects = list(Path.cwd().glob("*.kicad_pro"))
        if len(projects) != 1:
            raise ValueError("specify a board or run in a directory with one KiCad project")
        board = projects[0].with_suffix(".kicad_pcb").resolve()
    output = args.output.resolve() if args.output else ROOT / "build" / "boards" / board.stem
    return board, output


def run(*args, **kwargs):
    return subprocess.run([str(arg) for arg in args], check=True, **kwargs)


def cli(*args, **kwargs):
    return run(os.environ.get("KICAD_CLI", "kicad-cli"), *args, **kwargs)


def variants(board):
    project = json.loads(board.with_suffix(".kicad_pro").read_text())
    return [item["name"] for item in project["schematic"].get("variants", [])] or [None]


def variant_args(variant):
    return ["--variant", variant] if variant else []


def main():
    ap = parser(__doc__)
    ap.add_argument("--variant", help="population for board.step")
    ap.add_argument(
        "--extra-variant", action="append", default=[], help="also write board-VARIANT.step"
    )
    ap.add_argument(
        "--check", action="store_true", help="run DRC/parity and ERC instead of STEP export"
    )
    args = ap.parse_args()
    board, output = paths(args)
    output.mkdir(parents=True, exist_ok=True)
    if args.check:
        # Produce both reports even if one design check fails.
        failed = False
        for command in (
            [
                "pcb",
                "drc",
                "--schematic-parity",
                "--refill-zones",
                "--severity-error",
                "--exit-code-violations",
                "--format",
                "json",
                "-o",
                output / "drc.json",
                board,
            ],
            [
                "sch",
                "erc",
                "--severity-error",
                "--exit-code-violations",
                "--format",
                "json",
                "-o",
                output / "erc.json",
                board.with_suffix(".kicad_sch"),
            ],
        ):
            try:
                cli(*command)
            except subprocess.CalledProcessError:
                failed = True
        raise SystemExit(1 if failed else 0)
    exports = [(args.variant, "board.step")]
    exports += [(variant, f"board-{variant.lower()}.step") for variant in args.extra_variant]
    for variant, name in exports:
        cli(
            "pcb",
            "export",
            "step",
            "--subst-models",
            "--force",
            "--no-dnp",
            *variant_args(variant),
            "-o",
            output / name,
            board,
        )


if __name__ == "__main__":
    main()
