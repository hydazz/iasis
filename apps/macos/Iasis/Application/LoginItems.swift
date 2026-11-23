import OSLog
import ServiceManagement

/// The agent the app carries in `Contents/Helpers`, which launchd runs while
/// the user is logged in. A development build carries none and talks to
/// whichever agent runs.
enum BundledAgent {
    private static let plist = "com.hydaz.Iasis.agent.plist"
    private static let service = SMAppService.agent(plistName: plist)

    static let isBundled = FileManager.default.fileExists(
        atPath: Bundle.main.bundleURL.appending(path: "Contents/Library/LaunchAgents/\(plist)").path(percentEncoded: false)
    )

    /// The user turned the agent off in Login Items.
    static var needsApproval: Bool {
        isBundled && service.status == .requiresApproval
    }

    /// Have launchd run the agent now and at each login, unless the user
    /// turned it off.
    static func register() {
        switch service.status {
        case .notRegistered, .notFound:
            do {
                try service.register()
            } catch {
                Log.agent.error("Agent not registered: \(error.localizedDescription, privacy: .public)")
            }
        default:
            break
        }
    }

    /// Replace the running agent with the one this app carries.
    static func restart() async {
        do {
            try await service.unregister()
        } catch {
            Log.agent.error("Agent not unregistered: \(error.localizedDescription, privacy: .public)")
        }
        register()
    }
}

/// The app among the user's login items. It adds itself on its first launch.
enum OpenAtLogin {
    private static let addedKey = "AddedToLoginItems"

    static var isEnabled: Bool {
        SMAppService.mainApp.status == .enabled
    }

    static func set(_ enabled: Bool) {
        do {
            if enabled {
                try SMAppService.mainApp.register()
            } else {
                try SMAppService.mainApp.unregister()
            }
        } catch {
            Log.agent.error("Login item not changed: \(error.localizedDescription, privacy: .public)")
        }
    }

    /// Open at login from the first launch on, until the user turns it off.
    static func addOnFirstLaunch() {
        guard !UserDefaults.standard.bool(forKey: addedKey) else { return }
        UserDefaults.standard.set(true, forKey: addedKey)
        set(true)
    }
}
