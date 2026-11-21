import SwiftUI

/// The menu bar menu: each board's devices, then the app.
struct StatusMenu: View {
    let model: AgentModel
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        let state = model.menu
        if let boards = state.boards {
            if boards.isEmpty {
                Text("No Boards Connected")
            }
            ForEach(boards) { board in
                Section(board.title) {
                    if let notice = board.notice {
                        Text(notice)
                    }
                    if board.devices.isEmpty, board.notice == nil {
                        Text("No Devices")
                    }
                    ForEach(board.devices) { entry in
                        DeviceMenu(entry: entry, model: model)
                    }
                }
            }
            if let download = state.download {
                Divider()
                Text("Downloading Image")
                    .badge(Text(download))
            }
            if let problem = state.restoreProblem {
                Divider()
                Text(problem)
            }
        } else {
            Text("Agent Not Running")
        }
        Divider()
        Button("Open Iasis") {
            openWindow(id: DevicesView.windowID)
            NSApp.activate()
        }
        .keyboardShortcut("o")
        SettingsLink()
            .keyboardShortcut(",")
        Divider()
        Button("Quit Iasis") { NSApp.terminate(nil) }
            .keyboardShortcut("q")
    }
}

/// A device's line in the menu, with its actions in a submenu.
private struct DeviceMenu: View {
    let entry: StatusMenuState.Entry
    let model: AgentModel

    var body: some View {
        let device = entry.device
        Menu {
            DeviceActionButtons(actions: entry.actions) { requests in
                model.perform(requests)
            } restore: { confirmation in
                if confirm(confirmation) {
                    model.perform([confirmation.request])
                }
            }
        } label: {
            Label(entry.title, systemImage: "\(device.port.number).circle")
        }
        .badge(device.activity.text.isEmpty ? nil : Text(device.activity.text))
    }

    /// A menu has closed by the time its action runs, so it cannot hold a sheet.
    private func confirm(_ restore: RestoreConfirmation) -> Bool {
        let alert = NSAlert()
        alert.messageText = restore.title
        alert.informativeText = restore.message
        alert.alertStyle = .warning
        alert.addButton(withTitle: "Cancel")
        alert.addButton(withTitle: "Restore").hasDestructiveAction = true
        NSApp.activate()
        return alert.runModal() == .alertSecondButtonReturn
    }
}
