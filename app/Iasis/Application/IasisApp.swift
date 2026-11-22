import SwiftUI

@main
struct IasisApp: App {
    @NSApplicationDelegateAdaptor private var delegate: AppDelegate
    @State private var browser: DeviceBrowser
    private var model: AgentModel {
        browser.model
    }

    init() {
        let model = AgentModel(agent: AgentClient())
        model.start()
        _browser = State(initialValue: DeviceBrowser(model: model))
    }

    var body: some Scene {
        MenuBarExtra {
            StatusMenu(browser: browser)
        } label: {
            MenuBarLabel(status: MenuBarStatus(model.state), delegate: delegate)
        }
        .menuBarExtraStyle(.menu)

        Window("Iasis", id: DevicesView.windowID) {
            DevicesView(browser: browser)
        }
        .defaultSize(width: 720, height: 414)
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
