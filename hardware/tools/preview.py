"""Write the board previews: preview/board.png, a 3D render of the top, and
preview/layout.png, every copper layer's tracks and pads with the top silkscreen's outlines,
as KiCad's PCB editor shows them with zone fills hidden.

The 3D image uses KiCad's camera fit, with transparent space trimmed to a 24-pixel margin.
The layout is a KiCad SVG plot without text, rasterised by resvg without anti-aliasing and
packed by oxipng. Unchanged files are touched so mise sees the task as up to date.
"""

import re
import tempfile
from pathlib import Path

import pcbnew as pcb
import wx

from board import cli, copper_layers, parser, paths, run, variant_args

BACKGROUND = "#001023"  # KiCad's PCB editor background


def trim(path: Path) -> None:
    image = wx.Image(str(path))
    mask = image.Copy()
    mask.ConvertAlphaToMask(1)
    bounds = wx.Region(wx.Bitmap(mask)).GetBox()
    image = image.GetSubImage(bounds)
    image.Resize((bounds.width + 48, bounds.height + 48), (24, 24))
    image.SaveFile(str(path), wx.BITMAP_TYPE_PNG)


def without_text(svg: str) -> str:
    """Each KiCad SVG text is a hidden <text> for search and a group of strokes."""
    svg = re.sub(r"<text\b.*?</text>\n?", "", svg, flags=re.DOTALL)
    return re.sub(r'<g class="stroked-text">.*?</g>\n?', "", svg, flags=re.DOTALL)


def write(new: Path, out: Path) -> None:
    if out.exists() and new.read_bytes() == out.read_bytes():
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
        write(layout, output / "layout.png")

        cli("pcb", "render", *variant_args(args.variant), "--side", "top",
            "--width", "1600", "--height", "1600", "--background", "transparent",
            "--quality", "basic", "--output", str(render), str(source))  # fmt: skip
        trim(render)
        write(render, output / "board.png")


if __name__ == "__main__":
    app = wx.App(False)
    main()
