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
            "Enter DFU", "Restart", "Revive", "Restore…", "Restoring This Mac Needs Apple Configurator", "Port Power",
        ])
        let restart = commands.indexOfItem(withTitle: "Restart")
        commands.performActionForItem(at: restart)
        for _ in 0 ..< 100 where await agent.received.isEmpty {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(await agent.received == [.reboot(board: "a0e012192cdfdb67", port: 2)])
    }

    @MainActor @Test func anOpenMenuShowsChangesAtOnceAndKeepsItsItems() async throws {
        var state = try Fixtures.state
        let agent = FakeAgent(reports: [state])
        let model = AgentModel(agent: agent)
        model.start()
        for _ in 0 ..< 200 where model.menu.boards == nil {
            try await Task.sleep(for: .milliseconds(10))
        }
        let opened = model.menu

        // Progress, a finished job and what a device will take are state.
        state.boards[0].ports[2].stage?.percent = 44
        state.boards[0].ports[1].reboot = false
        let progressed = state
        Task.detached {
            try await Task.sleep(for: .milliseconds(50))
            await agent.publish(progressed)
        }
        let live = await trackMenu(for: 0.4) { model.menu }
        #expect(live == StatusMenuState(progressed))
        #expect(live.titles == opened.titles)

        // A device with another name is another item, once the menu closes.
        state.boards[0].ports[2].device = "iPhone 16"
        state.boards[0].ports[2].stage?.percent = 45
        let renamed = state
        Task.detached {
            try await Task.sleep(for: .milliseconds(50))
            await agent.publish(renamed)
        }
        let held = await trackMenu(for: 0.4) { (model.state, model.menu) }
        #expect(held.0 == renamed)
        #expect(held.1 == live)
        for _ in 0 ..< 200 where model.menu == live {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(model.menu == StatusMenuState(renamed))
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
        #expect(menu.download == "27.2.1, 43%")
        #expect(menu.titles == ["Repair Bench", "MacBook Air", "Configurator", "iPad", "Unidentified Device", "Unidentified Device", "Download"])
        state.boards[0].ports[2].state = "downloading image"
        #expect(StatusMenuState(state).download == nil)
        #expect(StatusMenuState(nil).boards == nil)
        #expect(StatusMenuState(nil).status == .offline)
    }
}
