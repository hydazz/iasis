import SwiftUI

struct MenuView: View {
    let model: AgentModel
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        if let state = model.state {
            if state.boards.isEmpty {
                Text("No Boards Connected")
            }
            ForEach(state.boards) { board in
                Section(state.title(for: board)) {
                    if let notice = board.notice {
                        Text(notice)
                    }
                    if board.visiblePorts.isEmpty, board.notice == nil {
                        Text("No Devices Connected")
                    }
                    ForEach(board.visiblePorts) { port in
                        PortMenu(board: board, boardTitle: state.title(for: board), port: port, model: model)
                    }
                }
            }
            if let fetch = state.images.fetch {
                Text("Downloading \(fetch.version) · \(fetch.fraction.formatted(.percent.precision(.fractionLength(0))))")
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
