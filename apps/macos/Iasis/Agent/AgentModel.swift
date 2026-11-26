import AppKit
import Observation
import OSLog

/// The agent's state for the views, and the requests they make of it.
@Observable
final class AgentModel {
    /// The agent's latest state, or nil while no agent answers.
    private(set) var state: AgentState?
    /// An archive on its way into the agent's store.
    private(set) var importing: Importing?
    /// Hubs being written IasisOS, by serial.
    private(set) var updating: Set<String> = []
    private let agent: any AgentService
    /// Replaces the running agent with the one the app carries; nil when it
    /// carries none.
    private let restartAgent: (() async -> Void)?
    private var restartedAgent = false
    private var watching: Task<Void, Never>?

    static let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? ""

    init(agent: any AgentService, restartAgent: (() async -> Void)? = nil) {
        self.agent = agent
        self.restartAgent = restartAgent
    }

    /// Follow the agent for as long as the app runs, across its restarts.
    func start() {
        guard watching == nil else {
            return
        }
        watching = Task {
            while !Task.isCancelled {
                do {
                    for try await state in agent.states() {
                        if state.version != Self.version, await replaceAgent(running: state.version) {
                            break
                        }
                        self.state = state
                    }
                } catch is DecodingError {
                    // A rebuild keeps the version, so a state this app cannot
                    // read is the other sign of another build's agent.
                    if await !replaceAgent(running: "of another build") {
                        Log.agent.error("Cannot read the agent's state")
                    }
                } catch {
                    Log.agent.debug("Agent unavailable: \(error.localizedDescription)")
                }
                state = nil
                try? await Task.sleep(for: .seconds(2))
            }
        }
    }

    /// An app put in place of another finds that app's agent still running.
    /// Restart it once: a difference a restart cannot fix would otherwise
    /// repeat it forever.
    private func replaceAgent(running version: String) async -> Bool {
        guard let restartAgent, !restartedAgent else {
            return false
        }
        restartedAgent = true
        Log.agent.notice("Restarting agent \(version, privacy: .public) for app \(Self.version, privacy: .public)")
        await restartAgent()
        return true
    }

    func perform(_ requests: [AgentRequest]) {
        Task {
            for request in requests {
                guard await send(request) else { return }
            }
        }
    }

    /// Write the IasisOS the agent carries to a hub.
    func updateFirmware(of hub: String) {
        guard updating.insert(hub).inserted else { return }
        Task {
            await send(.update(hub: hub, bytes: nil))
            updating.remove(hub)
        }
    }

    /// Send an archive to the agent's store, unless another is on its way.
    func importImage(_ file: URL) {
        guard importing == nil, let bytes = try? file.resourceValues(forKeys: [.fileSizeKey]).fileSize else {
            return
        }
        importing = Importing(name: file.lastPathComponent, bytes: Int64(bytes), sent: 0)
        Task {
            do {
                try await agent.importImage(file) { sent in
                    Task { @MainActor in self.importing?.sent = sent }
                }
            } catch {
                report(error)
            }
            importing = nil
        }
    }

    func catalog() async throws -> [AgentState.CatalogModel] {
        try await agent.catalog()
    }

    func releases(of model: String) async throws -> [AgentState.Release] {
        try await agent.releases(of: model)
    }

    /// What a restore would install on each target, or nil once the agent's
    /// refusal has been shown.
    func plan(_ hubs: [RestoreSelection]) async -> [AgentState.Planned]? {
        do {
            return try await agent.plan(hubs)
        } catch {
            report(error)
            return nil
        }
    }

    /// The supervision identity as a PKCS #12 archive in base64, or nil once
    /// the agent's refusal has been shown.
    func exportSupervision(password: String) async -> String? {
        do {
            return try await agent.exportSupervision(password: password)
        } catch {
            report(error)
            return nil
        }
    }

    @discardableResult
    func send(_ request: AgentRequest) async -> Bool {
        do {
            try await agent.send(request)
            return true
        } catch {
            report(error)
            return false
        }
    }

    struct Importing: Equatable {
        var name: String
        var bytes: Int64
        var sent: Int64

        var fraction: Double {
            bytes > 0 ? Double(sent) / Double(bytes) : 0
        }
    }

    private func report(_ error: any Error) {
        Log.agent.error("Request refused: \(error.localizedDescription)")
        let alert = NSAlert()
        alert.messageText = "The Action Could Not Be Completed"
        alert.informativeText = error.localizedDescription
        NSApp.activate()
        alert.runModal()
    }
}
