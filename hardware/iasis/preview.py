"""Write the tracked board previews: preview/board.png, a 3D render of the Hub PD top, and
preview/layout.png, every copper layer's tracks and pads with the top silkscreen's outlines,
as KiCad's PCB editor shows them with zone fills hidden.

The layout is a KiCad SVG plot without text, rasterised by resvg without anti-aliasing and
packed by oxipng: its few flat colours make a small palette PNG. A preview is rewritten only
when its content changes. KiCad's basic-quality render of an unchanged board still differs
in a few pixels by a level or two, under 10 bytes of pixel data, so the render is replaced
only when more than NOISE bytes change. Unchanged files are touched so mise sees the task as
up to date.
"""

import os
import re
import struct
import subprocess
import tempfile
import zlib
from pathlib import Path

HERE = Path(__file__).resolve().parent
OUT = HERE / "preview"
BOARD = HERE / "iasis.kicad_pcb"
LAYERS = "In4.Cu,In3.Cu,In2.Cu,In1.Cu,B.Cu,F.Cu,F.Silkscreen,Edge.Cuts"  # drawn in this order
BACKGROUND = "#001023"  # KiCad's PCB editor background
NOISE = 64


def run(*command: str) -> None:
    subprocess.run(command, check=True, stdout=subprocess.DEVNULL)


def pixel_data(png: Path) -> bytes:
    data = png.read_bytes()
    chunks, i = [], 8
    while i < len(data):
        size, kind = struct.unpack(">I4s", data[i : i + 8])
        if kind == b"IDAT":
            chunks.append(data[i + 8 : i + 8 + size])
        i += 12 + size
    return zlib.decompress(b"".join(chunks))


def same_render(a: Path, b: Path) -> bool:
    x, y = pixel_data(a), pixel_data(b)
    return len(x) == len(y) and (x == y or sum(p != q for p, q in zip(x, y)) <= NOISE)


def without_text(svg: str) -> str:
    """Each KiCad SVG text is a hidden <text> for search and a group of strokes."""
    svg = re.sub(r"<text\b.*?</text>\n?", "", svg, flags=re.DOTALL)
    return re.sub(r'<g class="stroked-text">.*?</g>\n?', "", svg, flags=re.DOTALL)


def write(new: Path, out: Path, same) -> None:
    if out.exists() and same(new, out):
        out.touch()
    else:
        new.replace(out)
        print(f"preview/{out.name} written")


def main() -> None:
    kicad = os.environ["KICAD_CLI"]
    OUT.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(dir=OUT) as tmp:
        unfilled, svg = Path(tmp, BOARD.name), Path(tmp, "layout.svg")
        layout, render = Path(tmp, "layout.png"), Path(tmp, "board.png")
        # Plot a copy without zone fills: only the inner layers have zones, and their planes
        # would hide every track beneath them.
        unfilled.write_text(
            re.sub(r"\n\t\t\(filled_polygon\n.*?\n\t\t\)", "", BOARD.read_text(), flags=re.DOTALL)
        )
        run(kicad, "pcb", "export", "svg", "--mode-single", "--layers", LAYERS,
            "--page-size-mode", "2", "--exclude-drawing-sheet", "--drill-shape-opt", "2",
            "--theme", "_builtin_default", "--output", str(svg), str(unfilled))  # fmt: skip
        svg.write_text(without_text(svg.read_text()))
        run("resvg", "--width", "2000", "--background", BACKGROUND,
            "--shape-rendering", "crispEdges", str(svg), str(layout))  # fmt: skip
        run("oxipng", "--quiet", "--opt", "4", "--strip", "safe", str(layout))
        write(layout, OUT / "layout.png", lambda a, b: a.read_bytes() == b.read_bytes())

        run(kicad, "pcb", "render", "--variant", "HubPD", "--side", "top",
            "--width", "1300", "--height", "1000", "--zoom", "1.25",
            "--quality", "basic", "--output", str(render), str(BOARD))  # fmt: skip
        write(render, OUT / "board.png", same_render)


if __name__ == "__main__":
    main()
