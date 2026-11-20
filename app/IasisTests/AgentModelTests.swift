import Foundation
@testable import Iasis
import Testing

struct AgentModelTests {
    @Test func theModelFollowsTheAgent() async throws {
        let state = try Fixtures.state
        let model = AgentModel(agent: FakeAgent(reports: [state]))
        #expect(model.state == nil)
        model.start()
        try await waitUntil { model.state != nil }
        #expect(model.state == state)
        #expect(model.isBusy)
        #expect(model.hasFault)
    }

    @Test func anEraseWaitsForTheUsersWord() async throws {
        let agent = FakeAgent()
        let model = AgentModel(agent: agent)
        let port = PortID(board: "a0e012192cdfdb67", port: 2)
        model.askToRestore(port)
        #expect(model.confirming == port)
        model.cancelRestore()
        model.confirmRestore()
        #expect(await agent.received.isEmpty)

        model.askToRestore(port)
        model.confirmRestore()
        #expect(model.confirming == nil)
        try await waitUntil { await !agent.received.isEmpty }
        #expect(await agent.received == [.restore(board: port.board, ports: [2])])
    }

    @Test func aRefusalIsShownUntilDismissed() async {
        let model = AgentModel(agent: FakeAgent(refusal: "port 2 has no ready target"))
        await model.send(.dfu(board: "a0e0", port: 2))
        #expect(model.failure == "port 2 has no ready target")
        model.dismissFailure()
        #expect(model.failure == nil)
    }

    private func waitUntil(_ done: () async -> Bool) async throws {
        for _ in 0 ..< 200 {
            if await done() {
                return
            }
            try await Task.sleep(for: .milliseconds(10))
        }
        Issue.record("the condition never held")
    }
}
