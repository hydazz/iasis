import SwiftUI

/// What the hub's lights mean, port by port and for the status light.
struct IndicatorLightsView: View {
    static let windowID = "lights"
    @State private var panel = Panel.ports

    private enum Panel: String, CaseIterable {
        case ports = "Port Lights"
        case status = "Status Light"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Picker("Indicator", selection: $panel) {
                ForEach(Panel.allCases, id: \.self) { Text($0.rawValue) }
            }
            .pickerStyle(.segmented)
            .labelsHidden()

            Grid(alignment: .leading, horizontalSpacing: 12, verticalSpacing: 9) {
                switch panel {
                case .ports:
                    light(nil, "Off", "Empty, turned off or identifying")
                    light(.white, "White", "Device Iasis can't act on")
                    light(.green, "Green", "Target Iasis can act on, or restore complete")
                    light(.blue, "Blue", "Host sees DFU or Recovery")
                    light(.blue, "Pulsing Blue", "DFU requested, waiting for the host", .pulse)
                    light(.orange, "Amber", "Waiting for power")
                    light(.orange, "Pulsing Amber", "Restart requested", .pulse)
                    light(.purple, "Purple", "Restore outcome unknown. Check the host.")
                    light(.purple, "Pulsing Purple", "Fetching an image or restoring", .pulse)
                    light(.red, "Red", "Port fault or restore failed")
                case .status:
                    light(.white, "White", "Hub running on its own")
                    light(.green, "Green", "Host connected")
                    light(.blue, "Blue", "Release the button to enter DFU")
                    light(.orange, "Amber", "Release the button to restart")
                    light(.purple, "Purple", "Release the button to request a restore")
                    light(.purple, "Blinking Purple", "Hold to confirm erasing or tap to cancel.", .blink)
                    light(.red, "Red", "Shared power protection or I²C fault. The target ports are off. This can flash briefly when the charging supply is unplugged.")
                }
            }
            if panel == .ports {
                Text("Unselected ports dim. Faults stay bright.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
            Spacer(minLength: 0)
        }
        .padding(22)
        .frame(width: 450, height: 380, alignment: .topLeading)
    }

    private func light(_ color: Color?, _ label: LocalizedStringKey, _ meaning: LocalizedStringKey, _ pattern: LightSample.Pattern = .steady) -> some View {
        GridRow {
            LightSample(color: color, pattern: pattern)
                .accessibilityHidden(true)
            Text(label)
            Text(meaning)
                .foregroundStyle(.secondary)
        }
        .accessibilityElement(children: .combine)
    }
}

private struct LightSample: View {
    enum Pattern { case steady, pulse, blink }

    let color: Color?
    let pattern: Pattern
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        Group {
            if reduceMotion || pattern == .steady {
                lens
            } else if pattern == .pulse {
                lens.phaseAnimator([false, true]) { content, bright in
                    content.opacity(bright ? 1 : 0.3)
                } animation: { _ in .linear(duration: 0.6) }
            } else {
                TimelineView(.periodic(from: .now, by: 0.4)) { context in
                    let phase = context.date.timeIntervalSinceReferenceDate.truncatingRemainder(dividingBy: 0.8)
                    lens.opacity(phase < 0.4 ? 1 : 0.3)
                }
            }
        }
        .frame(width: 13, height: 13)
    }

    private var lens: some View {
        Circle()
            .fill((color ?? .clear).gradient)
            .overlay {
                Circle().strokeBorder(.primary.opacity(0.20), lineWidth: 0.75)
            }
            .shadow(color: (color ?? .clear).opacity(0.35), radius: 2)
    }
}
