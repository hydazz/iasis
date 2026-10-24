"""Opaque enclosure and board layers for OCP CAD Viewer."""

from typing import TYPE_CHECKING

from build123d import Color, Compound
from build123d.topology.shape_core import Shape
from ocp_vscode import Collapse, Render, set_viewer_config
from ocp_vscode import show as show_cad

from .side import Pair

if TYPE_CHECKING:
    from .case import Cover, Layout


def _coloured(shape: Shape, colour: str) -> Compound:
    return Compound([shape], color=Color(colour))


def show(lay: Layout, tray: Shape, covers: dict[str, Cover]) -> None:
    """Show the tray, populated PCB and both cover variants as opaque layers, the OLED
    cover hidden. For the CM5 build, a Flex layer holds both side-link options, the
    rainbow one hidden."""
    board = lay.board
    pair = board if isinstance(board, Pair) else None
    components = [p for ref, p in board.parts.items() if pair is None or ref != "flex"]
    pcb = {
        "Components": _coloured(Compound(components), "#9a9a9a"),
        "Board": _coloured(board.body, "#1f5e3c"),
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

    groups, names = [_coloured(tray, "#d6d8dc"), pcb, variants], ["Tray", "PCB", "Cover"]
    if pair is not None and pair.side is not None:
        s = pair.side
        groups.append(
            {
                "Straight": _coloured(s.straight.moved(pair.shift), "#d9a441"),
                "Rainbow": _coloured(s.rainbow.moved(pair.shift), "#d9a441"),
                "Vertical ZIFs": _coloured(s.vertical_zifs.moved(pair.shift), "#3a3d42"),
            }
        )
        names.append("Flex")
        states["/Group/Flex/Straight"] = [1, 1]
        states["/Group/Flex/Rainbow"] = states["/Group/Flex/Vertical ZIFs"] = [0, 0]

    show_cad(
        *groups,
        names=names,
        alphas=[1.0] * len(groups),
        modes=[Render.ALL] * len(groups),
        transparent=False,
        default_opacity=1.0,
        collapse=Collapse.ROOT,
        tab="tree",
    )
    set_viewer_config(states=states)
