"""The board as KiCad exports it: the PCB body and every placed part, by reference.

export_board.py writes a STEP assembly in which each part instance is named after its
reference designator. The enclosure looks connectors up by reference instead of
keeping its own copy of their coordinates.

Board coordinates: origin at the centre of the outline on the bottom face, +x to
the right, +y to the rear (away from the target ports), +z up through the parts.
"""

from collections.abc import Iterator
from dataclasses import dataclass, field
from pathlib import Path

from build123d import BoundBox, Compound, Face, GeomType, Location, Shape, Shell, Solid, Vector
from OCP.collections import Sequence_TDF_Label
from OCP.Interface import Interface_Static
from OCP.STEPCAFControl import STEPCAFControl_Reader, STEPCAFControl_Writer
from OCP.STEPControl import STEPControl_AsIs
from OCP.TCollection import TCollection_AsciiString, TCollection_ExtendedString
from OCP.TDataStd import TDataStd_Name
from OCP.TDF import TDF_Label
from OCP.TDocStd import TDocStd_Document
from OCP.TopAbs import TopAbs_SHELL, TopAbs_SOLID
from OCP.TopExp import TopExp_Explorer
from OCP.TopLoc import TopLoc_Location
from OCP.XCAFDoc import XCAFDoc_ColorGen, XCAFDoc_DocumentTool, XCAFDoc_ShapeTool


@dataclass
class Board:
    body: Solid
    parts: dict[str, Compound]
    step: Path
    origin: Location  # STEP coordinates to board coordinates
    _rough: dict[str, BoundBox] = field(default_factory=dict, repr=False)
    _exact: dict[str, BoundBox] = field(default_factory=dict, repr=False)

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
            if abs(face.radius * 2 - diameter) <= tol:
                c = face.axis_of_rotation.position
                c = Vector(c.X, c.Y, 0)
                if all((c - f).length > 0.01 for f in found):
                    found.append(c)
        return sorted(found, key=lambda v: (round(v.Y, 3), v.X))

    def box(self, ref: str) -> BoundBox:
        """Tight bounding box; slow on spline-heavy models, so computed on demand."""
        if ref not in self._exact:
            self._exact[ref] = self.parts[ref].bounding_box()
        return self._exact[ref]

    def rough(self, ref: str) -> BoundBox:
        """Fast bounding box, never smaller than the part; a few models overshoot by ~1.5 mm."""
        if ref not in self._rough:
            self._rough[ref] = self.parts[ref].bounding_box(optimal=False)
        return self._rough[ref]

    def placed(self) -> dict[str, Compound]:
        """Parts whose centre lies over the outline; the rest are still waiting to be placed."""
        bb = self.body.bounding_box()
        out = {}
        for ref, part in self.parts.items():
            c = self.rough(ref).center()
            if bb.min.X <= c.X <= bb.max.X and bb.min.Y <= c.Y <= bb.max.Y:
                out[ref] = part
        return out


def load(step: Path) -> Board:
    doc = TDocStd_Document(TCollection_ExtendedString("XCAF"))
    reader = STEPCAFControl_Reader()
    reader.SetNameMode(True)
    if reader.ReadFile(str(step)) != 1 or not reader.Transfer(doc):
        raise RuntimeError(f"cannot read {step}")
    tool = XCAFDoc_DocumentTool.ShapeTool_s(doc.Main())

    found: dict[str, list[Shape]] = {}
    body = None
    for component in _children(tool, _first_root(tool)):
        product = _referred(tool, component)
        solids = list(_solids(tool, component, TopLoc_Location()))
        # Part instances carry the reference; the PCB body's instance is unnamed and
        # goes by its product name, <board>_PCB.
        if _name(product).endswith("_PCB"):
            body = solids[0]
        else:
            found.setdefault(_name(component), []).extend(solids)
    if body is None:
        raise RuntimeError(f"no board body in {step}")

    bb = body.bounding_box()
    to_origin = Location((-bb.center().X, -bb.center().Y, -bb.min.Z))
    body = body.moved(to_origin)
    parts = {}
    for ref, solids in found.items():
        part = Compound([s.moved(to_origin) for s in solids])
        part.label = ref
        parts[ref] = part
    return Board(body=body, parts=parts, step=step, origin=to_origin)


def export_assembly(board: Board, extra: list[Shape], out: Path) -> None:
    """Write the board's own STEP with extra shapes (in board coordinates) added to it.

    Re-reading the export keeps KiCad's shared part definitions and model colours; a
    STEP rebuilt from the loaded solids comes out about eight times larger.
    """
    doc = TDocStd_Document(TCollection_ExtendedString("XCAF"))
    reader = STEPCAFControl_Reader()
    reader.SetNameMode(True)
    reader.SetColorMode(True)
    if reader.ReadFile(str(board.step)) != 1 or not reader.Transfer(doc):
        raise RuntimeError(f"cannot read {board.step}")
    tool = XCAFDoc_DocumentTool.ShapeTool_s(doc.Main())
    colors = XCAFDoc_DocumentTool.ColorTool_s(doc.Main())
    root = _first_root(tool)
    back = board.origin.inverse()
    for shape in extra:
        label = tool.AddShape(shape.moved(back).wrapped, False)
        TDataStd_Name.Set_s(label, TCollection_ExtendedString(shape.label))
        if shape.color is not None:
            colors.SetColor(label, shape.color.wrapped, XCAFDoc_ColorGen)
        tool.AddComponent(root, label, TopLoc_Location())
    tool.UpdateAssemblies()
    Interface_Static.SetIVal_s("write.surfacecurve.mode", 0)  # no pcurves: under half the size
    writer = STEPCAFControl_Writer()
    writer.SetNameMode(True)
    writer.SetColorMode(True)
    writer.Transfer(doc, STEPControl_AsIs)
    if writer.Write(str(out)) != 1:
        raise RuntimeError(f"cannot write {out}")


def _first_root(tool: XCAFDoc_ShapeTool) -> TDF_Label:
    roots = Sequence_TDF_Label()
    tool.GetFreeShapes(roots)
    return roots.Value(1)


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
        inner = loc.Multiplied(tool.GetLocation_s(label))
        yield from _solids(tool, _referred(tool, label), inner)
        return
    if tool.IsAssembly_s(label):
        for child in _children(tool, label):
            yield from _solids(tool, child, loc)
        return
    shape = XCAFDoc_ShapeTool.GetShape_s(label)
    for kind, cls in ((TopAbs_SOLID, Solid), (TopAbs_SHELL, Shell)):
        exp = TopExp_Explorer(shape, kind)
        found = False
        while exp.More():
            found = True
            yield cls(exp.Current().Moved(loc))
            exp.Next()
        if found:
            return
