import SwiftUI

/// What is known about the selected device.
struct DeviceInspector: View {
    let device: Device

    var body: some View {
        let operation = device.operation
        // The operation or fault, and what the host lacks, lead under the
        // device's name.
        let leads = operation != nil || device.port.fault != nil || device.needs != nil
        Form {
            if leads {
                Section {
                    if let fault = device.port.fault {
                        Label(fault, systemImage: "exclamationmark.triangle.fill")
                    } else if let operation {
                        OperationView(operation: operation)
                    }
                    if let needs = device.needs {
                        Label(needs.reason, systemImage: "exclamationmark.triangle")
                        if let url = needs.url.flatMap(URL.init(string:)) {
                            Link("Get \(needs.name)", destination: url)
                        }
                    }
                } header: {
                    DeviceTitle(device: device)
                }
            }
            Section {
                LabeledContent("Port", value: "\(device.hubTitle) · \(device.port.number)")
                if let ecid = device.port.ecid {
                    LabeledContent("ECID", value: ecid)
                }
                if let link = device.port.link {
                    LabeledContent("USB Link", value: link.title)
                }
                if let mode = device.port.modeTitle {
                    LabeledContent("USB Mode", value: mode)
                }
                if device.port.wrongDfuPort {
                    Text(Device.dfuPortHelp).foregroundStyle(.secondary)
                }
                if let problem = device.sharing?.problem {
                    Label(problem, systemImage: "exclamationmark.triangle")
                } else if device.sharesInternet {
                    Label("Internet Sharing", systemImage: DeviceSymbol.lan)
                }
            } header: {
                if !leads {
                    DeviceTitle(device: device)
                }
            }
            if device.port.powered || device.port.power != nil || device.port.waitingForPower || device.port.short {
                Section("Power") {
                    if let power = device.port.power {
                        LabeledContent(power.title, value: power.reading)
                            .help(power.source == .usb ? "Default USB power available to this device" : "Power negotiated by this device")
                    } else if device.port.powered {
                        Text("USB Power")
                    }
                    if device.port.waitingForPower {
                        Text("Waiting for Power")
                    } else if device.port.short {
                        Text("The device reports insufficient power.").foregroundStyle(.secondary)
                    }
                }
            }
        }
        .formStyle(.grouped)
        .textSelection(.enabled)
    }
}

private struct DeviceTitle: View {
    let device: Device

    var body: some View {
        DeviceName(device: device, lineLimit: nil)
            .font(.headline)
            .foregroundStyle(.primary)
            .textCase(nil)
    }
}

/// An operation from its title down: overall progress, each step and how it
/// stands, then the outcome.
private struct OperationView: View {
    let operation: OperationStatus

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Label(operation.title, systemImage: operation.symbol)
                    .fontWeight(.medium)
                Spacer()
                if case let .working(fraction?) = operation.mark {
                    Text(fraction.formatted(.percent.precision(.fractionLength(0))))
                        .monospacedDigit()
                        .foregroundStyle(.secondary)
                } else {
                    ActivityMark(activity: Activity(operation.title, operation.mark))
                }
            }
            if case let .working(fraction?) = operation.mark {
                ProgressView(value: fraction)
                    .accessibilityLabel(operation.title)
            }
            if !operation.steps.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    ForEach(Array(operation.steps.enumerated()), id: \.offset) { _, step in
                        StepLabel(step: step, detail: step.state == .active ? operation.detail : nil)
                    }
                }
            } else if let detail = operation.detail {
                Text(detail).foregroundStyle(.secondary)
            }
        }
    }
}

private struct StepLabel: View {
    let step: OperationStatus.Step
    /// What the worker is doing within the step, when it is something else.
    let detail: String?

    var body: some View {
        Label {
            VStack(alignment: .leading, spacing: 2) {
                Text(step.name)
                    .foregroundStyle(step.state == .pending ? .secondary : .primary)
                if let detail {
                    Text(detail)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
        } icon: {
            switch step.state {
            case .done:
                Image(systemName: "checkmark.circle.fill")
                    .foregroundStyle(.secondary)
                    .accessibilityLabel("Done")
            case .active:
                Image(systemName: "\(ActivityMark.turning).circle")
                    .symbolEffect(.rotate, options: .repeat(.continuous))
                    .accessibilityLabel("Under Way")
            case .pending:
                Image(systemName: "circle")
                    .foregroundStyle(.tertiary)
                    .accessibilityLabel("Waiting")
            }
        }
    }
}

private extension AgentState.Port {
    var modeTitle: String? {
        switch mode {
        case .dfu: "DFU"
        case .kisDfu: "DFU over Debug USB"
        case .recovery: "Recovery"
        case .kis: "Debug USB"
        case .portDfu: "Port DFU"
        case .restored: "Restore Service"
        case .normal: "Started Up"
        case nil: nil
        }
    }
}
