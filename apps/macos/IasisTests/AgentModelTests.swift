import Foundation
@testable import Iasis
import Testing

struct AgentModelTests {
    @Test func theModelFollowsTheAgent() async throws {
        let state = try Fixtures.state
        let agent = FakeAgent(reports: [state])
        let model = AgentModel(agent: agent)
        #expect(model.state == nil)
        model.start()
        try await waitUntil { model.state != nil }
        #expect(model.state == state)
        var next = state
        next.hubs[0].ports[1].device = "iPhone"
        await agent.publish(next)
        try await waitUntil { model.state?.devices[1].name == "iPhone" }
    }

    @Test func anAgentOfAnotherVersionIsRestartedOnce() async throws {
        var state = try Fixtures.state
        state.version = "0.0.1"
        let agent = FakeAgent(reports: [state])
        let restarts = Restarts()
        let model = AgentModel(agent: agent) { await restarts.count() }
        model.start()
        try await waitUntil { await restarts.total == 1 }
        #expect(model.state == nil)
        // The restarted agent is still another version.
        await agent.publish(state)
        try await waitUntil { model.state != nil }
        #expect(await restarts.total == 1)
    }

    @Test func anAgentWhoseStateCannotBeReadIsRestarted() async throws {
        let agent = FakeAgent(reports: [])
        let restarts = Restarts()
        let model = AgentModel(agent: agent) { await restarts.count() }
        model.start()
        await agent.fail(with: DecodingError.dataCorrupted(.init(codingPath: [], debugDescription: "another build's state")))
        try await waitUntil { await restarts.total == 1 }
        #expect(model.state == nil)
    }

    @Test func menuStatusFollowsConnectedHubsAndPortLights() throws {
        var state = try Fixtures.state
        #expect(MenuBarStatus(nil) == .offline)
        #expect(MenuBarStatus(state) == .fault)
        state.hubs[0].ports.removeAll { $0.light.hue == .red }
        state.images.fetch = nil
        #expect(MenuBarStatus(state) == .busy)
        state.hubs[0].ports.removeAll { $0.light.pulse }
        #expect(MenuBarStatus(state) == .ready)
        state.hubs[0].ports[0].run = .init(steps: [.init(kind: .setRegion, state: .active)])
        #expect(MenuBarStatus(state) == .busy)
        state.hubs[0].ports[0].run?.steps[0].state = .complete
        #expect(MenuBarStatus(state) == .ready)
        state.images.fetch = try Fixtures.state.images.fetch
        #expect(MenuBarStatus(state) == .busy)
        state.hubs[0].notice = "Cannot read hub"
        #expect(MenuBarStatus(state) == .fault)
        state.hubs = []
        #expect(MenuBarStatus(state) == .offline)
    }

    /// The model reconnects 2 s after an agent goes, so allow longer.
    private func waitUntil(_ done: () async -> Bool) async throws {
        for _ in 0 ..< 400 {
            if await done() {
                return
            }
            try await Task.sleep(for: .milliseconds(10))
        }
        Issue.record("the condition never held")
    }
}

private actor Restarts {
    private(set) var total = 0

    func count() {
        total += 1
    }
}
