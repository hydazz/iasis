import SwiftUI

/// What is known about the selected device.
struct DeviceInspector: View {
    let device: Device

    var body: some View {
        let steps = device.run?.steps ?? []
        let hasLiveAction = device.port.request != nil || device.port.job != nil || device.port.awaiting != nil
        let showsActivity = !steps.isEmpty || hasLiveAction || device.port.fault != nil || device.needs != nil
        Form {
            Section {
                if showsActivity {
                    activityRows(steps: steps, hasLiveAction: hasLiveAction)
                } else {
                    portDetails
                }
            } header: {
                DeviceName(device: device, lineLimit: nil)
                    .font(.headline)
                    .foregroundStyle(.primary)
                    .textCase(nil)
            }
            if showsActivity {
                Section { portDetails }
            }
        }
        .formStyle(.grouped)
        .textSelection(.enabled)
    }

    @ViewBuilder
    private func activityRows(steps: [AgentState.OperationStep], hasLiveAction: Bool) -> some View {
        if let fault = device.port.fault {
            Label(fault, systemImage: "exclamationmark.triangle.fill")
        }
        if hasLiveAction, device.port.fault == nil,
           steps.isEmpty || device.port.request != nil && device.run?.inProgress != true
        {
            HStack {
                ActivityMark(activity: device.activity)
                Text(device.activity.text)
            }
            .accessibilityElement(children: .combine)
        }
        ForEach(steps, id: \.kind) { step in
            OperationStepRow(step: step, waitingForImage: device.port.awaiting != nil)
        }
        if let needs = device.needs {
            Label(needs.reason, systemImage: "exclamationmark.triangle")
            if let url = needs.url.flatMap(URL.init(string:)) {
                Link("Get \(needs.name)", destination: url)
            }
        }
    }

    @ViewBuilder private var portDetails: some View {
        LabeledContent("Port", value: "\(device.hubTitle) · \(device.port.number)")
        if let ecid = device.port.ecid {
            LabeledContent("ECID", value: ecid)
        }
        if let link = device.port.link {
            LabeledContent("USB Link", value: link.title)
        }
        if device.port.identification == .waitingForUSB {
            Text("Unlock the device and allow the accessory if prompted. If it still doesn’t appear, reconnect the cable while it’s unlocked.")
                .foregroundStyle(.secondary)
        }
        if let mode = device.port.modeTitle {
            LabeledContent("USB Mode", value: mode)
        }
        if let power = device.port.power {
            LabeledContent(power.title, value: power.reading)
        }
        if device.port.waitingForPower {
            Text("Waiting for Power")
        } else if device.port.short {
            Text("The device reports insufficient power.").foregroundStyle(.secondary)
        }
        if device.port.wrongDfuPort {
            Text(Device.dfuPortHelp).foregroundStyle(.secondary)
        }
        if let problem = device.sharing?.problem {
            Label(problem, systemImage: "exclamationmark.triangle")
        } else if device.sharesInternet {
            Label("Internet Sharing", systemImage: DeviceSymbol.lan)
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
        case .restored: "Restore Mode"
        case .normal: "Normal Mode"
        case nil: nil
        }
    }
}
