import SwiftUI

struct PortMenu: View {
    let board: AgentState.Board
    let boardTitle: String
    let port: AgentState.Port
    let model: AgentModel

    var body: some View {
        Menu {
            if let device = port.device, device != port.shortName {
                Text(device)
            }
            if port.mode == "kis_dfu" {
                Text("Use Apple Configurator to restore this Mac.")
            }
            if let result = port.result {
                Text("Last restore: \(result.message)")
            }
            Button("Enter DFU") {
                model.perform(.dfu(board: board.serial, port: port.number))
            }
            .disabled(!port.dfu)
            Button("Restart Device") {
                model.perform(.reboot(board: board.serial, port: port.number))
            }
            .disabled(!port.reboot)
            Button("Erase and Restore…") {
                if let session = board.session, confirmErase() {
                    model.perform(.restore(board: board.serial, session: session, targets: [port.target]))
                }
            }
            .disabled(!port.restore || board.session == nil)
            Divider()
            Button(port.enabled ? "Turn Port Off" : "Turn Port On") {
                model.perform(.power(board: board.serial, port: port.number, on: !port.enabled))
            }
        } label: {
            Label(port.menuTitle, systemImage: port.symbol)
        }
    }

    private func confirmErase() -> Bool {
        // A native menu closes before its action runs, so it cannot host a sheet.
        let alert = NSAlert()
        alert.messageText = "Erase and restore \(port.shortName ?? "the device")?"
        alert.informativeText = "All data on the device connected to port \(port.number) of \(boardTitle) will be erased."
        alert.alertStyle = .warning
        let cancel = alert.addButton(withTitle: "Cancel")
        cancel.keyEquivalent = "\r"
        alert.addButton(withTitle: "Erase and Restore").hasDestructiveAction = true
        NSApp.activate()
        return alert.runModal() == .alertSecondButtonReturn
    }
}
