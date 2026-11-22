import Foundation

/// The shapes follow the agent's socket, decoded from snake case.
nonisolated struct AgentState: Decodable, Equatable {
    var boards: [Board]
    var images: ImageStore
    var sharing: Sharing
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
        var firmware: String
        var upstreamMbps: Int?
        /// The power supply's pool for the targets, when there is a supply.
        var pool: Pool?
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
    }

    struct Port: Decodable, Equatable, Identifiable {
        var number: Int
        var occupied: Bool
        var generation: UInt32
        var device: String?
        var mode: String?
        var ecid: String?
        var link: USBLink?
        var power: PortPower?
        /// The target took the largest offer its board could make and would
        /// use more.
        var short: Bool
        /// Why the port faulted, in the board's words.
        var fault: String?
        var identifying: Bool
        var request: SignalProgress?
        var waitingForPower: Bool
        var powered: Bool
        var wrongDfuPort: Bool
        var needsConfigurator: Bool
        var job: Job?
        var light: Light
        var dfu: Bool
        var reboot: Bool
        var restore: Bool
        var revive: Bool
        var enabled: Bool
        var stage: Stage?
        var result: Ending?

        var id: Int {
            number
        }
    }

    struct PortPower: Decodable, Equatable {
        enum Source: String, Decodable {
            case pd, usb
        }

        var source: Source
        var millivolts: Int
        var milliamps: Int
    }

    struct Pool: Decodable, Equatable {
        var budgetMw: Int
        var allocatedMw: Int
    }

    struct Sharing: Decodable, Equatable {
        var enabled: Bool
        var problem: String?
        var ports: [Port]

        struct Port: Decodable, Equatable {
            var board: String
            var port: Int
            var active: Bool
            var problem: String?
        }
    }

    enum USBLink: String, Decodable {
        case usb, usb2, usb3

        var title: String {
            switch self {
            case .usb: "USB"
            case .usb2: "USB 2"
            case .usb3: "USB 3"
            }
        }
    }

    enum SignalProgress: String, Decodable {
        case enteringDfu = "entering_dfu"
        case dfuUnconfirmed = "dfu_unconfirmed"
        case restarting
    }

    struct Job: Decodable, Equatable {
        var operation: Operation
        var phase: RestorePhase
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
