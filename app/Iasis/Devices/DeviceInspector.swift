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
                if device.port.occupied {
                    Section("Device") {
                        LabeledContent("Model", value: device.port.device ?? "Not identified")
                        LabeledContent("ECID", value: device.port.ecid ?? "Not identified")
                    }
                }
                Section("Connection") {
                    LabeledContent("Board", value: device.boardTitle)
                    LabeledContent("Port", value: device.port.number.formatted())
                    if device.port.occupied {
                        LabeledContent("Mode", value: device.port.modeTitle)
                        LabeledContent("USB", value: device.port.link.isEmpty ? "No link" : device.port.link)
                        LabeledContent("Power", value: device.port.contract.isEmpty ? "No contract" : device.port.contract)
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
    var modeTitle: String {
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
        case let other?: other
        case nil: "Not on USB"
        }
    }
}
