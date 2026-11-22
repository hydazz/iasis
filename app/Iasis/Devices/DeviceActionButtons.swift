import SwiftUI

/// The device actions, in one order and with one symbol each wherever they
/// appear: the toolbar, the Device menu, a row's context menu and the menu
/// bar.
struct DeviceActionButtons: View {
    let actions: DeviceActions
    var shortcuts = false
    let perform: ([AgentRequest]) -> Void
    let restore: (RestoreConfirmation) -> Void

    var body: some View {
        Button("Enter DFU", systemImage: DeviceSymbol.dfu) { perform(actions.dfu) }
            .keyboardShortcut(shortcuts ? KeyboardShortcut("d", modifiers: [.command, .shift]) : nil)
            .disabled(actions.dfu.isEmpty)
        Button("Restart", systemImage: DeviceSymbol.restart) { perform(actions.restart) }
            .keyboardShortcut(shortcuts ? KeyboardShortcut("r", modifiers: [.command, .shift]) : nil)
            .disabled(actions.restart.isEmpty)
        Divider()
        Button("Revive", systemImage: DeviceSymbol.revive) {
            if let revive = actions.revive {
                perform([revive])
            }
        }
        .disabled(actions.revive == nil)
        Button("Restore…", systemImage: DeviceSymbol.restore) {
            if let confirmation = actions.restore {
                restore(confirmation)
            }
        }
        .keyboardShortcut(shortcuts ? KeyboardShortcut("r") : nil)
        .disabled(actions.restore == nil)
        Divider()
        // A title that stays put lets an open menu keep its item.
        Toggle("USB Power", systemImage: DeviceSymbol.power, isOn: Binding {
            actions.powered
        } set: { _ in
            perform(actions.power)
        })
        .disabled(actions.power.isEmpty)
    }
}

enum DeviceSymbol {
    static let dfu = "memorychip"
    static let restart = "restart"
    static let revive = "cross.case"
    static let restore = "arrow.counterclockwise"
    static let power = "power"
    static let lan = "rectangle.connected.to.line.below"
}
