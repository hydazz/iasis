import Foundation

/// A request to the agent: one JSON object with an `op`.
nonisolated enum AgentRequest: Codable, Equatable {
    case state
    case watch
    /// What a restore would install on each target.
    case plan(boards: [RestoreSelection])
    /// Erases the targets. The user has confirmed. With `latest`, each gets
    /// the newest release Apple signs, downloaded if the store lacks it.
    case restore(boards: [RestoreSelection], latest: Bool)
    /// Repairs the targets from the images a restore would install and keeps
    /// their data.
    case revive(boards: [RestoreSelection], latest: Bool)
    case dfu(board: String, port: Int)
    case reboot(board: String, port: Int)
    case power(board: String, port: Int, on: Bool)
    case debugUsb(board: String, port: Int, on: Bool)
    case rename(board: String, name: String)
    case brightness(board: String, percent: Int)
    /// The remaining current of a legacy USB-A host port; zero clears it.
    case hostBudget(board: String, milliamps: Int)
    /// Write IasisOS to a board: the image the agent carries, or the UF2 file
    /// of `bytes` bytes that follows the request.
    case update(board: String, bytes: Int64?)
    case sharing(enabled: Bool)
    /// Download a release Apple signs for a model: the newest, or `build`.
    case download(model: String, build: String?)
    case releases(model: String)
    /// Keep the IPSW archive of `bytes` bytes that follows the request.
    case importImage(name: String, bytes: Int64)
    case remove(image: String)
    case catalog

    /// The bytes that follow the request on the connection.
    var payload: Int64 {
        switch self {
        case let .importImage(_, bytes): bytes
        case let .update(_, bytes): bytes ?? 0
        default: 0
        }
    }

    private enum CodingKeys: String, CodingKey {
        case op, board, boards, port, name, on, percent, milliamps, bytes, model, build, enabled, image, latest
    }

    init(from decoder: any Decoder) throws {
        let fields = try decoder.container(keyedBy: CodingKeys.self)
        func field<T: Decodable>(_ key: CodingKeys) throws -> T {
            try fields.decode(T.self, forKey: key)
        }
        let op: String = try field(.op)
        switch op {
        case "state": self = .state
        case "watch": self = .watch
        case "plan": self = try .plan(boards: field(.boards))
        case "restore": self = try .restore(boards: field(.boards), latest: field(.latest))
        case "revive": self = try .revive(boards: field(.boards), latest: field(.latest))
        case "dfu": self = try .dfu(board: field(.board), port: field(.port))
        case "reboot": self = try .reboot(board: field(.board), port: field(.port))
        case "power": self = try .power(board: field(.board), port: field(.port), on: field(.on))
        case "debug_usb": self = try .debugUsb(board: field(.board), port: field(.port), on: field(.on))
        case "rename": self = try .rename(board: field(.board), name: field(.name))
        case "brightness": self = try .brightness(board: field(.board), percent: field(.percent))
        case "host_budget": self = try .hostBudget(board: field(.board), milliamps: field(.milliamps))
        case "update": self = try .update(board: field(.board), bytes: fields.decodeIfPresent(Int64.self, forKey: .bytes))
        case "sharing": self = try .sharing(enabled: field(.enabled))
        case "download": self = try .download(model: field(.model), build: fields.decodeIfPresent(String.self, forKey: .build))
        case "releases": self = try .releases(model: field(.model))
        case "import": self = try .importImage(name: field(.name), bytes: field(.bytes))
        case "remove": self = try .remove(image: field(.image))
        case "catalog": self = .catalog
        default:
            throw DecodingError.dataCorruptedError(forKey: .op, in: fields, debugDescription: "unknown op \(op)")
        }
    }

    func encode(to encoder: any Encoder) throws {
        var fields = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .state:
            try fields.encode("state", forKey: .op)
        case .watch:
            try fields.encode("watch", forKey: .op)
        case let .plan(boards):
            try fields.encode("plan", forKey: .op)
            try fields.encode(boards, forKey: .boards)
        case let .restore(boards, latest):
            try fields.encode("restore", forKey: .op)
            try fields.encode(boards, forKey: .boards)
            try fields.encode(latest, forKey: .latest)
        case let .revive(boards, latest):
            try fields.encode("revive", forKey: .op)
            try fields.encode(boards, forKey: .boards)
            try fields.encode(latest, forKey: .latest)
        case let .dfu(board, port):
            try fields.encode("dfu", forKey: .op)
            try fields.encode(board, forKey: .board)
            try fields.encode(port, forKey: .port)
        case let .reboot(board, port):
            try fields.encode("reboot", forKey: .op)
            try fields.encode(board, forKey: .board)
            try fields.encode(port, forKey: .port)
        case let .power(board, port, on):
            try fields.encode("power", forKey: .op)
            try fields.encode(board, forKey: .board)
            try fields.encode(port, forKey: .port)
            try fields.encode(on, forKey: .on)
        case let .debugUsb(board, port, on):
            try fields.encode("debug_usb", forKey: .op)
            try fields.encode(board, forKey: .board)
            try fields.encode(port, forKey: .port)
            try fields.encode(on, forKey: .on)
        case let .rename(board, name):
            try fields.encode("rename", forKey: .op)
            try fields.encode(board, forKey: .board)
            try fields.encode(name, forKey: .name)
        case let .brightness(board, percent):
            try fields.encode("brightness", forKey: .op)
            try fields.encode(board, forKey: .board)
            try fields.encode(percent, forKey: .percent)
        case let .hostBudget(board, milliamps):
            try fields.encode("host_budget", forKey: .op)
            try fields.encode(board, forKey: .board)
            try fields.encode(milliamps, forKey: .milliamps)
        case let .update(board, bytes):
            try fields.encode("update", forKey: .op)
            try fields.encode(board, forKey: .board)
            try fields.encode(bytes, forKey: .bytes)
        case let .sharing(enabled):
            try fields.encode("sharing", forKey: .op)
            try fields.encode(enabled, forKey: .enabled)
        case let .download(model, build):
            try fields.encode("download", forKey: .op)
            try fields.encode(model, forKey: .model)
            try fields.encode(build, forKey: .build)
        case let .releases(model):
            try fields.encode("releases", forKey: .op)
            try fields.encode(model, forKey: .model)
        case let .importImage(name, bytes):
            try fields.encode("import", forKey: .op)
            try fields.encode(name, forKey: .name)
            try fields.encode(bytes, forKey: .bytes)
        case let .remove(image):
            try fields.encode("remove", forKey: .op)
            try fields.encode(image, forKey: .image)
        case .catalog:
            try fields.encode("catalog", forKey: .op)
        }
    }
}

/// The agent's answer: `"ok"`, `{"error": …}`, `{"state": …}`,
/// `{"catalog": …}`, `{"releases": …}` or `{"plan": …}`.
nonisolated enum AgentReply: Codable, Equatable {
    case ok
    case error(String)
    case state(AgentState)
    case catalog([AgentState.CatalogModel])
    case releases([AgentState.Release])
    case plan([AgentState.Planned])

    private enum CodingKeys: String, CodingKey {
        case error, state, catalog, releases, plan
    }

    init(from decoder: any Decoder) throws {
        if let fields = try? decoder.container(keyedBy: CodingKeys.self) {
            if let message = try fields.decodeIfPresent(String.self, forKey: .error) {
                self = .error(message)
            } else if let models = try fields.decodeIfPresent([AgentState.CatalogModel].self, forKey: .catalog) {
                self = .catalog(models)
            } else if let releases = try fields.decodeIfPresent([AgentState.Release].self, forKey: .releases) {
                self = .releases(releases)
            } else if let planned = try fields.decodeIfPresent([AgentState.Planned].self, forKey: .plan) {
                self = .plan(planned)
            } else {
                self = try .state(fields.decode(AgentState.self, forKey: .state))
            }
        } else {
            _ = try decoder.singleValueContainer().decode(String.self)
            self = .ok
        }
    }

    func encode(to encoder: any Encoder) throws {
        if case .ok = self {
            var value = encoder.singleValueContainer()
            try value.encode("ok")
            return
        }
        var fields = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .ok: break
        case let .error(message): try fields.encode(message, forKey: .error)
        case let .state(state): try fields.encode(state, forKey: .state)
        case let .catalog(models): try fields.encode(models, forKey: .catalog)
        case let .releases(releases): try fields.encode(releases, forKey: .releases)
        case let .plan(planned): try fields.encode(planned, forKey: .plan)
        }
    }
}

/// The targets selected on one board, in the session the user saw.
nonisolated struct RestoreSelection: Codable, Equatable {
    let board: String
    let session: UInt32
    var targets: [RestoreTarget]
}

/// One attachment, and the device on it when it is identified.
nonisolated struct RestoreTarget: Codable, Equatable {
    let port: Int
    let generation: UInt32
    var ecid: String?
}
