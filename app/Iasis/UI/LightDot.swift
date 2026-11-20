import SwiftUI

/// A port's light, drawn as the board shows it: the same hue, pulsing while
/// the board's pulses.
struct LightDot: View {
    let light: AgentState.Light

    var body: some View {
        Circle()
            .fill(color)
            .strokeBorder(.tertiary, lineWidth: outlined ? 1 : 0)
            .frame(width: 10, height: 10)
            .opacity(light.dim ? 0.5 : 1)
            .phaseAnimator(light.pulse ? [1.0, 0.25] : [1.0]) { dot, opacity in
                dot.opacity(opacity)
            } animation: { _ in
                .easeInOut(duration: 0.6)
            }
            .accessibilityHidden(true)
    }

    private var color: Color {
        switch light.hue {
        case .off: .clear
        case .red: .red
        case .green: .green
        case .white: .white
        case .blue: .blue
        case .amber: .orange
        case .purple: .purple
        }
    }

    /// An empty port and a white light need an edge to show against the panel.
    private var outlined: Bool {
        light.hue == .off || light.hue == .white
    }
}
