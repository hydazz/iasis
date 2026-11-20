import Foundation

/// What the agent reports: its boards, its restore images and whether it can
/// run restores. The shapes follow the agent's socket, decoded from snake case.
nonisolated struct AgentState: Decodable, Equatable {
    var boards: [Board]
    var images: ImageStore
    /// Why the agent cannot run restores, when it cannot.
    var restoreProblem: String?
    var brightness: Int

    struct Board: Decodable, Equatable, Identifiable {
        var serial: String
        var firmware: String
        var supply: String
        var host: Bool
        var restores: Bool
        var brightness: Int
        var ports: [Port]
        /// Why the board cannot be read, or why its last restore request came
        /// to nothing.
        var notice: String?

        var id: String {
            serial
        }
    }

    struct Port: Decodable, Equatable, Identifiable {
        var number: Int
        var light: Light
        var state: String
        var usb: String
        var link: String
        var contract: String
        var dfu: Bool
        var reboot: Bool
        var restore: Bool
        var enabled: Bool
        var job: RestorePhase?
        var result: Ending?

        var id: Int {
            number
        }
    }

    /// What a port's light shows on the board.
    struct Light: Decodable, Equatable {
        var hue: Hue
        var pulse: Bool
        var dim: Bool
    }

    enum Hue: String, Decodable {
        case off, red, green, white, blue, amber, purple

        /// A hue from a newer agent shows as an unidentified device.
        init(from decoder: any Decoder) throws {
            let name = try decoder.singleValueContainer().decode(String.self)
            self = Hue(rawValue: name) ?? .white
        }
    }

    enum RestorePhase: String, Decodable {
        case preparing = "Preparing"
        case running = "Running"
        case reported = "Reported"
        case unconfirmed = "Unconfirmed"
        case failed = "Failed"
    }

    /// How a port's last restore ended.
    struct Ending: Decodable, Equatable {
        var ecid: String
        var phase: RestorePhase
        var message: String
    }

    struct ImageStore: Decodable, Equatable {
        var images: [Image]
        var models: [Model]
        var fetch: Fetch?
    }

    struct Image: Decodable, Equatable, Identifiable {
        var productVersion: String
        var buildVersion: String
        var bytes: Int64
        var productTypes: [String]
        /// Fetched for a model, as opposed to added from a file.
        var fetched: Bool

        var id: String {
            buildVersion
        }
    }

    /// A model whose newest image the agent keeps.
    struct Model: Decodable, Equatable, Identifiable {
        var identifier: String
        var name: String
        var latest: Release?
        var error: String?
        var ready: Bool

        var id: String {
            identifier
        }
    }

    struct Release: Decodable, Equatable {
        var version: String
        var build: String
        var bytes: Int64
    }

    /// A download under way.
    struct Fetch: Decodable, Equatable {
        var version: String
        var build: String
        var bytes: Int64
        var done: Int64

        var fraction: Double {
            bytes > 0 ? Double(done) / Double(bytes) : 0
        }
    }
}

/// One port of one board.
nonisolated struct PortID: Hashable {
    var board: String
    var port: Int
}
