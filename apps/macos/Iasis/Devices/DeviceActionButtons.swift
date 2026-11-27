import SwiftUI

/// The actions in the Device menu and a row's context menu.
struct DeviceActionButtons: View {
    let browser: DeviceBrowser?
    var contextSelection: Set<Device.ID>?

    var body: some View {
        let ids = contextSelection ?? browser?.selection ?? []
        let devices = browser?.devices(in: ids) ?? []
        let actions = DeviceActions(devices)
        let fullMenu = contextSelection == nil

        Section {
            if fullMenu || !actions.dfu.isEmpty {
                Button("Enter DFU", systemImage: DeviceSymbol.dfu) { browser?.model.perform(actions.dfu) }
                    .keyboardShortcut(fullMenu ? KeyboardShortcut("d", modifiers: [.command, .shift]) : nil)
                    .disabled(actions.dfu.isEmpty)
            }
            if fullMenu || !actions.restart.isEmpty {
                Button("Restart", systemImage: DeviceSymbol.restart) { browser?.model.perform(actions.restart) }
                    .keyboardShortcut(fullMenu ? KeyboardShortcut("r", modifiers: [.command, .shift]) : nil)
                    .disabled(actions.restart.isEmpty)
            }
        }
        Section {
            if fullMenu || actions.revive != nil {
                Button("Revive", systemImage: DeviceSymbol.revive) {
                    if let revive = actions.revive {
                        browser?.model.perform([revive])
                    }
                }
                .disabled(actions.revive == nil)
            }
            if fullMenu || actions.restore != nil {
                Button("Restore…", systemImage: DeviceSymbol.restore) {
                    if let confirmation = actions.restore {
                        browser?.confirm(confirmation)
                    }
                }
                .keyboardShortcut(fullMenu ? KeyboardShortcut("r") : nil)
                .disabled(actions.restore == nil)
            }
            if fullMenu || !actions.advanceSetup.isEmpty {
                Button("Advance Setup", systemImage: DeviceSymbol.advanceSetup) {
                    browser?.model.advanceSetup(actions.advanceSetup)
                }
                .disabled(actions.advanceSetup.isEmpty)
            }
        }
        if let browser, contextSelection != nil, devices.count == 1 {
            Divider()
            Button("Get Info", systemImage: "info.circle") {
                browser.selection = ids
                browser.showsInspector = true
            }
        }
    }
}

enum DeviceSymbol {
    static let dfu = "memorychip"
    static let restart = "restart"
    static let revive = "cross.case"
    static let restore = "arrow.counterclockwise"
    static let advanceSetup = "globe"
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
