import SwiftUI

struct StatusMenu: View {
    let browser: DeviceBrowser
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        let model = browser.model
        let live = model.state?.devices ?? []
        if let snapshot = model.menuSnapshot {
            if snapshot.boards.isEmpty {
                Text("No Boards Connected")
            }
            ForEach(snapshot.boardDevices) { board in
                Section(board.title) {
                    let ports = board.devices.filter(\.isSelectable)
                    if let notice = board.board.notice {
                        Text(notice)
                    } else if ports.isEmpty {
                        Text("No Devices")
                    }
                    ForEach(ports) { port in
                        let device = live.first { port.sameAttachment(as: $0) }
                        Menu {
                            Button("Get Info", systemImage: "info.circle") {
                                browser.selection = [port.id]
                                browser.showsInspector = true
                                showDevices()
                            }
                            Divider()
                            if let device {
                                DeviceActionButtons(actions: DeviceActions([device], restoreProblem: model.state?.restoreProblem)) {
                                    model.perform($0)
                                } restore: { confirmation in
                                    showDevices()
                                    browser.confirming = confirmation
                                }
                            }
                        } label: {
                            Text("\(port.port.number)  \(port.shortName)")
                            if let status = device?.menuSubtitle {
                                Text(status)
                            }
                        }
                        .disabled(device == nil)
                    }
                }
            }
            if let fetch = model.state?.images.fetch, !live.contains(where: { $0.port.stage != nil && $0.port.job == nil }) {
                Text("Downloading Image").badge(Text(fetch.fraction.formatted(.percent)))
            }
            if let problem = snapshot.restoreProblem {
                Text(problem)
            }
        } else {
            Text("Agent Not Running")
        }
        Divider()
        Button("Open Iasis", action: showDevices).keyboardShortcut("o")
        SettingsLink().keyboardShortcut(",")
        Divider()
        Button("Quit Iasis") { NSApp.terminate(nil) }.keyboardShortcut("q")
    }

    private func showDevices() {
        openWindow(id: DevicesView.windowID)
        NSApp.activate()
    }
}

extension Device {
    func sameAttachment(as live: Device) -> Bool {
        id == live.id && session == live.session && port.generation == live.port.generation &&
            (port.ecid == nil || port.ecid == live.port.ecid) && live.isSelectable
    }

    var menuSubtitle: String? {
        let text = activity.text
        if !text.isEmpty {
            return text
        }
        return port.wrongDfuPort ? "PORT" : nil
    }
}
