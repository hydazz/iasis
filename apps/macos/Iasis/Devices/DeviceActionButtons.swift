import SwiftUI

/// The actions in the Device menu and a row's context menu.
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
    }
}

enum DeviceSymbol {
    static let dfu = "memorychip"
    static let restart = "restart"
    static let revive = "cross.case"
    static let restore = "arrow.counterclockwise"
    static let lan = "rectangle.connected.to.line.below"
    static let download = "arrow.down.circle"

    /// The symbol for images of a system, by the name the agent gives it.
    static func system(_ name: String) -> String {
        switch name {
        case "macOS": "laptopcomputer"
        case "iPadOS": "ipad"
        case "iOS": "iphone"
        case "tvOS": "appletv"
        case "watchOS": "applewatch"
        case "visionOS": "vision.pro"
        case "HomePod Software": "homepod"
        default: "doc.zipper"
        }
    }

    /// The symbol for a device of this name.
    static func kind(of name: String) -> String {
        let name = name.lowercased()
        let kinds = [
            ("ipad", "ipad"),
            ("iphone", "iphone"),
            ("macbook", "laptopcomputer"),
            ("mac mini", "macmini"),
            ("mac studio", "macstudio"),
            ("mac pro", "macpro.gen3"),
            ("imac", "desktopcomputer"),
            ("mac", "desktopcomputer"),
            ("homepod mini", "homepodmini"),
            ("homepod", "homepod"),
            ("audioaccessory", "homepod"),
            ("apple tv", "appletv"),
            ("appletv", "appletv"),
            ("apple watch", "applewatch"),
            ("apple vision", "vision.pro"),
        ]
        return kinds.first { name.hasPrefix($0.0) }?.1 ?? "questionmark"
    }
}

extension AgentState.Operation {
    var symbol: String {
        switch self {
        case .restore: DeviceSymbol.restore
        case .revive: DeviceSymbol.revive
        }
    }
}
