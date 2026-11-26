import Foundation

/// Finder's habit of opening a window for each Mac, iPad or iPhone that enters
/// DFU or recovery mode. macOS keeps it in an undocumented preference of the
/// user's device agent.
nonisolated enum FinderReveal {
    private static let domain = "com.apple.AMPDeviceDiscoveryAgent"
    private static let key = "reveal-devices"

    static var isEnabled: Bool {
        CFPreferencesCopyAppValue(key as CFString, domain as CFString) as? Bool ?? true
    }

    /// Turning it back on removes the preference, which is how macOS ships.
    static func set(_ enabled: Bool) {
        CFPreferencesSetAppValue(key as CFString, enabled ? nil : kCFBooleanFalse, domain as CFString)
        CFPreferencesAppSynchronize(domain as CFString)
    }
}
