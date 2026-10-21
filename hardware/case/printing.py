"""Print-oriented exports, with optional flush lettering as a second material."""

from pathlib import Path
from xml.etree import ElementTree as ET
from zipfile import ZIP_DEFLATED, ZipFile

from build123d import Color, Compound, Location, Mesher, export_stl
from build123d.topology.shape_core import Shape


def export(
    name: str,
    body: Shape,
    directory: Path,
    markings: Shape | None = None,
    rotation: Location | None = None,
) -> None:
    """Write an engraved STL/3MF and, with markings, a flush multicolour 3MF.

    Body and markings share assembled coordinates and meet without overlap.
    Both receive the same rotation and translation onto the print bed.
    """
    directory.mkdir(parents=True, exist_ok=True)
    rotation = rotation or Location()
    body = rotation * body
    if markings is not None:
        markings = rotation * markings
    bb = Compound([body] + ([markings] if markings is not None else [])).bounding_box()
    on_bed = Location((-bb.center().X, -bb.center().Y, -bb.min.Z))
    body = on_bed * body
    if markings is not None:
        markings = on_bed * markings

    export_stl(body, directory / f"{name}.stl", tolerance=0.01, angular_tolerance=0.1)
    mesher = Mesher()
    mesher.add_shape(Compound([body], label=name), linear_deflection=0.01, angular_deflection=0.1)
    mesher.write(directory / f"{name}.3mf")
    if markings is not None:
        _multicolour(directory / f"{name}-multicolour.3mf", name, body, markings)


def _multicolour(path: Path, name: str, body: Shape, markings: Shape) -> None:
    mesher = Mesher()
    assert mesher.model is not None
    for shape, label, colour in (
        (body, "Body", Color(0.13, 0.13, 0.13)),
        (markings, "Lettering", Color(1, 1, 1)),
    ):
        # A labelled compound keeps disconnected glyphs in one selectable part.
        part = Compound([shape], label=label, color=colour)
        mesher.add_shape(part, linear_deflection=0.01, angular_deflection=0.1)

    # Mesher creates independent build items; replace them with one assembly.
    items = mesher.model.GetBuildItems()
    old_items = []
    while items.MoveNext():
        old_items.append(items.GetCurrent())
    for item in old_items:
        mesher.model.RemoveBuildItem(item)
    assembly = mesher.model.AddComponentsObject()
    assembly.SetName(name)
    identity = mesher.wrapper.GetIdentityTransform()
    for mesh in mesher.meshes:
        assembly.AddComponent(mesh, identity)
    mesher.model.AddBuildItem(assembly, identity)
    mesher.write(path)

    # BambuStudio stores per-part filament slots outside the core 3MF model.
    config = ET.Element("config")
    obj = ET.SubElement(config, "object", id=str(assembly.GetResourceID()))
    ET.SubElement(obj, "metadata", key="name", value=name)
    ET.SubElement(obj, "metadata", key="extruder", value="1")
    for slot, mesh in enumerate(mesher.meshes, 1):
        part = ET.SubElement(obj, "part", id=str(mesh.GetResourceID()), subtype="normal_part")
        ET.SubElement(part, "metadata", key="name", value=mesh.GetName())
        ET.SubElement(part, "metadata", key="extruder", value=str(slot))
    with ZipFile(path) as archive:
        entries = {entry: archive.read(entry) for entry in archive.namelist()}
    entries["Metadata/model_settings.config"] = ET.tostring(
        config, encoding="utf-8", xml_declaration=True
    )
    types = ET.fromstring(entries["[Content_Types].xml"])
    ET.SubElement(
        types,
        "{http://schemas.openxmlformats.org/package/2006/content-types}Default",
        Extension="config",
        ContentType="application/xml",
    )
    entries["[Content_Types].xml"] = ET.tostring(types, encoding="utf-8", xml_declaration=True)
    with ZipFile(path, "w", ZIP_DEFLATED) as archive:
        for entry, data in entries.items():
            archive.writestr(entry, data)
