"""Write a cropped KiCad top-view photo beside the board."""

import tempfile
from pathlib import Path

from PIL import Image, ImageOps

from board import cli, parser, paths


def write(new: Path, out: Path) -> None:
    if out.exists() and new.read_bytes() == out.read_bytes():
        out.touch()
    else:
        new.replace(out)
        print(f"{out.name} written")


def main() -> None:
    args = parser(__doc__).parse_args()
    source, _ = paths(args)
    output = args.output.resolve() if args.output else source.parent
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=output) as tmp:
        render = Path(tmp, "board.png")
        cli("pcb", "render", "--side", "top",
            "--width", "1600", "--height", "1600", "--background", "transparent",
            "--quality", "basic", "--output", render, source)  # fmt: skip
        with Image.open(render) as image:
            image = ImageOps.expand(image.crop(image.getbbox()), border=24, fill=0)
            image.save(render)
        write(render, output / "board.png")


if __name__ == "__main__":
    main()
