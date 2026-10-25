"""Fit a filled SVG into the back-silkscreen logo slot on a board copy."""

import pcbnew as pcb
from wx.svg import (
    SVG_FILLRULE_EVENODD,
    SVG_FLAGS_VISIBLE,
    SVG_PAINT_COLOR,
    SVG_PAINT_NONE,
    SVGimage,
)


def filled_paths(paths, evenodd):
    """Combine contours using the SVG fill rule, including nested holes."""
    if evenodd:
        result = pcb.SHAPE_POLY_SET()
        for path in paths:
            result.BooleanXor(pcb.SHAPE_POLY_SET(path))
        return result

    def add(regions, winding, area):
        if winding and area.OutlineCount():
            regions.setdefault(winding, pcb.SHAPE_POLY_SET()).BooleanAdd(area)

    regions = {}
    for path in paths:
        sign = 1 if path.Area(False) > 0 else -1
        contour = pcb.SHAPE_POLY_SET(path)
        uncovered = contour.CloneDropTriangulation()
        updated = {}

        for winding, region in regions.items():
            outside, overlap = pcb.SHAPE_POLY_SET(), pcb.SHAPE_POLY_SET()
            outside.BooleanSubtract(region, contour)
            overlap.BooleanIntersection(region, contour)
            add(updated, winding, outside)
            add(updated, winding + sign, overlap)
            uncovered.BooleanSubtract(region)
        add(updated, sign, uncovered)
        regions = updated
    result = pcb.SHAPE_POLY_SET()
    for region in regions.values():
        result.BooleanAdd(region)
    return result


def load_logo(filename, x0, y0, slot):
    svg = SVGimage.CreateFromFile(str(filename))
    shapes = [shape for shape in svg.shapes if shape.flags & SVG_FLAGS_VISIBLE and shape.opacity]
    if not shapes:
        raise ValueError("logo must contain filled SVG paths")
    for shape in shapes:
        if shape.fill.type != SVG_PAINT_COLOR or shape.stroke.type != SVG_PAINT_NONE:
            raise ValueError("logo must use solid fills; convert strokes to paths")
    bounds = [shape.bounds for shape in shapes]
    left, top = min(b[0] for b in bounds), min(b[1] for b in bounds)
    right, bottom = max(b[2] for b in bounds), max(b[3] for b in bounds)
    if right <= left or bottom <= top:
        raise ValueError("logo must have positive width and height")
    scale = min(slot[0] / (right - left), slot[1] / (bottom - top))
    cx, cy = (left + right) / 2, (top + bottom) / 2

    def point(xy):
        x, y = xy
        # Mirror SVG artwork so it reads normally from the back of the board.
        return pcb.VECTOR2I(pcb.FromMM(x0 - (x - cx) * scale), pcb.FromMM(y0 + (y - cy) * scale))

    result = pcb.SHAPE_POLY_SET()
    for shape in shapes:
        paths = []
        for path in shape.paths:
            points = list(path.points)
            chain = pcb.SHAPE_LINE_CHAIN()
            for i in range(0, len(points) - 1, 3):
                curve = pcb.PCB_SHAPE(None, pcb.SHAPE_T_BEZIER)
                curve.SetStart(point(points[i]))
                curve.SetBezierC1(point(points[i + 1]))
                curve.SetBezierC2(point(points[i + 2]))
                curve.SetEnd(point(points[i + 3]))
                curve.RebuildBezierToSegmentsPointsList(pcb.FromMM(0.01))
                for vertex in curve.GetBezierPoints():
                    chain.Append(vertex)
            chain.SetClosed(True)
            chain.Simplify()
            paths.append(chain)
        result.BooleanAdd(filled_paths(paths, shape.fillRule == SVG_FILLRULE_EVENODD))
    if not result.OutlineCount():
        raise ValueError("logo contains no filled area")
    return result


def add_logo(board, logo, slot, centre):
    edge = board.GetBoardEdgesBoundingBox()
    polygon = load_logo(
        logo, pcb.ToMM(edge.GetRight()) - centre[0], pcb.ToMM(edge.GetTop()) + centre[1], slot
    )
    polygon.Fracture()
    group = pcb.PCB_GROUP(board)
    group.SetName("Logo")
    board.Add(group)
    # A PCB graphic serializes one outline, so keep disconnected islands separate.
    for index in range(polygon.OutlineCount()):
        shape = pcb.PCB_SHAPE(board)
        shape.SetShape(pcb.SHAPE_T_POLY)
        shape.SetPolyShape(pcb.SHAPE_POLY_SET(polygon.Outline(index)))
        shape.SetFilled(True)
        shape.SetWidth(0)
        shape.SetLayer(pcb.B_SilkS)
        shape.SetLocked(True)
        if not edge.Contains(shape.GetBoundingBox()):
            raise ValueError("logo extends beyond the board")
        board.Add(shape)
        group.AddItem(shape)
