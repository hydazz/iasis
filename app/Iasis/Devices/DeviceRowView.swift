import SwiftUI

/// One port on one line: its number in the colour of its light, the device,
/// and what the port is doing.
struct DeviceRowView: View {
    let device: Device

    var body: some View {
        let activity = device.activity
        HStack(spacing: 8) {
            PortBadge(number: device.port.number, light: device.port.light)
            Image(systemName: device.symbol ?? "circle")
                .foregroundStyle(.secondary)
                .opacity(device.symbol == nil ? 0 : 1)
                .frame(width: 22)
                .accessibilityHidden(true)
            Text(device.name)
                .foregroundStyle(device.port.occupied ? .primary : .tertiary)
                .lineLimit(1)
            if let problem = device.sharing?.problem {
                Image(systemName: "exclamationmark.triangle")
                    .help(problem)
                    .accessibilityLabel("Internet sharing")
                    .accessibilityValue(Text(problem))
            } else if device.sharesInternet {
                Image(systemName: "globe")
                    .foregroundStyle(.secondary)
                    .help("Sharing internet over USB")
                    .accessibilityLabel("Sharing internet over USB")
            }
            Spacer(minLength: 12)
            Text(activity.text)
                .foregroundStyle(.secondary)
                .monospacedDigit()
                .lineLimit(1)
            // Each mark has its own place, taken or not.
            Color.clear
                .frame(width: 16, height: 16)
                .overlay { PowerMark(device: device) }
            Color.clear
                .frame(width: 16, height: 16)
                .overlay { ActivityMark(mark: activity.mark) }
        }
        .accessibilityElement(children: .combine)
    }
}

/// The port's number, drawn as its light on the board: same colour, and
/// pulsing while the light pulses.
struct PortBadge: View {
    let number: Int
    let light: AgentState.Light

    var body: some View {
        Image(systemName: light.hue == "off" ? "\(number).circle" : "\(number).circle.fill")
            .imageScale(.large)
            .foregroundStyle(light.style)
            .symbolEffect(.pulse, options: .repeating, isActive: light.pulse)
            .accessibilityLabel("Port \(number)")
    }
}

/// The target reported a shortfall on the port’s highest-power offer.
struct PowerMark: View {
    let device: Device

    var body: some View {
        if device.port.short, let watts = device.watts {
            Image(systemName: "bolt.trianglebadge.exclamationmark.fill")
                .symbolRenderingMode(.multicolor)
                .foregroundStyle(.yellow)
                .help("PD contract of \(watts). The device reports insufficient power.")
                .accessibilityLabel("Power contract limited to \(watts)")
        }
    }
}

struct ActivityMark: View {
    let mark: Activity.Mark

    var body: some View {
        switch mark {
        case .none:
            EmptyView()
        case let .working(fraction):
            if let fraction {
                ProgressView(value: fraction)
                    .progressViewStyle(.circular)
                    .controlSize(.small)
            } else {
                ProgressView()
                    .controlSize(.small)
            }
        case .done:
            Image(systemName: "checkmark.circle.fill")
                .foregroundStyle(.green)
                .accessibilityLabel("Complete")
        case .failed:
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundStyle(.red)
                .accessibilityLabel("Failed")
        case .uncertain:
            Image(systemName: "questionmark.circle.fill")
                .foregroundStyle(.purple)
                .accessibilityLabel("Unknown")
        case .caution:
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundStyle(.yellow)
                .accessibilityLabel("Needs attention")
        }
    }
}

extension AgentState.Light {
    var style: AnyShapeStyle {
        switch hue {
        case "red": AnyShapeStyle(.red)
        case "green": AnyShapeStyle(.green)
        case "blue": AnyShapeStyle(.blue)
        case "amber": AnyShapeStyle(.orange)
        case "purple": AnyShapeStyle(.purple)
        case "white": AnyShapeStyle(.secondary)
        default: AnyShapeStyle(.tertiary)
        }
    }
}
