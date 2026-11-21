import SwiftUI

/// What is known about the selected device.
struct DeviceInspector: View {
    let devices: [Device]

    var body: some View {
        if devices.count == 1, let device = devices.first {
            Form {
                Section {
                    LabeledContent("Status", value: status(of: device))
                    if device.port.mode == "kis_dfu", !device.port.restore {
                        Text("Use Apple Configurator to restore this Mac.")
                            .foregroundStyle(.secondary)
                    }
                } header: {
                    Label(device.name, systemImage: device.symbol ?? "powerplug")
                        .font(.headline)
                        .foregroundStyle(.primary)
                }
                if device.port.occupied, device.port.device != nil || device.port.ecid != nil {
                    Section("Device") {
                        if let model = device.port.device {
                            LabeledContent("Model", value: model)
                        }
                        if let ecid = device.port.ecid {
                            LabeledContent("ECID", value: ecid)
                        }
                    }
                }
                Section("Connection") {
                    LabeledContent("Board", value: device.boardTitle)
                    LabeledContent("Port", value: device.port.number.formatted())
                    if device.port.occupied {
                        if let mode = device.port.modeTitle {
                            LabeledContent("Mode", value: mode)
                        }
                        if !device.port.link.isEmpty {
                            LabeledContent("USB link", value: device.port.link)
                        }
                        if let watts = device.watts {
                            LabeledContent("PD contract", value: "\(device.port.contract) (\(watts))")
                        }
                        if device.port.short {
                            Text("The device reports that the available power does not meet its requirements.")
                                .foregroundStyle(.secondary)
                        }
                    }
                }
                if let result = device.result {
                    Section("Last \(result.operation.title)") {
                        Text(result.message)
                        if let directory = result.logDirectory {
                            Button("Show Logs in Finder") {
                                NSWorkspace.shared.open(URL(filePath: directory, directoryHint: .isDirectory))
                            }
                        }
                    }
                }
            }
            .formStyle(.grouped)
            .textSelection(.enabled)
        } else if devices.isEmpty {
            ContentUnavailableView("No Selection", systemImage: "cable.connector")
        } else {
            ContentUnavailableView("\(devices.count) Devices Selected", systemImage: "rectangle.stack")
        }
    }

    private func status(of device: Device) -> String {
        let text = device.activity.text
        if !text.isEmpty {
            return text
        }
        return device.port.state.prefix(1).uppercased() + device.port.state.dropFirst()
    }
}

private extension AgentState.Port {
    var modeTitle: String? {
        switch mode {
        case "dfu": "DFU"
        case "kis_dfu": "DFU over Debug USB"
        case "recovery": "Recovery"
        case "kis": "Debug USB"
        case "port_dfu": "Port DFU"
        case "restored": "Restore service"
        case "normal": "Started up"
        case "mux": "System"
        case "wtf": "WTF"
        default: mode
        }
    }
}
