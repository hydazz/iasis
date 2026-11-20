import Foundation
import Observation
import OSLog

/// The agent's state for the views, and the requests they make of it.
@Observable
final class AgentModel {
    /// The agent's latest state, or nil while no agent answers.
    private(set) var state: AgentState?
    /// Why the last request was refused.
    private(set) var failure: String?
    /// The port whose erase is waiting for the user's word.
    private(set) var confirming: PortID?

    private let agent: any AgentService
    private var watching: Task<Void, Never>?

    init(agent: any AgentService) {
        self.agent = agent
    }

    /// A port is busy with something the user should leave alone.
    var isBusy: Bool {
        ports.contains { $0.light.pulse }
    }

    var hasFault: Bool {
        ports.contains { $0.light.hue == .red }
    }

    private var ports: [AgentState.Port] {
        state?.boards.flatMap(\.ports) ?? []
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
                confirming = nil
                try? await Task.sleep(for: .seconds(2))
            }
        }
    }

    func perform(_ request: AgentRequest) {
        Task {
            await send(request)
        }
    }

    func send(_ request: AgentRequest) async {
        do {
            try await agent.send(request)
            failure = nil
        } catch {
            Log.agent.error("Request refused: \(error.localizedDescription)")
            failure = error.localizedDescription
        }
    }

    func dismissFailure() {
        failure = nil
    }

    /// Ask the user before erasing a port's target.
    func askToRestore(_ port: PortID) {
        confirming = port
    }

    func cancelRestore() {
        confirming = nil
    }

    /// Erase and restore the port the user has just confirmed.
    func confirmRestore() {
        guard let port = confirming else {
            return
        }
        confirming = nil
        perform(.restore(board: port.board, ports: [port.port]))
    }
}
