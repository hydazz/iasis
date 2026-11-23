import Foundation

/// The shapes follow the agent's socket, decoded from snake case.
nonisolated struct AgentState: Decodable, Equatable {
    var boards: [Board]
    var images: ImageStore
    var sharing: Sharing

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
        var job: Job?
        var light: Light
        var dfu: Bool
        var reboot: Bool
        var restore: Bool
        var enabled: Bool
        /// Percent of the restore image fetched, while the port waits for one.
        var fetch: UInt8?
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

    /// What a job's worker is doing: its step among the job's steps, and
    /// how far along the whole job is.
    struct Stage: Decodable, Equatable {
        var name: String
        var percent: UInt8
        var steps: [String]
        /// The step under way. The steps before it are done.
        var step: Int
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

    /// The store's archives: those it holds and those it is still to fetch.
    struct ImageStore: Decodable, Equatable {
        var images: [Image]
        var fetch: Fetch?

        /// The image being downloaded.
        var fetching: Image? {
            images.first { $0.id == fetch?.id }
        }
    }

    /// One IPSW archive, in the store or wanted there.
    struct Image: Decodable, Equatable, Identifiable {
        /// The store's name for the archive, which requests use.
        var id: String
        /// The system it installs, as Apple names it.
        var os: String
        var version: String
        var build: String
        var bytes: Int64
        /// The models it restores, by name.
        var models: [String]
        /// The archive is in the store.
        var ready: Bool
        var error: String?

        var title: String {
            os.isEmpty ? version : "\(os) \(version)"
        }
    }

    /// What a restore would install on one target.
    struct Planned: Decodable, Equatable {
        var board: String
        var port: Int
        /// Absent while the agent does not know the target's model.
        var offer: Offer?
    }

    /// What a restore of one device can install.
    struct Offer: Decodable, Equatable {
        struct Release: Decodable, Equatable {
            var os: String
            var version: String
            var build: String
            var bytes: Int64

            var title: String {
                os.isEmpty ? version : "\(os) \(version)"
            }
        }

        /// The newest release Apple signs for the device.
        var latest: Release?
        /// The newest image in the store that restores it and that Apple
        /// still signs.
        var held: Release?
        /// Why the catalog could not say what Apple signs.
        var problem: String?

        /// The newest release, when the image in the store is an older one.
        var newer: Release? {
            guard let held, let latest else { return nil }
            let version = held.version.compare(latest.version, options: .numeric)
            let older = version == .orderedSame
                ? held.build.compare(latest.build, options: .numeric) == .orderedAscending
                : version == .orderedAscending
            return older ? latest : nil
        }
    }

    /// A release Apple signs for a model.
    struct Release: Decodable, Equatable, Identifiable {
        var version: String
        var build: String
        var bytes: Int64

        var id: String {
            build
        }
    }

    /// A device model the catalog lists.
    struct CatalogModel: Decodable, Equatable, Identifiable {
        var identifier: String
        var name: String

        var id: String {
            identifier
        }
    }

    /// A download under way.
    struct Fetch: Decodable, Equatable {
        /// The image it becomes.
        var id: String
        var bytes: Int64
        var done: Int64

        var fraction: Double {
            bytes > 0 ? Double(done) / Double(bytes) : 0
        }
    }
}
