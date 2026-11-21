import Foundation

/// A request to the agent: one JSON object with an `op`.
nonisolated enum AgentRequest: Encodable, Equatable {
    case watch
    /// Erases the targets. The user has confirmed.
    case restore(boards: [RestoreSelection])
    case revive(board: String, session: UInt32, port: Int, generation: UInt32, ecid: String)
    case dfu(board: String, port: Int)
    case reboot(board: String, port: Int)
    case power(board: String, port: Int, on: Bool)
    case rename(board: String, name: String)
    case brightness(board: String, percent: Int)
    case keep(model: String)
    case forget(model: String)
    case importImage(path: String)
    case remove(build: String)
    case refresh

    private enum CodingKeys: String, CodingKey {
        case op, board, boards, port, session, generation, ecid, name, on, percent, model, path, build
    }

    func encode(to encoder: any Encoder) throws {
        var fields = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .watch:
            try fields.encode("watch", forKey: .op)
        case let .restore(boards):
            try fields.encode("restore", forKey: .op)
            try fields.encode(boards, forKey: .boards)
        case let .revive(board, session, port, generation, ecid):
            try fields.encode("revive", forKey: .op)
            try fields.encode(board, forKey: .board)
            try fields.encode(session, forKey: .session)
            try fields.encode(port, forKey: .port)
            try fields.encode(generation, forKey: .generation)
            try fields.encode(ecid, forKey: .ecid)
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
        case let .keep(model):
            try fields.encode("keep", forKey: .op)
            try fields.encode(model, forKey: .model)
        case let .forget(model):
            try fields.encode("forget", forKey: .op)
            try fields.encode(model, forKey: .model)
        case let .importImage(path):
            try fields.encode("import", forKey: .op)
            try fields.encode(path, forKey: .path)
        case let .remove(build):
            try fields.encode("remove", forKey: .op)
            try fields.encode(build, forKey: .build)
        case .refresh:
            try fields.encode("refresh", forKey: .op)
        }
    }
}

/// The agent's answer: `"ok"`, `{"error": …}` or `{"state": …}`.
nonisolated enum AgentReply: Decodable, Equatable {
    case ok
    case error(String)
    case state(AgentState)

    private enum CodingKeys: String, CodingKey {
        case error, state
    }

    init(from decoder: any Decoder) throws {
        if let fields = try? decoder.container(keyedBy: CodingKeys.self) {
            if let message = try fields.decodeIfPresent(String.self, forKey: .error) {
                self = .error(message)
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
