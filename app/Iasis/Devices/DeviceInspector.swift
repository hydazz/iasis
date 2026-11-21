import SwiftUI

struct DeviceInspector: View {
    let selection: DeviceSelection
    let state: AgentState?

    var body: some View {
        if let row = selection.single, let state {
            ScrollView {
                VStack(alignment: .leading, spacing: 14) {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(row.name).font(.headline)
                        Text("\(state.title(for: row.board)) · Port \(row.port.number)")
                            .foregroundStyle(.secondary)
                    }
                    Divider()
                    field("Status", row.status)
                    if let ecid = row.port.ecid {
                        field("ECID", ecid)
                    }
                    if let seen = row.port.lastSeen {
                        Divider()
                        Text("Last Seen").font(.headline)
                        if let name = seen.device {
                            field("Model", name)
                        }
                        field("ECID", seen.ecid)
                    }
                    if let result = row.port.result {
                        Divider()
                        Text("Last \(result.operation.title) on Port \(row.port.number)").font(.headline)
                        Text(result.message)
                        if let ecid = result.ecid {
                            field("ECID", ecid)
                        }
                        if let directory = result.logDirectory {
                            Button("Show Logs in Finder") {
                                NSWorkspace.shared.open(URL(filePath: directory, directoryHint: .isDirectory))
                            }
                        }
                    }
                    if row.port.mode != nil || !row.port.link.isEmpty || !row.port.contract.isEmpty {
                        Divider()
                        DisclosureGroup("Connection") {
                            VStack(alignment: .leading, spacing: 8) {
                                if let mode = row.port.usbModeTitle {
                                    field("USB mode", mode)
                                }
                                if !row.port.link.isEmpty {
                                    field("USB", row.port.link)
                                }
                                if !row.port.contract.isEmpty {
                                    field("Power contract", row.port.contract)
                                }
                            }
                            .padding(.vertical, 6)
                        }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding()
                .textSelection(.enabled)
            }
        } else if selection.rows.isEmpty {
            ContentUnavailableView("Select a Device", systemImage: "info.circle")
        } else {
            ContentUnavailableView("\(selection.rows.count) Devices Selected", systemImage: "rectangle.stack", description: Text(selection.restore == nil ? "Restore requires eligible devices on the same board." : "These devices can be restored together."))
        }
    }

    private func field(_ label: LocalizedStringKey, _ value: String) -> some View {
        LabeledContent {
            Text(value)
                .frame(maxWidth: .infinity, alignment: .trailing)
                .multilineTextAlignment(.trailing)
        } label: {
            Text(label).foregroundStyle(.secondary)
        }
    }
}

private extension AgentState.Port {
    var usbModeTitle: String? {
        switch mode {
        case "dfu": "DFU"
        case "kis_dfu": "DFU (Debug USB)"
        case "recovery": "Recovery"
        case "kis": "Debug USB"
        case "port_dfu": "Port DFU"
        case "restored": "Restore service"
        case "normal": "Booted"
        case "mux": "System"
        case "wtf": "WTF"
        default: mode
        }
    }
}
