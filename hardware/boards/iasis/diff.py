"""Show the board's changes between two revisions in the browser: build/boards/iasis/diff/index.html has one
image per layer, with what was removed red, what was added green and what stayed grey, and the
count of changed tracks, vias and footprints.

Usage: diff.py [OLD [NEW]]. OLD defaults to HEAD and NEW to the working copy; both take any git
revision. Zone fills are left out: they follow every via and track and would hide the change.
"""

import html
import os
import re
import subprocess
import sys
import tempfile
import webbrowser
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

HERE = Path(__file__).resolve().parent
BOARD = HERE / "iasis.kicad_pcb"
OUT = HERE.parents[2] / "build" / "boards" / "iasis" / "diff"
LAYERS = ("F.Cu", "In1.Cu", "In2.Cu", "In3.Cu", "In4.Cu", "B.Cu", "F.Silkscreen", "B.Silkscreen",
          "F.Courtyard", "B.Courtyard", "Edge.Cuts")  # fmt: skip
PX_PER_MM = 24
SAME, REMOVED, ADDED = "#c4c4c4", "#d7301f", "#1a9641"


def run(*command: str) -> None:
    subprocess.run(command, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def board_text(rev: str | None) -> str:
    if rev is None:
        return BOARD.read_text()
    root = Path(
        subprocess.check_output(
            ["git", "rev-parse", "--show-toplevel"], cwd=HERE, text=True
        ).strip()
    )
    path = BOARD.relative_to(root).as_posix()
    paths = subprocess.check_output(
        ["git", "ls-tree", "-r", "--full-tree", "--name-only", rev], cwd=HERE, text=True
    ).splitlines()
    if path not in paths:
        # Revisions before a directory move still use the board's original path.
        matches = [p for p in paths if Path(p).name == BOARD.name]
        if len(matches) != 1:
            raise ValueError(f"{rev}: cannot identify {BOARD.name}")
        path = matches[0]
    return subprocess.check_output(["git", "show", f"{rev}:{path}"], cwd=HERE, text=True)


def blocks(text: str) -> list[str]:
    """The top-level (...) items of a board file as KiCad writes it: each starts a line, one tab in."""
    return re.split(r"\n\t(?=\()", text)[1:]


def items(text: str) -> tuple[Counter, dict]:
    """Tracks and vias as a multiset of their text without the uuid; footprints by reference."""
    copper, parts = Counter(), {}
    for b in blocks(text):
        kind = re.match(r"\((\w+)", b)[1]
        if kind in ("segment", "arc", "via"):
            layer = "vias" if kind == "via" else re.search(r'\(layer "([^"]+)"\)', b)[1]
            copper[(layer, re.sub(r'\s*\(uuid "[^"]*"\)', "", b))] += 1
        elif kind == "footprint":
            ref = re.search(r'\(property "Reference" "([^"]*)"', b)
            at = re.search(r"\(at ([-\d.]+ [-\d.]+(?: [-\d.]+)?)\)", b)
            side = re.search(r'\(layer "([^"]+)"\)', b)
            if ref and at:
                parts[ref[1]] = (at[1], side[1] if side else "")
    return copper, parts


def summary(old: str, new: str) -> tuple[list[tuple[str, int, int]], list[str]]:
    (oc, op), (nc, np) = items(old), items(new)
    gone, came = oc - nc, nc - oc
    rows = []
    for layer in (*LAYERS, "vias"):
        r = sum(n for (lay, _), n in gone.items() if lay == layer)
        a = sum(n for (lay, _), n in came.items() if lay == layer)
        if r or a:
            rows.append((layer, r, a))
    moved = sorted((ref for ref in op.keys() & np.keys() if op[ref] != np[ref]), key=natural)
    moved += [f"{ref} (new)" for ref in sorted(np.keys() - op.keys(), key=natural)]
    moved += [f"{ref} (removed)" for ref in sorted(op.keys() - np.keys(), key=natural)]
    return rows, moved


def natural(ref: str) -> tuple:
    m = re.match(r"([A-Za-z_]*)(\d*)", ref)
    return (m[1], int(m[2] or 0), ref)


def plot(text: str, folder: Path) -> dict[str, str]:
    """Each layer's SVG body, every item drawn in currentColor."""
    folder.mkdir()
    board = folder / BOARD.name
    board.write_text(re.sub(r"\n\t\t\(filled_polygon\n.*?\n\t\t\)", "", text, flags=re.DOTALL))
    run(os.environ["KICAD_CLI"], "pcb", "export", "svg", "--mode-multi", "--black-and-white",
        "--page-size-mode", "1", "--exclude-drawing-sheet", "--drill-shape-opt", "0",
        "--layers", ",".join(LAYERS), "--output", str(folder), str(board))  # fmt: skip
    bodies = {}
    for layer in LAYERS:
        svg = (folder / f"{board.stem}-{layer.replace('.', '_')}.svg").read_text()
        body = svg[svg.index("</desc>") + 7 : svg.rindex("</svg>")]
        body = re.sub(r"<text\b.*?</text>", "", body, flags=re.DOTALL)  # hidden search copies
        bodies[layer] = re.sub(r"#(000000|FFFFFF)", "currentColor", body, flags=re.IGNORECASE)
    return bodies


def frame(*texts: str, margin: float = 2.0) -> tuple[float, float, float, float]:
    """The board outline's bounding box in millimetres, both revisions together. The plots use
    the page's coordinates, which are the board's."""
    xs, ys = [], []
    for text in texts:
        for b in blocks(text):
            if b.startswith("(gr_") and '(layer "Edge.Cuts")' in b:
                for x, y in re.findall(r"\((?:start|mid|end|center|xy) ([-\d.]+) ([-\d.]+)\)", b):
                    xs.append(float(x))
                    ys.append(float(y))
    x0, y0 = min(xs) - margin, min(ys) - margin
    return x0, y0, max(xs) + margin - x0, max(ys) + margin - y0


def compose(old: str, new: str, box: tuple[float, float, float, float]) -> str:
    """Unchanged ink is new masked by old, removed is old outside new, added is new outside old."""
    x, y, w, h = box
    area = f'x="{x}" y="{y}" width="{w}" height="{h}"'
    region = f'maskUnits="userSpaceOnUse" {area}'
    return f"""<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink"
 width="{w}mm" height="{h}mm" viewBox="{x} {y} {w} {h}">
<defs>
<g id="o">{old}</g>
<g id="n">{new}</g>
<mask id="in-o" {region}><use xlink:href="#o" color="#fff"/></mask>
<mask id="out-o" {region}><rect {area} fill="#fff"/><use xlink:href="#o" color="#000"/></mask>
<mask id="out-n" {region}><rect {area} fill="#fff"/><use xlink:href="#n" color="#000"/></mask>
</defs>
<rect {area} fill="#fff"/>
<use xlink:href="#n" color="{SAME}" mask="url(#in-o)"/>
<use xlink:href="#o" color="{REMOVED}" mask="url(#out-n)"/>
<use xlink:href="#n" color="{ADDED}" mask="url(#out-o)"/>
</svg>
"""


def render(svg: Path, png: Path, width: int) -> None:
    run(
        "resvg",
        "--width",
        str(width),
        "--shape-rendering",
        "crispEdges",
        str(svg),
        str(png),
    )
    run("oxipng", "--quiet", "--opt", "2", "--strip", "safe", str(png))


PAGE = """<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>Board diff {title}</title>
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
body {{ margin: 0; font: 14px/1.4 -apple-system, system-ui, sans-serif; background: #f4f4f4; color: #222; }}
header {{ padding: 10px 16px; display: flex; flex-wrap: wrap; gap: 8px 20px; align-items: center; }}
h1 {{ font-size: 16px; margin: 0; }}
.key span {{ display: inline-block; width: 11px; height: 11px; margin: 0 4px 0 10px; vertical-align: -1px; }}
nav {{ display: flex; flex-wrap: wrap; gap: 6px; padding: 0 16px 10px; }}
nav button {{ font: inherit; border: 1px solid #bbb; background: #fff; border-radius: 4px; padding: 3px 9px; cursor: pointer; }}
nav button.on {{ background: #222; color: #fff; border-color: #222; }}
nav small {{ color: {removed}; }} nav small + small {{ color: {added}; }} nav button.on small {{ filter: brightness(1.8); }}
#view {{ position: relative; overflow: hidden; height: calc(100vh - 150px); background: #fff; border-top: 1px solid #ddd; border-bottom: 1px solid #ddd; cursor: grab; }}
#view img {{ position: absolute; left: 0; top: 0; transform-origin: 0 0; image-rendering: pixelated; user-select: none; }}
details {{ padding: 8px 16px 16px; }} td {{ padding: 1px 12px 1px 0; }}
</style></head><body>
<header><h1>{title}</h1>
<div class="key"><span style="background:{removed}"></span>removed<span style="background:{added}"></span>added<span style="background:{same}"></span>unchanged</div>
<div>scroll to zoom, drag to pan, double-click to fit</div></header>
<nav>{buttons}</nav>
<div id="view"><img id="img" alt="layer diff" draggable="false"></div>
<details{open}><summary>Changed items</summary><table>{rows}</table><p>{moved}</p></details>
<script>
const view = document.getElementById('view'), img = document.getElementById('img');
let s = 1, x = 0, y = 0, drag = null;
const put = () => img.style.transform = `translate(${{x}}px,${{y}}px) scale(${{s}})`;
const fit = () => {{ s = Math.min(view.clientWidth / img.naturalWidth, view.clientHeight / img.naturalHeight);
  x = (view.clientWidth - img.naturalWidth * s) / 2; y = (view.clientHeight - img.naturalHeight * s) / 2; put(); }};
let first = true;
img.onload = () => {{ if (first) {{ fit(); first = false; }} }};
function show(name) {{
  document.querySelectorAll('nav button').forEach(b => b.classList.toggle('on', b.dataset.layer === name));
  img.src = name.replace('.', '_') + '.png'; try {{ localStorage.setItem('layer', name); }} catch (e) {{}}
}}
document.querySelectorAll('nav button').forEach(b => b.onclick = () => show(b.dataset.layer));
view.addEventListener('wheel', e => {{ e.preventDefault(); const r = view.getBoundingClientRect();
  const k = Math.exp(-e.deltaY * 0.0015), px = e.clientX - r.left, py = e.clientY - r.top;
  x = px - (px - x) * k; y = py - (py - y) * k; s *= k; put(); }}, {{ passive: false }});
view.onmousedown = e => drag = [e.clientX - x, e.clientY - y];
window.onmouseup = () => drag = null;
window.onmousemove = e => {{ if (drag) {{ x = e.clientX - drag[0]; y = e.clientY - drag[1]; put(); }} }};
view.ondblclick = fit;
let start = null; try {{ start = localStorage.getItem('layer'); }} catch (e) {{}}
show([...document.querySelectorAll('nav button')].some(b => b.dataset.layer === start) ? start : '{first}');
</script></body></html>
"""


def main() -> None:
    old_rev = sys.argv[1] if len(sys.argv) > 1 else "HEAD"
    new_rev = sys.argv[2] if len(sys.argv) > 2 else None
    old, new = board_text(old_rev), board_text(new_rev)
    title = f"{old_rev} → {new_rev or 'working copy'}"
    OUT.mkdir(parents=True, exist_ok=True)
    for png in OUT.glob("*.png"):
        png.unlink()
    with tempfile.TemporaryDirectory(dir=OUT) as tmp:
        a, b = plot(old, Path(tmp, "old")), plot(new, Path(tmp, "new"))
        box = frame(old, new)
        width = round(box[2] * PX_PER_MM)
        jobs = []
        for layer in LAYERS:
            svg = Path(tmp, f"{layer}.svg")
            svg.write_text(compose(a[layer], b[layer], box))
            jobs.append((svg, OUT / f"{layer.replace('.', '_')}.png", width))
        with ThreadPoolExecutor(os.cpu_count()) as pool:
            list(pool.map(lambda job: render(*job), jobs))
    rows, moved = summary(old, new)
    count = {layer: (r, n) for layer, r, n in rows}
    buttons = "".join(
        f'<button data-layer="{layer}">{layer}'
        + (f" <small>−{count[layer][0]}</small> <small>+{count[layer][1]}</small>" if layer in count else "")
        + "</button>"
        for layer in LAYERS
    )  # fmt: skip
    table = "".join(f"<tr><td>{layer}</td><td>−{r}</td><td>+{n}</td></tr>" for layer, r, n in rows)
    first = next((layer for layer, _, _ in rows if layer in LAYERS), LAYERS[0])
    page = PAGE.format(
        title=html.escape(title), buttons=buttons, first=first, open=" open" if rows or moved else "",
        rows=table or "<tr><td>no track or via changes</td></tr>",
        moved=html.escape("Moved footprints: " + ", ".join(moved)) if moved else "No footprint moved.",
        removed=REMOVED, added=ADDED, same=SAME,
    )  # fmt: skip
    index = OUT / "index.html"
    index.write_text(page)
    print(f"{index.relative_to(HERE.parents[2])}: {title}")
    webbrowser.open(index.as_uri())


if __name__ == "__main__":
    main()
