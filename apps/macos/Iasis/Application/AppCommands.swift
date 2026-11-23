import SwiftUI
import UniformTypeIdentifiers

/// The File menu: add a restore image, above the window's Close.
struct FileCommands: Commands {
    let model: AgentModel

    var body: some Commands {
        CommandGroup(replacing: .newItem) {
            Button("Add Restore Image…") {
                let panel = NSOpenPanel()
                panel.allowedContentTypes = [UTType(filenameExtension: "ipsw") ?? .data]
                panel.prompt = "Add"
                if panel.runModal() == .OK, let file = panel.url {
                    model.importImage(file)
                }
            }
        }
    }
}

/// The Device menu: the main window's actions for its selection.
struct DeviceCommands: Commands {
    @FocusedValue(\.deviceBrowser) private var browser

    var body: some Commands {
        CommandMenu("Device") {
            DeviceMenuItems(browser: browser)
        }
    }
}

private struct DeviceMenuItems: View {
    let browser: DeviceBrowser?

    var body: some View {
        DeviceActionButtons(actions: browser?.actions ?? DeviceActions(), shortcuts: true) { requests in
            browser?.model.perform(requests)
        } restore: { confirmation in
            browser?.confirm(confirmation)
        }
    }
}

/// Help explains the lights, which are the board's own interface.
struct HelpCommands: Commands {
    @Environment(\.openWindow) private var openWindow

    var body: some Commands {
        CommandGroup(replacing: .help) {
            Button("Indicator Lights") {
                openWindow(id: IndicatorLightsView.windowID)
            }
        }
    }
}
