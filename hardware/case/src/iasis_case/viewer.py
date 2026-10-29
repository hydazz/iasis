"""Opaque enclosure and board layers for OCP CAD Viewer."""

from typing import TYPE_CHECKING

from build123d import Color, Compound
from build123d.topology.shape_core import Shape
from ocp_vscode import Collapse, Render, set_viewer_config
from ocp_vscode import show as show_cad

from .case import feet

if TYPE_CHECKING:
    from .case import Cover, Layout


def _coloured(shape: Shape, colour: str) -> Compound:
    return Compound([shape], color=Color(colour))


def show(lay: Layout, tray: Shape, covers: dict[str, Cover]) -> None:
    """Show the tray, populated PCB and both cover variants as opaque layers, the OLED
    cover hidden."""
    pcb = {
        "Components": _coloured(Compound(list(lay.board.parts.values())), "#9a9a9a"),
        "Board": _coloured(lay.board.body, "#1f5e3c"),
    }
    variants = {}
    base = {
        "Body": _coloured(tray, "#d6d8dc"),
        "Feet": _coloured(Compound(feet(lay)), "#353b39"),
    }
    states = {
        **{f"/Group/Tray/{part}": [1, 1] for part in base},
        **{f"/Group/PCB/{part}": [1, 1] for part in pcb},
    }
    for name, cover in covers.items():
        group = {
            "Body": _coloured(cover.part, "#d6d8dc"),
            "Markings": _coloured(cover.markings, "#23262a"),
            "Button": _coloured(cover.button, "#d6d8dc"),
        }
        if cover.display is not None:
            group["Display envelope"] = _coloured(cover.display, "#3d6fd1")
        if cover.retainer is not None:
            group["Retainer"] = _coloured(cover.retainer.part, "#d6d8dc")
        variant = "OLED" if name == "display" else "Passive"
        variants[variant] = group
        for part in group:
            states[f"/Group/Cover/{variant}/{part}"] = [1, 1] if variant == "Passive" else [0, 0]

    show_cad(
        base,
        pcb,
        variants,
        names=["Tray", "PCB", "Cover"],
        alphas=[1.0] * 3,
        modes=[Render.ALL] * 3,
        transparent=False,
        default_opacity=1.0,
        collapse=Collapse.ROOT,
        tab="tree",
    )
    set_viewer_config(states=states)
