import AppKit
import Observation
import OSLog

/// The agent's state for the views, and the requests they make of it.
@Observable
final class AgentModel {
    /// The agent's latest state, or nil while no agent answers.
    private(set) var state: AgentState?
    /// Keep the titles under the pointer until AppKit finishes tracking the menu.
    private(set) var menuSnapshot: AgentState?
    private var menuRefreshPending = false
    private let agent: any AgentService
    private var watching: Task<Void, Never>?

    init(agent: any AgentService) {
        self.agent = agent
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

    private func report(_ error: any Error) {
        Log.agent.error("Request refused: \(error.localizedDescription)")
        let alert = NSAlert()
        alert.messageText = "The Action Could Not Be Completed"
        alert.informativeText = error.localizedDescription
        NSApp.activate()
        alert.runModal()
    }
}
