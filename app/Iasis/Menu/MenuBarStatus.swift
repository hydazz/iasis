import AppKit

enum MenuBarStatus: String, CaseIterable {
    case offline = "Iasis, no board connected"
    case ready = "Iasis, ready"
    case busy = "Iasis, busy"
    case fault = "Iasis, needs attention"

    init(_ state: AgentState?) {
        guard let state, !state.boards.isEmpty else {
            self = .offline
            return
        }
        let ports = state.boards.lazy.flatMap(\.ports)
        if state.boards.contains(where: { $0.notice != nil }) || ports.contains(where: { $0.light.hue == "red" }) {
            self = .fault
        } else if state.images.fetch != nil || ports.contains(where: \.light.pulse) {
            self = .busy
        } else {
            self = .ready
        }
    }

    var image: NSImage {
        Self.images[self]!
    }

    private static let images = Dictionary(uniqueKeysWithValues: allCases.map { ($0, $0.makeImage()) })

    private func makeImage() -> NSImage {
        let lead = NSImage(resource: .menuBarLead)
        let image = NSImage(size: lead.size, flipped: true) { bounds in
            guard let context = NSGraphicsContext.current?.cgContext else { return false }
            lead.draw(
                in: bounds,
                from: .zero,
                operation: .sourceOver,
                fraction: self == .offline ? 0.42 : 1,
                respectFlipped: true,
                hints: nil
            )
            guard self == .busy || self == .fault else { return true }
            let badge = CGRect(x: 10.5, y: 6.5, width: 8.5, height: 8.5)
            context.setBlendMode(.clear)
            context.fillEllipse(in: badge.insetBy(dx: -1, dy: -1))
            context.setBlendMode(.normal)
            if self == .busy {
                context.setLineWidth(1.5)
                context.setLineCap(.round)
                context.setStrokeColor(NSColor.black.cgColor)
                context.addArc(
                    center: CGPoint(x: badge.midX, y: badge.midY),
                    radius: 3.5,
                    startAngle: 0,
                    endAngle: 1.5 * .pi,
                    clockwise: false
                )
                context.strokePath()
            } else {
                context.setBlendMode(.sourceAtop)
                context.setFillColor(NSColor.labelColor.cgColor)
                context.fill(bounds)
                context.setBlendMode(.normal)
                context.setFillColor(NSColor.systemRed.cgColor)
                context.fillEllipse(in: badge)
                context.setFillColor(NSColor.white.cgColor)
                context.fill(CGRect(x: badge.midX - 0.75, y: 8, width: 1.5, height: 3.5))
                context.fillEllipse(in: CGRect(x: badge.midX - 0.75, y: 12.25, width: 1.5, height: 1.5))
            }
            return true
        }
        image.isTemplate = self != .fault
        image.accessibilityDescription = rawValue
        return image
    }
}
