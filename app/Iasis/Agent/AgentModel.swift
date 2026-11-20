import AppKit
import Observation
import OSLog

/// The agent's state for the views, and the requests they make of it.
@Observable
final class AgentModel {
    /// The agent's latest state, or nil while no agent answers.
    private(set) var state: AgentState?
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
                        self.state = state
                    }
                } catch {
                    Log.agent.debug("Agent unavailable: \(error.localizedDescription)")
                }
                state = nil
                try? await Task.sleep(for: .seconds(2))
            }
        }
    }

    func perform(_ request: AgentRequest) {
        Task { await send(request) }
    }

    @discardableResult
    func send(_ request: AgentRequest) async -> Bool {
        do {
            try await agent.send(request)
            return true
        } catch {
            Log.agent.error("Request refused: \(error.localizedDescription)")
            let alert = NSAlert()
            alert.messageText = "Action Failed"
            alert.informativeText = error.localizedDescription
            NSApp.activate()
            alert.runModal()
            return false
        }
    }
}
