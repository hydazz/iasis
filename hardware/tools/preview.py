"""Write the board previews: preview/board.png, a 3D render of the top, and
preview/layout.png, every copper layer's tracks and pads with the top silkscreen's outlines,
as KiCad's PCB editor shows them with zone fills hidden.

The layout is a KiCad SVG plot without text, rasterised by resvg without anti-aliasing and
packed by oxipng: its few flat colours make a small palette PNG. A preview is rewritten only
when its content changes. KiCad's basic-quality render of an unchanged board still differs
in a few pixels by a level or two, under 10 bytes of pixel data, so the render is replaced
only when more than NOISE bytes change. Unchanged files are touched so mise sees the task as
up to date.
"""

import re
import struct
import tempfile
import zlib
from pathlib import Path

import pcbnew as pcb
import wx

from board import cli, copper_layers, parser, paths, run, variant_args

BACKGROUND = "#001023"  # KiCad's PCB editor background
NOISE = 64


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
    ap = parser(__doc__)
    ap.add_argument("--variant", help="population for the 3D render")
    args = ap.parse_args()
    source, _ = paths(args)
    output = args.output.resolve() if args.output else source.parent / "preview"
    output.mkdir(parents=True, exist_ok=True)
    board = pcb.LoadBoard(str(source))
    copper = copper_layers(board)
    layers = ",".join(list(reversed(copper[1:-1])) + ["B.Cu", "F.Cu", "F.Silkscreen", "Edge.Cuts"])
    with tempfile.TemporaryDirectory(dir=output) as tmp:
        unfilled, svg = Path(tmp, source.name), Path(tmp, "layout.svg")
        layout, render = Path(tmp, "layout.png"), Path(tmp, "board.png")
        # Zone fills would hide the tracks beneath them. Save only the plotting copy.
        for zone in board.Zones():
            zone.UnFill()
        pcb.SaveBoard(str(unfilled), board)
        cli("pcb", "export", "svg", "--mode-single", "--layers", layers,
            "--page-size-mode", "2", "--exclude-drawing-sheet", "--drill-shape-opt", "2",
            "--theme", "_builtin_default", "--output", str(svg), str(unfilled))  # fmt: skip
        svg.write_text(without_text(svg.read_text()))
        run("resvg", "--width", "2000", "--background", BACKGROUND,
            "--shape-rendering", "crispEdges", str(svg), str(layout))  # fmt: skip
        run("oxipng", "--quiet", "--opt", "4", "--strip", "safe", str(layout))
        write(layout, output / "layout.png", lambda a, b: a.read_bytes() == b.read_bytes())

        cli("pcb", "render", *variant_args(args.variant), "--side", "top",
            "--width", "1300", "--height", "1000", "--zoom", "1.25",
            "--quality", "basic", "--output", str(render), str(source))  # fmt: skip
        write(render, output / "board.png", same_render)


if __name__ == "__main__":
    app = wx.App(False)
    main()
