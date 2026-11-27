import SwiftUI

/// One port on one line: its number in the colour of its light, the device,
/// and at the trailing edge one mark for what the port is doing.
struct DeviceRowView: View {
    let device: Device

    var body: some View {
        let activity = device.activity
        HStack(spacing: 8) {
            PortBadge(number: device.port.number, light: device.port.light)
            DeviceName(device: device)
            ConnectionMarks(device: device)
            if device.port.wrongDfuPort {
                Text("PORT")
                    .font(.caption2.weight(.medium))
                    .foregroundStyle(.secondary)
                    .help(Device.dfuPortHelp)
                    .accessibilityLabel(Device.dfuPortHelp)
            }
            // A mode is the device's own state. An operation is the mark's to show.
            if activity.mark == .none, activity != .idle {
                Text(activity.text)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Spacer(minLength: 4)
            ActivityMark(activity: activity)
        }
        .accessibilityElement(children: .combine)
    }
}

/// The port's number, drawn as its light on the hub: same colour, and
/// pulsing while the light pulses.
struct PortBadge: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let number: Int
    let light: AgentState.Light

    var body: some View {
        Image(systemName: light.hue == .off ? "\(number).circle" : "\(number).circle.fill")
            .imageScale(.large)
            .foregroundStyle(light.style)
            .symbolEffect(.pulse, options: .repeating, isActive: light.pulse && !reduceMotion)
            .accessibilityLabel("Port \(number)")
    }
}

struct DeviceName: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let device: Device

    var lineLimit: Int? = 1

    var body: some View {
        HStack(spacing: 6) {
            Group {
                if device.port.identifying {
                    Image(systemName: ActivityMark.turning)
                        .symbolEffect(.rotate, options: .repeat(.continuous), isActive: !reduceMotion)
                } else if let symbol = device.symbol {
                    Image(systemName: symbol)
                } else {
                    Color.clear.frame(height: 1)
                }
            }
            .frame(width: 20)
            .foregroundStyle(.secondary)
            .accessibilityHidden(true)
            Text(device.name)
                .italic(device.port.identifying)
                .foregroundStyle(device.port.identifying || !device.port.occupied ? .secondary : .primary)
                .lineLimit(lineLimit)
                .help(device.name)
        }
    }
}

private struct ConnectionMarks: View {
    let device: Device

    var body: some View {
        HStack(spacing: 6) {
            if device.port.powered {
                let limited = device.port.short
                let power = if let reading = device.port.power, reading.source == .pd {
                    "\(reading.title) · \(reading.reading)"
                } else {
                    "USB Power"
                }
                let detail = limited ? "\(power). The device reports insufficient power." : power
                Image(systemName: limited ? "bolt.trianglebadge.exclamationmark.fill" : "bolt.fill")
                    .foregroundStyle(limited ? AnyShapeStyle(.orange) : AnyShapeStyle(.secondary))
                    .help(detail)
                    .accessibilityLabel(detail)
            }
            if let link = device.port.link {
                Image(systemName: "cable.connector")
                    .symbolRenderingMode(.palette)
                    .foregroundStyle(Color.secondary, link == .usb3 ? Color(nsColor: .controlAccentColor) : Color.secondary)
                    .help(link.title)
                    .accessibilityLabel(link.title)
            }
            if let problem = device.sharing?.problem {
                Image(systemName: "exclamationmark.triangle")
                    .foregroundStyle(.orange)
                    .help(problem)
                    .accessibilityLabel("Internet sharing: \(problem)")
            } else if device.sharesInternet {
                Image(systemName: DeviceSymbol.lan)
                    .help("Sharing internet over USB")
                    .accessibilityLabel("Sharing internet over USB")
            }
        }
        .foregroundStyle(.secondary)
    }
}

/// What a port is doing or how it ended, as one circled symbol: an open
/// circle while work runs, a filled one once it ends. Work with a known
/// fraction is its operation's symbol in a ring drawn that far round; without
/// one, arrows turn.
struct ActivityMark: View {
    static let turning = "arrow.trianglehead.2.clockwise.rotate.90"

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let activity: Activity

    var body: some View {
        Group {
            switch activity.mark {
            case .none:
                EmptyView()
            case .pending:
                Image(systemName: "circle").foregroundStyle(.tertiary)
            case .skipped:
                Image(systemName: "minus.circle").foregroundStyle(.secondary)
            case let .working(fraction?):
                Image(systemName: activity.symbol, variableValue: fraction)
                    .symbolVariableValueMode(.draw)
            case .working(nil):
                Image(systemName: "\(Self.turning).circle")
                    .symbolEffect(.rotate, options: .repeat(.continuous), isActive: !reduceMotion)
            case .done:
                Image(systemName: "checkmark.circle.fill").foregroundStyle(.green)
            case .failed:
                Image(systemName: "xmark.circle.fill").foregroundStyle(.red)
            case .uncertain:
                Image(systemName: "questionmark.circle.fill").foregroundStyle(.purple)
            case .caution:
                Image(systemName: "exclamationmark.circle.fill").foregroundStyle(.yellow)
            }
        }
        .imageScale(.large)
        .help(activity.text)
        .accessibilityLabel(activity.text)
    }
}

extension AgentState.Light {
    var style: AnyShapeStyle {
        switch hue {
        case .red: AnyShapeStyle(.red)
        case .green: AnyShapeStyle(.green)
        case .blue: AnyShapeStyle(.blue)
        case .amber: AnyShapeStyle(.orange)
        case .purple: AnyShapeStyle(.purple)
        case .white: AnyShapeStyle(.secondary)
        case .off: AnyShapeStyle(.tertiary)
        }
    }
}
