import AppKit
import Observation
import OSLog

/// The agent's state for the views, and the requests they make of it.
@Observable
final class AgentModel {
    /// The agent's latest state, or nil while no agent answers.
    private(set) var state: AgentState?
    /// The state as of the last time no menu was open, and all the menu bar
    /// item reads. AppKit tracks a menu in its own run loop mode, so the copy
    /// waits for the default one.
    private(set) var menuSnapshot: AgentState?
    /// An archive on its way into the agent's store.
    private(set) var importing: Importing?
    /// Boards being written IasisOS, by serial.
    private(set) var updating: Set<String> = []
    private var menuRefreshPending = false
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
                        // An app put in place of another finds that app's
                        // agent still running. One restart: a difference it
                        // cannot fix would otherwise repeat it forever.
                        if state.version != Self.version, let restartAgent, !restartedAgent {
                            restartedAgent = true
                            Log.agent.notice("Restarting agent \(state.version, privacy: .public) for app \(Self.version, privacy: .public)")
                            await restartAgent()
                            break
                        }
                        publish(state)
                    }
                } catch {
                    Log.agent.debug("Agent unavailable: \(error.localizedDescription)")
                }
                publish(nil)
                try? await Task.sleep(for: .seconds(2))
            }
        }
    }

    private func publish(_ next: AgentState?) {
        state = next
        guard !menuRefreshPending else { return }
        menuRefreshPending = true
        RunLoop.main.perform(inModes: [.default]) {
            MainActor.assumeIsolated {
                self.menuSnapshot = self.state
                self.menuRefreshPending = false
            }
        }
    }

    func perform(_ requests: [AgentRequest]) {
        Task {
            for request in requests {
                guard await send(request) else { return }
            }
        }
    }

    /// Write the IasisOS the agent carries to a board.
    func updateFirmware(of board: String) {
        guard updating.insert(board).inserted else { return }
        Task {
            await send(.update(board: board, bytes: nil))
            updating.remove(board)
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
    func plan(_ boards: [RestoreSelection]) async -> [AgentState.Planned]? {
        do {
            return try await agent.plan(boards)
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
