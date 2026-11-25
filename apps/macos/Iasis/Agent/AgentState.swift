import Foundation

/// The shapes follow the agent's socket in snake case, field for field.
/// `crates/api/fixtures` holds the agent's example of every message, which
/// these models read and write back unchanged.
nonisolated struct AgentState: Codable, Equatable {
    /// The agent's release version.
    var version: String
    var boards: [Board]
    /// A submitting connection's results in target order, each null until it
    /// has one. Absent on other connections.
    var submitted: [Ending?]?
    var images: ImageStore
    var sharing: Sharing
    var supervision: Supervision

    /// How many ports wait for the image being downloaded.
    var waitingForDownload: Int {
        guard let id = images.fetch?.id else { return 0 }
        return boards.flatMap(\.ports).filter { $0.awaiting == id }.count
    }

    func title(for board: Board) -> String {
        let matches = boards.filter { $0.displayName.caseInsensitiveCompare(board.displayName) == .orderedSame }
        guard matches.count > 1 else { return board.displayName }
        let suffix = board.serial.suffix(4)
        let unique = !matches.contains { $0.serial != board.serial && $0.serial.hasSuffix(suffix) }
        return "\(board.displayName) · \(unique ? String(suffix) : board.serial)"
    }

    struct Board: Codable, Equatable, Identifiable {
        var serial: String
        var name: String
        var session: UInt32?
        /// The IasisOS version the board runs; empty while it cannot be read.
        var firmware: String
        /// The IasisOS the agent carries, when the board runs another.
        var update: FirmwareUpdate?
        var upstreamMbps: Int?
        var supply: Supply
        /// The power supply's pool for the targets, when there is a supply.
        var pool: Pool?
        /// A host holds the board.
        var host: Bool
        /// The host takes restore requests for the board.
        var restores: Bool
        var brightness: Int
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

    /// IasisOS the agent can write to a board.
    struct FirmwareUpdate: Codable, Equatable {
        var version: String
        /// The board runs a build the agent cannot talk to, so it has no
        /// ports until it takes this one.
        var required: Bool
    }

    /// What powers the targets.
    enum Supply: String, Codable {
        /// A USB-C power supply on the board's power input.
        case brick
        /// The host's USB port alone.
        case host
        case off
    }

    struct Port: Codable, Equatable, Identifiable {
        var number: Int
        var generation: UInt32
        var occupied: Bool
        var device: String?
        var identifying: Bool
        var ecid: String?
        /// What the board's host finds on the port's USB.
        var usb: USB?
        /// The Apple target's USB mode, when `usb` is `apple`.
        var mode: AppleMode?
        var light: Light
        var request: SignalProgress?
        var waitingForPower: Bool
        var powered: Bool
        var wrongDfuPort: Bool
        var link: USBLink?
        var power: PortPower?
        /// Power supply capacity committed to this port.
        var allocatedMw: Int
        /// The target took the largest offer its board could make and would
        /// use more.
        var short: Bool
        /// Why the port faulted, in the board's words.
        var fault: String?
        var dfu: Bool
        var reboot: Bool
        var restore: Bool
        var enabled: Bool
        var job: Job?
        /// The store's id for the image the port's restore waits for, while
        /// it waits.
        var awaiting: String?
        var stage: Stage?
        var result: Ending?

        var id: Int {
            number
        }
    }

    enum USB: String, Codable {
        /// Nothing enumerates.
        case absent
        /// A device that is not an Apple target.
        case other
        /// Several devices, so no single target can be named.
        case ambiguous
        case apple
    }

    enum AppleMode: String, Codable {
        case dfu
        case portDfu = "port_dfu"
        case recovery
        case kis
        case kisDfu = "kis_dfu"
        case restored
        case normal
    }

    struct PortPower: Codable, Equatable {
        enum Source: String, Codable {
            case pd, usb
        }

        var source: Source
        var millivolts: Int
        var milliamps: Int
    }

    struct Pool: Codable, Equatable {
        var budgetMw: Int
        var allocatedMw: Int
    }

    struct Sharing: Codable, Equatable {
        var enabled: Bool
        var problem: String?
        var ports: [Port]

        struct Port: Codable, Equatable {
            var board: String
            var port: Int
            var active: Bool
            var problem: String?
        }
    }

    /// The identity the host pairs with as supervisor, so supervised devices
    /// that list its certificate don't ask for Trust.
    struct Supervision: Codable, Equatable {
        /// The certificate's common name.
        var name: String?
        /// The certificate in base64 DER.
        var certificate: String?
        var problem: String?
    }

    enum USBLink: String, Codable {
        case usb, usb2, usb3

        var title: String {
            switch self {
            case .usb: "USB"
            case .usb2: "USB 2"
            case .usb3: "USB 3"
            }
        }
    }

    enum SignalProgress: String, Codable {
        case enteringDfu = "entering_dfu"
        case dfuUnconfirmed = "dfu_unconfirmed"
        case restarting
    }

    struct Job: Codable, Equatable {
        var operation: Operation
        var phase: RestorePhase
    }

    /// What a job's worker is doing: its step among the job's steps, and
    /// how far along the whole job is.
    struct Stage: Codable, Equatable {
        var name: String
        var percent: UInt8
        var steps: [String]
        /// The step under way. The steps before it are done.
        var step: Int
    }

    /// The port's light on the board.
    struct Light: Codable, Equatable {
        var hue: Hue
        var pulse: Bool
        var dim: Bool
    }

    enum Hue: String, Codable {
        case off, red, green, blue, amber, purple, white
    }

    enum RestorePhase: String, Codable {
        case preparing = "Preparing"
        case running = "Running"
        case reported = "Reported"
        case unconfirmed = "Unconfirmed"
        case failed = "Failed"
    }

    enum Operation: String, Codable {
        case restore, revive

        var title: String {
            switch self {
            case .restore: "Restore"
            case .revive: "Revive"
            }
        }

        var running: String {
            switch self {
            case .restore: "Restoring"
            case .revive: "Reviving"
            }
        }
    }

    /// How an operation ended.
    struct Ending: Codable, Equatable {
        var operation: Operation
        /// The target's ECID, when it was known.
        var ecid: String?
        var phase: RestorePhase
        var message: String
        var logDirectory: String?
    }

    /// The store's archives: those it holds and those it is still to fetch.
    struct ImageStore: Codable, Equatable {
        var images: [Image]
        var fetch: Fetch?

        /// The image being downloaded.
        var fetching: Image? {
            images.first { $0.id == fetch?.id }
        }
    }

    /// One IPSW archive, in the store or wanted there.
    struct Image: Codable, Equatable, Identifiable {
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
        /// The archive's file, once it is in the store.
        var path: String?
        /// Fetched from Apple's catalog, as opposed to added from a file.
        var fetched: Bool
        /// A restore is reading it, so the agent will not remove it.
        var inUse: Bool

        var title: String {
            os.isEmpty ? version : "\(os) \(version)"
        }
    }

    /// What a restore would install on one target.
    struct Planned: Codable, Equatable {
        var board: String
        var port: Int
        /// Absent while the agent does not know the target's model.
        var offer: Offer?
    }

    /// What a restore of one device can install.
    struct Offer: Codable, Equatable {
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

    /// A system release, as an image in the store or as Apple signs it.
    struct Release: Codable, Equatable, Identifiable {
        /// The system, as Apple names it.
        var os: String
        var version: String
        var build: String
        var bytes: Int64

        var id: String {
            build
        }

        var title: String {
            os.isEmpty ? version : "\(os) \(version)"
        }
    }

    /// A device model the catalog lists.
    struct CatalogModel: Codable, Equatable, Identifiable {
        var identifier: String
        var name: String

        var id: String {
            identifier
        }
    }

    /// A download under way.
    struct Fetch: Codable, Equatable {
        /// The image it becomes.
        var id: String
        var version: String
        var build: String
        var bytes: Int64
        var done: Int64

        var fraction: Double {
            bytes > 0 ? Double(done) / Double(bytes) : 0
        }

        /// Whole percent, rounded down as the agent rounds a port's.
        var percent: Int64 {
            done * 100 / max(bytes, 1)
        }
    }
}
