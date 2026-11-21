import AppKit
import SwiftUI

/// Iasis lives in the menu bar. While one of its windows is open it is also
/// an ordinary app, with a Dock icon and a menu bar of its own.
final class AppDelegate: NSObject, NSApplicationDelegate {
    /// Started at login, which shows no window.
    static let launchedInBackground = CommandLine.arguments.contains("--background")

    /// Opens the app's windows; the menu bar item supplies it.
    var openWindow: OpenWindowAction?
    private var observers: [any NSObjectProtocol] = []

    func applicationDidFinishLaunching(_: Notification) {
        for name in [NSWindow.didBecomeKeyNotification, NSWindow.willCloseNotification] {
            observers.append(NotificationCenter.default.addObserver(forName: name, object: nil, queue: .main) { _ in
                // A closing window is still on screen while it says so.
                Task { @MainActor in Self.followWindows() }
            })
        }
        // The window shown at launch opens before the app is active.
        Task { Self.followWindows() }
    }

    func applicationShouldTerminateAfterLastWindowClosed(_: NSApplication) -> Bool {
        false
    }

    /// Opening the app again, from Finder or the Dock, shows its window.
    func applicationShouldHandleReopen(_: NSApplication, hasVisibleWindows _: Bool) -> Bool {
        if !Self.hasOpenWindow {
            openWindow?(id: DevicesView.windowID)
            NSApp.activate()
        }
        return false
    }

    private static var hasOpenWindow: Bool {
        NSApp.windows.contains { $0.isVisible && $0.styleMask.contains(.titled) }
    }

    private static func followWindows() {
        let open = hasOpenWindow
        let policy: NSApplication.ActivationPolicy = open ? .regular : .accessory
        guard NSApp.activationPolicy() != policy else { return }
        NSApp.setActivationPolicy(policy)
        if open {
            NSApp.activate()
        } else {
            // Leave the front to whichever app had it before.
            NSApp.deactivate()
        }
    }
}
