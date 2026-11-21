import SwiftUI

struct MenuView: View {
    let model: AgentModel
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        if let state = model.menu {
            if state.boards.isEmpty {
                Text("No Boards Connected")
            }
            ForEach(state.boards) { board in
                Section(board.title) {
                    if let notice = board.notice {
                        Text(notice)
                    }
                    if board.ports.isEmpty, board.notice == nil {
                        Text("No Devices Connected")
                    }
                    ForEach(board.ports) { port in
                        PortMenu(port: port, model: model)
                    }
                }
            }
            if let version = state.backgroundDownload {
                Text("Downloading \(version)")
            }
            if let problem = state.restoreProblem {
                Text(problem)
            }
        } else {
            Text("Agent Not Running")
        }
        Divider()
        Button("Devices") {
            NSApp.activate()
            openWindow(id: DevicesView.windowID)
        }
        .keyboardShortcut("1")
        Button("About Iasis") {
            NSApp.activate()
            openWindow(id: AboutView.windowID)
        }
        SettingsLink()
            .keyboardShortcut(",")
        Button("Quit Iasis") { NSApp.terminate(nil) }
            .keyboardShortcut("q")
    }
}
