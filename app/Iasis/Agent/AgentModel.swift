import AppKit
import Observation
import OSLog

/// The agent's state for the views, and the requests they make of it.
@Observable
final class AgentModel {
    /// The agent's latest state, or nil while no agent answers.
    private(set) var state: AgentState?
    /// What the menu bar shows. An open menu shows each change of state at
    /// once and keeps the items it opened with.
    private(set) var menu = StatusMenuState(nil)
    private let agent: any AgentService
    private var watching: Task<Void, Never>?
    private var menuIsCurrent = true

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
        let next = StatusMenuState(update)
        if next.titles == menu.titles {
            if next != menu {
                menu = next
            }
            return
        }
        // New titles mean new items. Menu tracking runs the main loop in its
        // own mode, so this waits for an open menu to close.
        guard menuIsCurrent else { return }
        menuIsCurrent = false
        RunLoop.main.perform(inModes: [.default]) {
            MainActor.assumeIsolated { self.refreshMenu() }
        }
    }

    private func refreshMenu() {
        menuIsCurrent = true
        let current = StatusMenuState(state)
        if current != menu {
            menu = current
        }
    }

    func perform(_ requests: [AgentRequest]) {
        Task {
            for request in requests {
                guard await send(request) else { return }
            }
        }
    }

    @discardableResult
    func send(_ request: AgentRequest) async -> Bool {
        do {
            try await agent.send(request)
            return true
        } catch {
            Log.agent.error("Request refused: \(error.localizedDescription)")
            let alert = NSAlert()
            alert.messageText = "The Action Could Not Be Completed"
            alert.informativeText = error.localizedDescription
            NSApp.activate()
            alert.runModal()
            return false
        }
    }
}
