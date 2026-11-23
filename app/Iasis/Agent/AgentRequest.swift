import Foundation

/// A request to the agent: one JSON object with an `op`.
nonisolated enum AgentRequest: Encodable, Equatable {
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
    case rename(board: String, name: String)
    case brightness(board: String, percent: Int)
    case sharing(enabled: Bool)
    /// Download one of the releases Apple signs for a model.
    case download(model: String, build: String)
    case importImage(path: String)
    case remove(image: String)
    case catalog
    case releases(model: String)

    private enum CodingKeys: String, CodingKey {
        case op, board, boards, port, name, on, percent, model, path, build, enabled, image, latest
    }

    func encode(to encoder: any Encoder) throws {
        var fields = encoder.container(keyedBy: CodingKeys.self)
        switch self {
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
        case let .rename(board, name):
            try fields.encode("rename", forKey: .op)
            try fields.encode(board, forKey: .board)
            try fields.encode(name, forKey: .name)
        case let .brightness(board, percent):
            try fields.encode(board, forKey: .board)
            try fields.encode("brightness", forKey: .op)
            try fields.encode(percent, forKey: .percent)
        case let .sharing(enabled):
            try fields.encode("sharing", forKey: .op)
            try fields.encode(enabled, forKey: .enabled)
        case let .download(model, build):
            try fields.encode("download", forKey: .op)
            try fields.encode(model, forKey: .model)
            try fields.encode(build, forKey: .build)
        case let .importImage(path):
            try fields.encode("import", forKey: .op)
            try fields.encode(path, forKey: .path)
        case let .remove(image):
            try fields.encode("remove", forKey: .op)
            try fields.encode(image, forKey: .image)
        case .catalog:
            try fields.encode("catalog", forKey: .op)
        case let .releases(model):
            try fields.encode("releases", forKey: .op)
            try fields.encode(model, forKey: .model)
        }
    }
}

/// The agent's answer: `"ok"`, `{"error": …}`, `{"state": …}`,
/// `{"catalog": …}`, `{"releases": …}` or `{"plan": …}`.
nonisolated enum AgentReply: Decodable, Equatable {
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
}

/// The targets selected on one board, in the session the user saw.
nonisolated struct RestoreSelection: Encodable, Equatable {
    let board: String
    let session: UInt32
    var targets: [RestoreTarget]
}

/// One attachment, and the device on it when it is identified.
nonisolated struct RestoreTarget: Encodable, Equatable {
    let port: Int
    let generation: UInt32
    var ecid: String?
}
