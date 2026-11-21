import SwiftUI

@main
struct IasisApp: App {
    @NSApplicationDelegateAdaptor private var delegate: AppDelegate
    @State private var model: AgentModel

    init() {
        let model = AgentModel(agent: AgentClient())
        model.start()
        _model = State(initialValue: model)
    }

    var body: some Scene {
        MenuBarExtra {
            StatusMenu(model: model)
        } label: {
            MenuBarLabel(status: model.menu.status, delegate: delegate)
        }
        .menuBarExtraStyle(.menu)

        Window("Iasis", id: DevicesView.windowID) {
            DevicesView(model: model)
        }
        .defaultSize(width: 540, height: 440)
        .defaultLaunchBehavior(AppDelegate.launchedInBackground ? .suppressed : .presented)
        .restorationBehavior(.disabled)
        .commands {
            FileCommands(model: model)
            InspectorCommands()
            DeviceCommands()
            HelpCommands()
        }

        Window("Indicator Lights", id: IndicatorLightsView.windowID) {
            IndicatorLightsView()
        }
        .windowResizability(.contentSize)
        .defaultLaunchBehavior(.suppressed)
        .restorationBehavior(.disabled)

        Settings {
            SettingsView(model: model)
        }
    }
}

private struct MenuBarLabel: View {
    let status: MenuBarStatus
    let delegate: AppDelegate
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        Image(nsImage: status.image)
            .accessibilityLabel(status.rawValue)
            .onAppear { delegate.openWindow = openWindow }
    }
}
