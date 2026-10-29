"""The board as KiCad exports it: the PCB body and every part, by reference designator.

KiCad's STEP names each part instance after its reference; build123d's import_step keeps
only product names, so this walks the XCAF document itself.

Board coordinates: origin at the centre of the outline on the bottom face, +x to the
right, +y to the rear (away from the target ports), +z up through the parts.
"""

from collections.abc import Iterator
from dataclasses import dataclass, field
from pathlib import Path

from build123d import BoundBox, Compound, Face, GeomType, Location, Shell, Solid, Vector
from build123d.topology.shape_core import Shape
from OCP.OCP.collections import Sequence_TDF_Label
from OCP.OCP.Interface import Interface_Static
from OCP.OCP.STEPCAFControl import STEPCAFControl_Reader, STEPCAFControl_Writer
from OCP.OCP.STEPControl import STEPControl_AsIs
from OCP.OCP.TCollection import TCollection_AsciiString, TCollection_ExtendedString
from OCP.OCP.TDataStd import TDataStd_Name
from OCP.OCP.TDF import TDF_Label
from OCP.OCP.TDocStd import TDocStd_Document
from OCP.OCP.TopAbs import TopAbs_SHELL, TopAbs_SOLID
from OCP.OCP.TopExp import TopExp_Explorer
from OCP.OCP.TopLoc import TopLoc_Location
from OCP.OCP.XCAFDoc import XCAFDoc_DocumentTool, XCAFDoc_ShapeTool


@dataclass
class Board:
    body: Solid | Compound
    parts: dict[str, Compound]
    step: Path
    origin: Location  # STEP coordinates to board coordinates
    _boxes: dict[tuple[str, bool], BoundBox] = field(default_factory=dict, repr=False)

    @property
    def size(self) -> Vector:
        return self.body.bounding_box().size

    def outline(self) -> Face:
        """The outline as a face on the board's bottom plane, holes left out."""
        bottom = min(self.body.faces().filter_by(GeomType.PLANE), key=lambda f: f.center().Z)
        return Face(bottom.outer_wire())

    def holes(self, diameter: float, tol: float = 0.02) -> list[Vector]:
        """Centres of the through holes of one drill size, on the bottom plane."""
        found: list[Vector] = []
        for face in self.body.faces().filter_by(GeomType.CYLINDER):
            radius, axis = face.radius, face.axis_of_rotation
            assert radius is not None and axis is not None
            if abs(radius * 2 - diameter) <= tol:
                c = axis.position
                c = Vector(c.X, c.Y, 0)
                if all((c - f).length > 0.01 for f in found):
                    found.append(c)
        return sorted(found, key=lambda v: (round(v.Y, 3), v.X))

    def box(self, ref: str, exact: bool = True) -> BoundBox:
        """A part's bounding box. The exact one is slow on spline-heavy models; the rough one
        is never smaller than the part, and a few models overshoot it by about 1.5 mm."""
        if (ref, exact) not in self._boxes:
            self._boxes[ref, exact] = self.parts[ref].bounding_box(optimal=exact)
        return self._boxes[ref, exact]


def load(step: Path) -> Board:
    _, tool = _read(step)
    found: dict[str, list[Shape]] = {}
    body = None
    for component in _children(tool, _first_root(tool)):
        solids = list(_solids(tool, component, TopLoc_Location()))
        # Part instances carry the reference; the PCB body's instance is unnamed and goes by
        # its product name, <board>_PCB.
        if _name(_referred(tool, component)).endswith("_PCB"):
            body = solids[0]
        else:
            found.setdefault(_name(component), []).extend(solids)
    if not isinstance(body, Solid):
        raise TypeError(f"no solid board body in {step}")
    bb = body.bounding_box()
    origin = Location((-bb.center().X, -bb.center().Y, -bb.min.Z))
    parts = {}
    for ref, solids in found.items():
        parts[ref] = Compound([s.moved(origin) for s in solids])
        parts[ref].label = ref
    return Board(body.moved(origin), parts, step, origin)


def export_assembly(
    board: Board,
    extra: list[Shape],
    out: Path,
    attach: list[tuple[Path, Location]] | None = None,
) -> None:
    """Write the board's own STEP with extra shapes (in board coordinates) added to it, and
    other KiCad STEPs placed under it (locations in the board STEP's coordinates).
    Re-reading the exports keeps KiCad's shared part definitions; a STEP rebuilt from the
    loaded solids comes out about eight times larger."""
    doc, tool = _read(board.step)
    root = _first_root(tool)
    for step, location in attach or []:
        known = _roots(tool)
        reader = STEPCAFControl_Reader()
        if reader.ReadFile(str(step)) != 1 or not reader.Transfer(doc):
            raise RuntimeError(f"cannot read {step}")
        for label in _roots(tool):
            if not any(label.IsEqual(k) for k in known):
                tool.AddComponent(root, label, location.wrapped)
    for shape in extra:
        label = tool.AddShape(shape.moved(board.origin.inverse()).wrapped, False)
        TDataStd_Name.Set_s(label, TCollection_ExtendedString(shape.label))
        tool.AddComponent(root, label, TopLoc_Location())
    tool.UpdateAssemblies()
    Interface_Static.SetIVal_s("write.surfacecurve.mode", 0)  # no pcurves: under half the size
    writer = STEPCAFControl_Writer()
    writer.Transfer(doc, STEPControl_AsIs)
    if writer.Write(str(out)) != 1:
        raise RuntimeError(f"cannot write {out}")


def _read(step: Path) -> tuple[TDocStd_Document, XCAFDoc_ShapeTool]:
    doc = TDocStd_Document(TCollection_ExtendedString("XCAF"))
    reader = STEPCAFControl_Reader()
    if reader.ReadFile(str(step)) != 1 or not reader.Transfer(doc):
        raise RuntimeError(f"cannot read {step}")
    return doc, XCAFDoc_DocumentTool.ShapeTool_s(doc.Main())


def _roots(tool: XCAFDoc_ShapeTool) -> list[TDF_Label]:
    roots = Sequence_TDF_Label()
    tool.GetFreeShapes(roots)
    return [roots.Value(i) for i in range(1, roots.Length() + 1)]


def _first_root(tool: XCAFDoc_ShapeTool) -> TDF_Label:
    return _roots(tool)[0]


def _children(tool: XCAFDoc_ShapeTool, label: TDF_Label) -> list[TDF_Label]:
    seq = Sequence_TDF_Label()
    tool.GetComponents_s(label, seq, False)
    return [seq.Value(i) for i in range(1, seq.Length() + 1)]


def _referred(tool: XCAFDoc_ShapeTool, label: TDF_Label) -> TDF_Label:
    product = TDF_Label()
    tool.GetReferredShape_s(label, product)
    return product


def _name(label: TDF_Label) -> str:
    attr = TDataStd_Name()
    if label.FindAttribute(TDataStd_Name.GetID_s(), attr):
        return TCollection_AsciiString(attr.Get()).ToCString()
    return ""


def _solids(tool: XCAFDoc_ShapeTool, label: TDF_Label, loc: TopLoc_Location) -> Iterator[Shape]:
    """Solids under a label, located; models without solids give their shells instead."""
    if tool.IsReference_s(label):
        yield from _solids(tool, _referred(tool, label), loc.Multiplied(tool.GetLocation_s(label)))
    elif tool.IsAssembly_s(label):
        for child in _children(tool, label):
            yield from _solids(tool, child, loc)
    else:
        shape = XCAFDoc_ShapeTool.GetShape_s(label)
        for kind, cls in ((TopAbs_SOLID, Solid), (TopAbs_SHELL, Shell)):
            exp = TopExp_Explorer(shape, kind)
            found = exp.More()
            while exp.More():
                yield cls(exp.Current().Moved(loc))
                exp.Next()
            if found:
                return
