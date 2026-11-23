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
        next.boards[0].ports[1].device = "iPhone"
        await agent.publish(next)
        try await waitUntil { model.state?.devices[1].name == "iPhone" }
    }

    @Test func menuStatusFollowsConnectedBoardsAndPortLights() throws {
        var state = try Fixtures.state
        #expect(MenuBarStatus(nil) == .offline)
        #expect(MenuBarStatus(state) == .fault)
        state.boards[0].ports.removeAll { $0.light.hue == .red }
        state.images.fetch = nil
        #expect(MenuBarStatus(state) == .busy)
        state.boards[0].ports.removeAll { $0.light.pulse }
        #expect(MenuBarStatus(state) == .ready)
        state.images.fetch = try Fixtures.state.images.fetch
        #expect(MenuBarStatus(state) == .busy)
        state.boards[0].notice = "Cannot read board"
        #expect(MenuBarStatus(state) == .fault)
        state.boards = []
        #expect(MenuBarStatus(state) == .offline)
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
