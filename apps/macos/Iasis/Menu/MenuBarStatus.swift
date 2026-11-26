import AppKit

enum MenuBarStatus: String, CaseIterable {
    case offline = "Iasis, no hub connected"
    case ready = "Iasis, ready"
    case busy = "Iasis, busy"
    case fault = "Iasis, needs attention"

    init(_ state: AgentState?) {
        guard let state, !state.hubs.isEmpty else {
            self = .offline
            return
        }
        let ports = state.hubs.lazy.flatMap(\.ports)
        if state.hubs.contains(where: { $0.notice != nil }) || ports.contains(where: { $0.light.hue == .red }) {
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
        let icon = NSImage(resource: .menuBar)
        let badge = badgeSymbol
        let image = NSImage(size: icon.size, flipped: true) { bounds in
            icon.draw(
                in: bounds,
                from: .zero,
                operation: .sourceOver,
                fraction: self == .offline ? 0.42 : 1,
                respectFlipped: true,
                hints: nil
            )
            guard let badge, let context = NSGraphicsContext.current?.cgContext else { return true }
            let corner = CGRect(x: 10.5, y: 6.5, width: 8.5, height: 8.5)
            // A gap sets the badge apart from the icon.
            context.setBlendMode(.clear)
            context.fillEllipse(in: corner.insetBy(dx: -1, dy: -1))
            context.setBlendMode(.normal)
            if self == .fault {
                // A coloured badge makes the image no template, so the icon
                // takes the menu bar's text colour here.
                context.setBlendMode(.sourceIn)
                context.setFillColor(NSColor.labelColor.cgColor)
                context.fill(bounds)
                context.setBlendMode(.normal)
            }
            badge.draw(
                in: corner,
                from: .zero,
                operation: .sourceOver,
                fraction: 1,
                respectFlipped: true,
                hints: nil
            )
            return true
        }
        image.isTemplate = self != .fault
        image.accessibilityDescription = rawValue
        return image
    }

    /// The symbol in the icon's corner: work under way, or something that
    /// needs attention.
    private var badgeSymbol: NSImage? {
        switch self {
        case .offline, .ready:
            nil
        case .busy:
            NSImage(systemSymbolName: ActivityMark.turning, accessibilityDescription: nil)?
                .withSymbolConfiguration(.init(pointSize: 8, weight: .bold))
        case .fault:
            NSImage(systemSymbolName: "exclamationmark.circle.fill", accessibilityDescription: nil)?
                .withSymbolConfiguration(
                    NSImage.SymbolConfiguration(pointSize: 8, weight: .bold)
                        .applying(.init(paletteColors: [.white, .systemRed]))
                )
        }
    }
}
