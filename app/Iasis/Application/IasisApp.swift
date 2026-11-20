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
            let status = MenuBarStatus(model.state)
            Image(nsImage: status.image)
                .accessibilityLabel(status.rawValue)
        }
        .menuBarExtraStyle(.menu)

        Settings {
            SettingsView(model: model)
        }
    }
}
