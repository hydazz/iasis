import SwiftUI

struct PortMenu: View {
    let port: DeviceMenuState.Port
    let model: AgentModel

    var body: some View {
        Menu {
            if let detail = port.detail {
                Text(detail)
            }
            if port.configuratorRestore {
                Text("Use Apple Configurator to restore this Mac.")
            }
            action("Enter DFU", request: port.dfu)
            action("Restart", request: port.restart)
            if let revive = port.revive {
                action("Revive", request: revive)
            }
            Button("Restore…") {
                if let restore = port.restore, confirm(restore) {
                    model.perform(restore.request)
                }
            }
            .disabled(port.restore == nil)
            Divider()
            action(port.powerOn ? "Turn Port Off" : "Turn Port On", request: port.power)
        } label: {
            Label(port.title, systemImage: port.symbol)
        }
        .labelStyle(.titleAndIcon)
    }

    private func action(_ title: LocalizedStringKey, request: AgentRequest?) -> some View {
        Button(title) {
            if let request {
                model.perform(request)
            }
        }
        .disabled(request == nil)
    }

    private func confirm(_ restore: RestoreConfirmation) -> Bool {
        // A native menu closes before its action runs, so it cannot host a sheet.
        let alert = NSAlert()
        alert.messageText = restore.title
        alert.informativeText = restore.message
        alert.alertStyle = .warning
        let cancel = alert.addButton(withTitle: "Cancel")
        cancel.keyEquivalent = "\r"
        alert.addButton(withTitle: "Restore").hasDestructiveAction = true
        NSApp.activate()
        return alert.runModal() == .alertSecondButtonReturn
    }
}
