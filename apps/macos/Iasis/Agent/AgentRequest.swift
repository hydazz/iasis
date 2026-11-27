import Foundation

/// A request to the agent: one JSON object with an `op`.
nonisolated enum AgentRequest: Encodable, Equatable {
    case state
    case watch
    /// What a restore would install on each target.
    case plan(hubs: [RestoreSelection])
    /// Erases the targets. The user has confirmed. With `latest`, each gets
    /// the newest release Apple signs, downloaded if the store lacks it.
    case restore(hubs: [RestoreSelection], latest: Bool, after: AfterErase)
    /// Repairs the targets from the images a restore would install and keeps
    /// their data.
    case revive(hubs: [RestoreSelection], latest: Bool)
    /// Signals the targets into DFU. The agent checks every one first.
    case dfu(hubs: [RestoreSelection])
    /// Signals the targets to restart, as `dfu`.
    case reboot(hubs: [RestoreSelection])
    case advanceSetup(hubs: [RestoreSelection], setup: SetupOptions)
    case power(hub: String, port: Int, on: Bool)
    case debugUsb(hub: String, port: Int, on: Bool)
    case rename(hub: String, name: String)
    case brightness(hub: String, percent: Int)
    /// Write IasisOS to a hub: the image the agent carries, or the UF2 file
    /// of `bytes` bytes that follows the request.
    case update(hub: String, bytes: Int64?)
    case sharing(enabled: Bool)
    /// Replace the host's supervision identity with the one in this PKCS #12
    /// archive, in base64.
    case importSupervision(pkcs12: String, password: String)
    /// The host's supervision identity as a PKCS #12 archive.
    case exportSupervision(password: String)
    /// Download a release Apple signs for a model: the newest, or `build`.
    case download(model: String, build: String?)
    case releases(model: String)
    /// Keep the IPSW archive of `bytes` bytes that follows the request.
    case importImage(name: String, bytes: Int64)
    case remove(image: String)
    case catalog
    /// What the agent reads from a hub and finds on its USB.
    case diagnostics(hub: String)

    /// The bytes that follow the request on the connection.
    var payload: Int64 {
        switch self {
        case let .importImage(_, bytes): bytes
        case let .update(_, bytes): bytes ?? 0
        default: 0
        }
    }

    private enum CodingKeys: String, CodingKey {
        case op, hub, hubs, port, name, on, percent, bytes, model, build, enabled, image, latest, pkcs12, password, after, setup
    }

    /// The request as the line the agent reads.
    func line() throws -> Data {
        var line = try JSONEncoder().encode(self)
        line.append(0x0A)
        return line
    }

    func encode(to encoder: any Encoder) throws {
        var fields = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .state:
            try fields.encode("state", forKey: .op)
        case .watch:
            try fields.encode("watch", forKey: .op)
        case let .plan(hubs):
            try fields.encode("plan", forKey: .op)
            try fields.encode(hubs, forKey: .hubs)
        case let .restore(hubs, latest, after):
            try fields.encode(after, forKey: .after)
            try fields.encode("restore", forKey: .op)
            try fields.encode(hubs, forKey: .hubs)
            try fields.encode(latest, forKey: .latest)
        case let .revive(hubs, latest):
            try fields.encode("revive", forKey: .op)
            try fields.encode(hubs, forKey: .hubs)
            try fields.encode(latest, forKey: .latest)
        case let .dfu(hubs):
            try fields.encode("dfu", forKey: .op)
            try fields.encode(hubs, forKey: .hubs)
        case let .reboot(hubs):
            try fields.encode("reboot", forKey: .op)
            try fields.encode(hubs, forKey: .hubs)
        case let .advanceSetup(hubs, setup):
            try fields.encode(setup, forKey: .setup)
            try fields.encode("advance_setup", forKey: .op)
            try fields.encode(hubs, forKey: .hubs)
        case let .power(hub, port, on):
            try fields.encode("power", forKey: .op)
            try fields.encode(hub, forKey: .hub)
            try fields.encode(port, forKey: .port)
            try fields.encode(on, forKey: .on)
        case let .debugUsb(hub, port, on):
            try fields.encode("debug_usb", forKey: .op)
            try fields.encode(hub, forKey: .hub)
            try fields.encode(port, forKey: .port)
            try fields.encode(on, forKey: .on)
        case let .rename(hub, name):
            try fields.encode("rename", forKey: .op)
            try fields.encode(hub, forKey: .hub)
            try fields.encode(name, forKey: .name)
        case let .brightness(hub, percent):
            try fields.encode("brightness", forKey: .op)
            try fields.encode(hub, forKey: .hub)
            try fields.encode(percent, forKey: .percent)
        case let .update(hub, bytes):
            try fields.encode("update", forKey: .op)
            try fields.encode(hub, forKey: .hub)
            try fields.encode(bytes, forKey: .bytes)
        case let .sharing(enabled):
            try fields.encode("sharing", forKey: .op)
            try fields.encode(enabled, forKey: .enabled)
        case let .importSupervision(pkcs12, password):
            try fields.encode("import_supervision", forKey: .op)
            try fields.encode(pkcs12, forKey: .pkcs12)
            try fields.encode(password, forKey: .password)
        case let .exportSupervision(password):
            try fields.encode("export_supervision", forKey: .op)
            try fields.encode(password, forKey: .password)
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
        case let .diagnostics(hub):
            try fields.encode("diagnostics", forKey: .op)
            try fields.encode(hub, forKey: .hub)
        }
    }
}

/// The agent's answer: `"ok"`, `{"error": …}`, `{"state": …}`,
/// `{"catalog": …}`, `{"releases": …}`, `{"plan": …}` or `{"supervision": …}`.
nonisolated enum AgentReply: Decodable, Equatable {
    case ok
    case error(String)
    case state(AgentState)
    case catalog([AgentState.CatalogModel])
    case releases([AgentState.Release])
    case plan([AgentState.Planned])
    case supervision(String)

    private enum CodingKeys: String, CodingKey {
        case error, state, catalog, releases, plan, supervision
    }

    /// One line of the agent's answer.
    init(line: Data) throws {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        self = try decoder.decode(Self.self, from: line)
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
            } else if let identity = try fields.decodeIfPresent(String.self, forKey: .supervision) {
                self = .supervision(identity)
            } else {
                self = try .state(fields.decode(AgentState.self, forKey: .state))
            }
        } else {
            _ = try decoder.singleValueContainer().decode(String.self)
            self = .ok
        }
    }
}

/// The targets selected on one hub, in the session the user saw.
nonisolated struct RestoreSelection: Encodable, Equatable {
    let hub: String
    let session: UInt32
    var targets: [RestoreTarget]
}

/// One attachment, and the device on it when it is identified.
nonisolated struct RestoreTarget: Encodable, Equatable {
    let port: Int
    let generation: UInt32
    var ecid: String?
}

nonisolated struct SetupOptions: Codable, Equatable {
    var language: String
    var locale: String
    var enroll: Bool
}

nonisolated struct AfterErase: Encodable, Equatable {
    var shutdownMacs: Bool
    var setup: SetupOptions?

    private enum CodingKeys: String, CodingKey {
        case shutdownMacs = "shutdown_macs"
        case setup
    }
}
