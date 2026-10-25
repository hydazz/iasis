"""Render case previews with OCCT's offscreen viewer on macOS."""

import ctypes
from io import BytesIO
from pathlib import Path

from build123d.topology.shape_core import Shape
from OCP.OCP.AIS import AIS_InteractiveContext, AIS_Shape
from OCP.OCP.Aspect import Aspect_DisplayConnection, Aspect_TOL_SOLID
from OCP.OCP.Cocoa import Cocoa_Window
from OCP.OCP.GeomAbs import GeomAbs_C0
from OCP.OCP.Graphic3d import Graphic3d_BT_RGBA
from OCP.OCP.Image import Image_AlienPixMap
from OCP.OCP.OpenGl import OpenGl_GraphicDriver
from OCP.OCP.Prs3d import Prs3d_LineAspect
from OCP.OCP.Quantity import Quantity_Color, Quantity_TOC_sRGB
from OCP.OCP.TCollection import TCollection_AsciiString
from OCP.OCP.V3d import V3d_Viewer
from PIL import Image, ImageOps


def render(parts: list[tuple[Shape, tuple[int, int, int]]], view: tuple[float, float, float],
           size: tuple[int, int] = (1400, 900), supersample: int = 2) -> Image.Image:  # fmt: skip
    """Orthographic render with shaded faces, sharp edges and a transparent margin."""
    # Cocoa needs an application object even for an unmapped, virtual window.
    ctypes.CDLL("/System/Library/Frameworks/AppKit.framework/AppKit").NSApplicationLoad()
    driver = OpenGl_GraphicDriver(Aspect_DisplayConnection())
    driver.ChangeOptions().buffersOpaqueAlpha = False
    viewer = V3d_Viewer(driver)
    viewer.SetDefaultLights()
    viewer.SetLightOn()
    camera = viewer.CreateView()
    window = Cocoa_Window("Iasis preview", 0, 0, *size)
    window.SetVirtual(True)
    camera.SetWindow(window)
    context = AIS_InteractiveContext(viewer)
    edge = Prs3d_LineAspect(Quantity_Color(0.3, 0.3, 0.3, Quantity_TOC_sRGB), Aspect_TOL_SOLID, 1)
    for shape, colour in parts:
        item = AIS_Shape(shape.wrapped)
        item.SetColor(Quantity_Color(*(v / 255 for v in colour), Quantity_TOC_sRGB))
        item.Attributes().SetFaceBoundaryDraw(True)
        item.Attributes().SetFaceBoundaryUpperContinuity(GeomAbs_C0)
        item.Attributes().SetFaceBoundaryAspect(edge)
        context.Display(item, 1, -1, False)
    camera.SetProj(*view)
    camera.SetUp(0, 0, 1)
    camera.FitAll(0.03, False)
    camera.ZFitAll()

    pixels = Image_AlienPixMap()
    if not camera.ToPixMap(pixels, size[0] * supersample, size[1] * supersample, Graphic3d_BT_RGBA):
        raise RuntimeError("OCCT could not render the case preview")
    buffer = BytesIO()
    if not pixels.Save(buffer, TCollection_AsciiString(".png")):
        raise RuntimeError("OCCT could not encode the case preview")
    buffer.seek(0)
    image = Image.open(buffer).convert("RGBa").reduce(supersample).convert("RGBA")
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
