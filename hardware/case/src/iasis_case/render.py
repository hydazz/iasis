"""Shaded PNG renders of build123d shapes for the tracked previews.

A numpy z-buffer rasteriser in parallel projection, so an unchanged case gives the same
pixels. Each face is shaded smoothly; lines mark silhouettes and creases, where depth or
surface direction jumps between neighbouring pixels, and seams, where two parts of one
colour meet. Inlays need no seam: their colour already sets them apart.
"""

from pathlib import Path

import numpy as np
from build123d.topology.shape_core import Shape
from PIL import Image

AMBIENT, DIFFUSE = 0.4, 0.6
CREASE = 0.94  # cosine between neighbouring pixel normals below which a line is drawn
STEP = 1.0  # mm of depth between neighbouring pixels that marks a silhouette
LINE = 0.3  # brightness of lines


def render(parts: list[tuple[Shape, tuple[int, int, int]]], view: tuple[float, float, float],
           size: tuple[int, int] = (1400, 900), supersample: int = 2) -> Image.Image:  # fmt: skip
    """parts seen from view towards the origin, Z up, filling size with a small margin."""
    d = -np.asarray(view, float)
    d /= np.linalg.norm(d)
    right = np.cross(d, (0.0, 0.0, 1.0))
    right /= np.linalg.norm(right)
    up = np.cross(right, d)
    light = -d + 0.6 * up - 0.4 * right
    light /= np.linalg.norm(light)

    tris, normals, colours, owners = [], [], [], []
    for i, (shape, colour) in enumerate(parts):
        verts, faces = shape.tessellate(0.05, 0.2)
        v = np.array([(p.X, p.Y, p.Z) for p in verts])
        t = np.array(faces)
        tri = v[t]
        n = np.cross(tri[:, 1] - tri[:, 0], tri[:, 2] - tri[:, 0])
        vn = np.zeros_like(v)
        for k in range(3):  # vertices are per face, so this smooths within faces only
            np.add.at(vn, t[:, k], n)
        vn /= np.linalg.norm(vn, axis=1, keepdims=True) + 1e-12
        towards = np.where(n @ d > 0, -1.0, 1.0)  # light the side facing the camera
        tris.append(tri)
        normals.append(vn[t] * towards[:, None, None])
        colours.append(np.repeat([colour], len(t), axis=0))
        owners.append(np.full(len(t), i))
    tri, nrm, col = np.concatenate(tris), np.concatenate(normals), np.concatenate(colours)
    own = np.concatenate(owners)

    w, h = size[0] * supersample, size[1] * supersample
    xy = np.stack([tri @ right, -(tri @ up)], axis=-1)
    z = tri @ d
    lo, hi = xy.reshape(-1, 2).min(0), xy.reshape(-1, 2).max(0)
    scale = 0.94 * min(w / (hi[0] - lo[0]), h / (hi[1] - lo[1]))
    xy = (xy - (lo + hi) / 2) * scale + (w / 2, h / 2)

    depth = np.full((h, w), np.inf)
    normal = np.zeros((h, w, 3), np.float32)
    base = np.zeros((h, w, 3), np.float32)
    part = np.full((h, w), -1)
    for (a, b, c), zt, nt, rgb, k in zip(xy, z, nrm, col, own):
        x0, x1 = max(int(min(a[0], b[0], c[0])), 0), min(int(max(a[0], b[0], c[0])) + 1, w)
        y0, y1 = max(int(min(a[1], b[1], c[1])), 0), min(int(max(a[1], b[1], c[1])) + 1, h)
        den = (b[1] - c[1]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[1] - c[1])
        if x0 >= x1 or y0 >= y1 or abs(den) < 1e-12:
            continue
        px, py = np.meshgrid(np.arange(x0, x1) + 0.5, np.arange(y0, y1) + 0.5)
        w0 = ((b[1] - c[1]) * (px - c[0]) + (c[0] - b[0]) * (py - c[1])) / den
        w1 = ((c[1] - a[1]) * (px - c[0]) + (a[0] - c[0]) * (py - c[1])) / den
        w2 = 1 - w0 - w1
        zz = w0 * zt[0] + w1 * zt[1] + w2 * zt[2]
        window = depth[y0:y1, x0:x1]
        hit = (w0 >= -1e-9) & (w1 >= -1e-9) & (w2 >= -1e-9) & (zz < window)
        if not hit.any():
            continue
        window[hit] = zz[hit]
        bary = np.stack([w0[hit], w1[hit], w2[hit]], axis=-1)
        normal[y0:y1, x0:x1][hit] = bary @ nt
        base[y0:y1, x0:x1][hit] = rgb
        part[y0:y1, x0:x1][hit] = k

    covered = np.isfinite(depth)
    depth[~covered] = 0
    normal /= np.linalg.norm(normal, axis=-1, keepdims=True) + 1e-12
    shade = AMBIENT + DIFFUSE * np.clip(normal @ light.astype(np.float32), 0, 1)
    line = np.zeros((h, w), bool)
    for one, other in (
        ((slice(None), slice(1, None)), (slice(None), slice(None, -1))),
        ((slice(1, None), slice(None)), (slice(None, -1), slice(None))),
    ):
        both = covered[one] & covered[other]
        jump = (covered[one] != covered[other]) | both & (
            (np.abs(depth[one] - depth[other]) > STEP)
            | ((normal[one] * normal[other]).sum(-1) < CREASE)
            | (part[one] != part[other]) & (base[one] == base[other]).all(-1)
        )
        line[one] |= jump
        line[other] |= jump
    shade = np.where(line, LINE, shade)
    rgba = np.zeros((h, w, 4), np.uint8)
    rgba[..., :3] = np.clip(base * shade[..., None], 0, 255).round().astype(np.uint8)
    rgba[..., 3] = covered * 255
    return Image.fromarray(rgba, "RGBA").convert("RGBa").reduce(supersample).convert("RGBA")


def save(image: Image.Image, path: Path) -> bool:
    """Write image unless path already holds the same pixels. True when written."""
    if path.exists():
        old = Image.open(path)
        if old.size == image.size and np.array_equal(
            np.asarray(old.convert("RGBA")), np.asarray(image)
        ):
            path.touch()
            return False
    image.save(path)
    return True
