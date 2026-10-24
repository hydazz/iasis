"""Opaque enclosure and board layers for OCP CAD Viewer."""

from typing import TYPE_CHECKING

from build123d import Color, Compound
from build123d.topology.shape_core import Shape
from ocp_vscode import Collapse, Render, set_viewer_config
from ocp_vscode import show as show_cad

if TYPE_CHECKING:
    from .board import Board
    from .case import Cover, Layout
    from .side import Side


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
    states = {"/Group/Tray": [1, 1], **{f"/Group/PCB/{part}": [1, 1] for part in pcb}}
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
        _coloured(tray, "#d6d8dc"),
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


def show_cm5(board: Board, side: Side) -> None:
    """Show the CM5 build: both boards, the CM5 cooler and both side-link flex options,
    the rainbow one hidden. Its enclosure, one tray and cover for both boards, is not
    modelled yet."""
    groups = {
        "PCB": {
            "Components": _coloured(Compound(list(board.parts.values())), "#9a9a9a"),
            "Board": _coloured(board.body, "#1f5e3c"),
        },
        "Side board": {
            "Components": _coloured(side.parts, "#9a9a9a"),
            "Board": _coloured(side.body, "#1f5e3c"),
            "Cooler": _coloured(side.cooler, "#b8bcc2"),
        },
        "Flex": {
            "Straight": _coloured(side.straight, "#d9a441"),
            "Rainbow": _coloured(side.rainbow, "#d9a441"),
            "Vertical ZIFs": _coloured(side.vertical_zifs, "#3a3d42"),
        },
    }
    states = {f"/Group/{g}/{part}": [1, 1] for g, parts in groups.items() for part in parts}
    states["/Group/Flex/Rainbow"] = states["/Group/Flex/Vertical ZIFs"] = [0, 0]
    show_cad(
        *groups.values(),
        names=list(groups),
        alphas=[1.0] * len(groups),
        modes=[Render.ALL] * len(groups),
        transparent=False,
        default_opacity=1.0,
        collapse=Collapse.ROOT,
        tab="tree",
    )
    set_viewer_config(states=states)
