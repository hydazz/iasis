import Foundation

/// What the agent reports: its boards, its restore images and whether it can
/// run restores. The shapes follow the agent's socket, decoded from snake case.
nonisolated struct AgentState: Decodable, Equatable {
    var boards: [Board]
    var images: ImageStore
    /// Why the agent cannot run restores, when it cannot.
    var restoreProblem: String?

    func title(for board: Board) -> String {
        let matches = boards.filter { $0.displayName.caseInsensitiveCompare(board.displayName) == .orderedSame }
        guard matches.count > 1 else { return board.displayName }
        let suffix = board.serial.suffix(4)
        let unique = !matches.contains { $0.serial != board.serial && $0.serial.hasSuffix(suffix) }
        return "\(board.displayName) · \(unique ? String(suffix) : board.serial)"
    }

    struct Board: Decodable, Equatable, Identifiable {
        var serial: String
        var name: String
        var session: UInt32?
        var brightness: Int
        var settingsBusy: Bool
        var ports: [Port]
        /// Why the board cannot be read, or why its last restore request came
        /// to nothing.
        var notice: String?

        var id: String {
            serial
        }

        var displayName: String {
            name.isEmpty ? "Iasis" : name
        }

        var visiblePorts: [Port] {
            ports.filter { $0.occupied || !$0.enabled }
        }
    }

    struct Port: Decodable, Equatable, Identifiable {
        var number: Int
        var occupied: Bool
        var generation: UInt32
        var device: String?
        var mode: String?
        var ecid: String?
        var link: String
        var contract: String
        var state: String
        var light: Light
        var dfu: Bool
        var reboot: Bool
        var restore: Bool
        var revive: Bool
        var enabled: Bool
        var lastSeen: LastSeen?
        var stage: Stage?
        var result: Ending?

        var id: Int {
            number
        }
    }

    struct LastSeen: Decodable, Equatable {
        var device: String?
        var ecid: String
    }

    struct Stage: Decodable, Equatable {
        var name: String
        var percent: UInt8?
    }

    struct Light: Decodable, Equatable {
        var hue: String
        var pulse: Bool
    }

    enum RestorePhase: String, Decodable {
        case preparing = "Preparing"
        case running = "Running"
        case reported = "Reported"
        case unconfirmed = "Unconfirmed"
        case failed = "Failed"
    }

    enum Operation: String, Decodable {
        case restore, revive

        var title: String {
            switch self {
            case .restore: "Restore"
            case .revive: "Revive"
            }
        }
    }

    /// The last worker result on this physical port.
    struct Ending: Decodable, Equatable {
        var operation: Operation
        var logDirectory: String?
        var ecid: String?
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

extension AgentState.Port {
    var target: RestoreTarget {
        RestoreTarget(port: number, generation: generation, ecid: ecid)
    }

    var shortName: String? {
        device?.components(separatedBy: " (").first
    }

    var inDFU: Bool {
        mode == "dfu" || mode == "kis_dfu"
    }

    var menuTitle: String {
        let status: String? = switch state {
        case "ready" where inDFU, "charging" where inDFU: "DFU"
        case "ready" where mode == "recovery": "Recovery"
        case "ready" where shortName != nil: nil
        default: state.prefix(1).uppercased() + state.dropFirst()
        }
        let label = shortName.map { "Port \(number) · \($0)" } ?? "Port \(number)"
        return status.map { "\(label) · \($0)" } ?? label
    }

    var symbol: String {
        let name = (device ?? "").lowercased()
        if name.hasPrefix("ipad") {
            return "ipad"
        }
        if name.hasPrefix("iphone") {
            return "iphone"
        }
        if name.hasPrefix("macbook") {
            return "laptopcomputer"
        }
        if name.hasPrefix("mac mini") {
            return "macmini"
        }
        if name.hasPrefix("mac studio") {
            return "macstudio"
        }
        if name.hasPrefix("mac pro") {
            return "macpro.gen3"
        }
        if name.hasPrefix("imac") {
            return "desktopcomputer"
        }
        if name.hasPrefix("mac") {
            return "macwindow"
        }
        if name.hasPrefix("homepod mini") {
            return "homepodmini"
        }
        if name.hasPrefix("homepod") || name.hasPrefix("audioaccessory") {
            return "homepod"
        }
        if name.hasPrefix("apple tv") || name.hasPrefix("appletv") {
            return "appletv"
        }
        return "cable.connector"
    }
}

nonisolated struct RestoreTarget: Encodable, Equatable {
    let port: Int
    let generation: UInt32
    var ecid: String?
}
