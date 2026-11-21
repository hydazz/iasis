import AppKit
@testable import Iasis
import SwiftUI
import Testing

struct MenuTests {
    @Test func aDeviceIsOneItemWithItsActionsInASubmenu() async throws {
        let state = try Fixtures.state
        let agent = FakeAgent(reports: [state])
        let model = AgentModel(agent: agent)
        model.start()
        for _ in 0 ..< 200 where model.menu.boards == nil {
            try await Task.sleep(for: .milliseconds(10))
        }
        let menu = NSHostingMenu(rootView: StatusMenu(model: model))
        menu.update()
        let items = menu.items
        #expect(items.contains { $0.title == "Repair Bench" })
        #expect(!items.contains { $0.title == "Empty" })
        let mac = try #require(items.first { $0.title == "MacBook Air" })
        #expect(mac.badge?.stringValue == "DFU")
        #expect(mac.image != nil)
        let commands = try #require(mac.submenu)
        commands.update()
        #expect(commands.items.map(\.title).filter { !$0.isEmpty } == [
            "Enter DFU", "Restart", "Revive", "Restore…", "Apple Configurator restores this Mac", "Turn Port Off",
        ])
        let restart = commands.indexOfItem(withTitle: "Restart")
        commands.performActionForItem(at: restart)
        for _ in 0 ..< 100 where await agent.received.isEmpty {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(await agent.received == [.reboot(board: "a0e012192cdfdb67", port: 2)])
    }

    @MainActor @Test func anOpenMenuKeepsTheContentItOpenedWith() async throws {
        var state = try Fixtures.state
        let agent = FakeAgent(reports: [state])
        let model = AgentModel(agent: agent)
        model.start()
        for _ in 0 ..< 200 where model.menu.boards == nil {
            try await Task.sleep(for: .milliseconds(10))
        }
        let opened = model.menu
        state.boards[0].ports[2].stage?.percent = 44
        let next = state
        Task.detached {
            try await Task.sleep(for: .milliseconds(50))
            await agent.publish(next)
        }
        let shown = await trackMenu(for: 0.4) { (model.state, model.menu) }
        #expect(shown.0 == next)
        #expect(shown.1 == opened)
        for _ in 0 ..< 200 where model.menu == opened {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(model.menu == StatusMenuState(next))
    }

    /// A menu tracks the mouse in a run loop mode of its own until it
    /// closes. Returns what `reading` finds just before it does.
    private func trackMenu<Reading: Sendable>(
        for seconds: TimeInterval,
        reading: @escaping @MainActor () -> Reading
    ) async -> Reading {
        await withCheckedContinuation { closed in
            RunLoop.main.perform(inModes: [.default]) {
                let closes = Date.now.addingTimeInterval(seconds)
                while Date.now < closes {
                    RunLoop.main.run(mode: .eventTracking, before: closes)
                }
                closed.resume(returning: MainActor.assumeIsolated(reading))
            }
        }
    }

    @Test func theMenuStateSaysWhatTheIconAndFooterShow() throws {
        var state = try Fixtures.state
        let menu = StatusMenuState(state)
        #expect(menu.status == .fault)
        #expect(menu.boards?.first?.devices.map(\.device.port.number) == [2, 3, 4, 5])
        #expect(menu.download == "Downloading 27.2.1 (43%)")
        state.boards[0].ports[2].state = "downloading image"
        #expect(StatusMenuState(state).download == nil)
        #expect(StatusMenuState(nil).boards == nil)
        #expect(StatusMenuState(nil).status == .offline)
    }
}
