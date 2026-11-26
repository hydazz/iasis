import SwiftUI

@main
struct IasisApp: App {
    @NSApplicationDelegateAdaptor private var delegate: AppDelegate
    @Environment(\.openWindow) private var openWindow
    @Environment(\.openSettings) private var openSettings
    @State private var browser: DeviceBrowser
    private var model: AgentModel {
        browser.model
    }

    init() {
        let model = AgentModel(agent: AgentClient(), restartAgent: BundledAgent.isBundled ? { await BundledAgent.restart() } : nil)
        model.start()
        _browser = State(initialValue: DeviceBrowser(model: model))
    }

    var body: some Scene {
        let _ = delegate.installStatusMenu(browser: browser, openWindow: openWindow, openSettings: openSettings)

        Window("Iasis", id: DevicesView.windowID) {
            DevicesView(browser: browser)
        }
        .defaultSize(width: 720, height: 520)
        .defaultLaunchBehavior(.suppressed)
        .restorationBehavior(.disabled)
        .commands {
            FileCommands(model: model)
            InspectorCommands()
            DeviceCommands()
            HubCommands(model: model)
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
