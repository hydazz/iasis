import AppKit
import Observation
import OSLog

/// The agent's state for the views, and the requests they make of it.
@Observable
final class AgentModel {
    /// The agent's latest state, or nil while no agent answers.
    private(set) var state: AgentState?
    private(set) var menu: DeviceMenuState?
    private(set) var menuBarStatus = MenuBarStatus.offline
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

    private func publish(_ update: AgentState?) {
        state = update
        let content = update.map(DeviceMenuState.init)
        if content != menu {
            menu = content
        }
        let status = MenuBarStatus(update)
        if status != menuBarStatus {
            menuBarStatus = status
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
