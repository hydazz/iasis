"""Offscreen case previews using pygfx and the OCP tessellator."""

from pathlib import Path

import numpy as np
import pygfx as gfx
from build123d.topology.shape_core import Shape
from ocp_tessellate.tessellator import Tessellator
from PIL import Image, ImageOps
from rendercanvas.offscreen import RenderCanvas


def render(parts: list[tuple[Shape, tuple[int, int, int]]], view: tuple[float, float, float],
           size: tuple[int, int] = (1400, 900), supersample: int = 2) -> Image.Image:  # fmt: skip
    """Shaded orthographic view with a transparent margin."""
    canvas = RenderCanvas(size=size, pixel_ratio=supersample)
    renderer = gfx.WgpuRenderer(canvas, pixel_ratio=supersample)
    model = gfx.Group()
    for shape, colour in parts:
        mesh = Tessellator("")
        mesh.compute(shape.wrapped, 0.05, 0.2, compute_edges=False)
        geometry = gfx.Geometry(
            positions=mesh.get_vertices().reshape(-1, 3),
            indices=mesh.get_triangles().reshape(-1, 3),
            normals=mesh.get_normals().reshape(-1, 3),
        )
        material = gfx.MeshPhongMaterial(color=tuple(v / 255 for v in colour), side="both")
        model.add(gfx.Mesh(geometry, material))
    camera = gfx.OrthographicCamera(*size)
    camera.show_object(model, view_dir=tuple(-v for v in view), up=(0, 0, 1))
    camera.add(gfx.DirectionalLight(intensity=2))
    scene = gfx.Scene()
    scene.add(model, camera, gfx.AmbientLight(intensity=0.6))
    canvas.request_draw(lambda: renderer.render(scene, camera))
    image = Image.fromarray(np.asarray(canvas.draw()))
    canvas.close()
    image = image.convert("RGBa").reduce(supersample).convert("RGBA")
    return ImageOps.expand(image.crop(image.getbbox()), border=24, fill=0)


def save(image: Image.Image, path: Path) -> bool:
    """Write image unless path already holds the same pixels. True when written."""
    if path.exists():
        with Image.open(path) as old:
            if old.size == image.size and old.convert("RGBA").tobytes() == image.tobytes():
                path.touch()
                return False
    image.save(path)
    return True
