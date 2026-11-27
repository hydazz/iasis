import Foundation

struct RestorePreferences: Codable, Equatable {
    var shutdownMacs = false
    var advanceAfterErase = false
    var setup = SetupOptions(
        language: Locale.preferredLanguages.first ?? "en",
        locale: Locale.current.identifier(.icu).components(separatedBy: "@")[0],
        enroll: false
    )

    var afterErase: AfterErase {
        AfterErase(shutdownMacs: shutdownMacs, setup: advanceAfterErase ? setup : nil)
    }

    static let file = URL.applicationSupportDirectory.appending(path: "com.hydaz.Iasis/restore-settings.json")

    private enum CodingKeys: String, CodingKey {
        case shutdownMacs = "shutdown_macs"
        case advanceAfterErase = "advance_after_erase"
        case setup
    }

    func save(to url: URL) throws {
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try JSONEncoder().encode(self).write(to: url, options: .atomic)
    }
}
