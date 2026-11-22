import SwiftUI

/// What is known about the selected device.
struct DeviceInspector: View {
    let devices: [Device]

    var body: some View {
        if devices.count == 1, let device = devices.first {
            Form {
                Section {
                    LabeledContent("Port", value: "\(device.boardTitle) · \(device.port.number)")
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
                    DeviceName(device: device, lineLimit: nil)
                        .font(.headline)
                        .foregroundStyle(.primary)
                        .textCase(nil)
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
                if let fault = device.port.fault {
                    Label(fault, systemImage: "exclamationmark.triangle")
                } else if device.port.request != nil || device.port.stage != nil || (device.port.job != nil && device.port.result == nil) {
                    LabeledContent("Operation", value: device.activity.text)
                }
                if let result = device.port.result {
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
}

private extension AgentState.Port {
    var modeTitle: String? {
        switch mode {
        case "dfu": "DFU"
        case "kis_dfu": "DFU over Debug USB"
        case "recovery": "Recovery"
        case "kis": "Debug USB"
        case "port_dfu": "Port DFU"
        case "restored": "Restore Service"
        case "normal": "Started Up"
        case "mux": "System"
        case "wtf": "WTF"
        default: mode
        }
    }
}
