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

        Window("Devices", id: DevicesView.windowID) {
            DevicesView(model: model)
        }
        .defaultSize(width: 720, height: 420)
        .defaultLaunchBehavior(.suppressed)
        .restorationBehavior(.disabled)

        Window("About Iasis", id: AboutView.windowID) {
            AboutView()
        }
        .windowResizability(.contentSize)
        .defaultLaunchBehavior(.suppressed)
        .restorationBehavior(.disabled)

        Settings {
            SettingsView(model: model)
        }
    }
}
