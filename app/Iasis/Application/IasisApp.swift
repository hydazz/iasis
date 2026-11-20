import SwiftUI

@main
struct IasisApp: App {
    @State private var model: AgentModel

    init() {
        let model = AgentModel(agent: AgentClient())
        model.start()
        _model = State(initialValue: model)
    }

    var body: some Scene {
        MenuBarExtra {
            MenuView(model: model)
        } label: {
            MenuBarLabel(model: model)
        }
        .menuBarExtraStyle(.window)

        Settings {
            SettingsView(model: model)
        }
    }
}
