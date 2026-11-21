import SwiftUI

struct AboutView: View {
    static let windowID = "about"
    @State private var panel = Panel.ports

    private enum Panel: String, CaseIterable {
        case ports = "Port Lights"
        case status = "Status Light"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            HStack(spacing: 12) {
                Image(nsImage: NSApp.applicationIconImage)
                    .resizable()
                    .frame(width: 56, height: 56)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 2) {
                    Text("Iasis").font(.title.bold())
                    if let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String {
                        Text("Version \(version)").foregroundStyle(.secondary)
                    }
                }
            }
            Picker("Indicator", selection: $panel) {
                ForEach(Panel.allCases, id: \.self) { Text($0.rawValue) }
            }
            .pickerStyle(.segmented)
            .labelsHidden()

            Grid(alignment: .leading, horizontalSpacing: 12, verticalSpacing: 9) {
                switch panel {
                case .ports:
                    light(nil, "Off", "Empty or turned off")
                    light(.white, "White", "No power contract. Dim while connecting.")
                    light(.green, "Green", "Device identified or restore complete")
                    light(.blue, "Blue", "Host sees DFU or Recovery")
                    light(.blue, "Pulsing blue", "DFU requested, waiting for the host", .pulse)
                    light(.orange, "Amber", "Waiting for power")
                    light(.orange, "Pulsing amber", "Restart requested", .pulse)
                    light(.purple, "Purple", "Restore outcome unknown. Check the host.")
                    light(.purple, "Pulsing purple", "Fetching an image or restoring", .pulse)
                    light(.red, "Red", "Port fault or restore failed")
                case .status:
                    light(.white, "White", "Board running on its own")
                    light(.green, "Green", "Host connected")
                    light(.blue, "Blue", "Release the button to enter DFU")
                    light(.orange, "Amber", "Release the button to restart")
                    light(.purple, "Purple", "Release the button to request a restore")
                    light(.purple, "Blinking purple", "Hold to confirm erasing or tap to cancel.", .blink)
                    light(.red, "Red", "Board fault. Target ports are off.")
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
        .frame(width: 450, height: 440, alignment: .topLeading)
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
