import AppKit
@testable import Iasis
import SwiftUI
import Testing

struct MenuTests {
    @Test func deviceCommandsUseTheSelectedPort() async throws {
        let state = try Fixtures.state
        let agent = FakeAgent(reports: [state])
        let model = AgentModel(agent: agent)
        let menu = NSHostingMenu(rootView: DeviceActionButtons(actions: DeviceActions([state.devices[1]])) {
            model.perform($0)
        } restore: { _ in })
        menu.update()
        let restart = menu.indexOfItem(withTitle: "Restart")
        try #require(restart >= 0)
        menu.performActionForItem(at: restart)
        for _ in 0 ..< 100 where await agent.received.isEmpty {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(await agent.received == [.reboot(board: "a0e012192cdfdb67", port: 2)])
    }

    @Test func theBoardMenuUpdatesABoardThatRunsAnotherIasisOS() async throws {
        var state = try Fixtures.state
        let agent = FakeAgent(reports: [state])
        let model = AgentModel(agent: agent)
        model.start()
        for _ in 0 ..< 100 where model.state == nil {
            try await Task.sleep(for: .milliseconds(10))
        }
        let menu = NSHostingMenu(rootView: BoardMenuItems(model: model))
        menu.update()
        let update = try #require(menu.item(withTitle: "Update IasisOS"))
        #expect(!update.isEnabled)
        state.boards[0].update = .init(version: "0.2.0", required: false)
        await agent.publish(state)
        for _ in 0 ..< 100 where !update.isEnabled {
            try await Task.sleep(for: .milliseconds(10))
            menu.update()
        }
        let index = menu.index(of: update)
        try #require(index >= 0)
        menu.performActionForItem(at: index)
        for _ in 0 ..< 100 where await agent.received.isEmpty {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(await agent.received == [.update(board: "a0e012192cdfdb67", bytes: nil)])
    }

    @Test func aDeviceShowsItsPortNameAndStatusAndTheDownloadItsPercent() async throws {
        let (model, _) = try await started(Fixtures.state)
        let menu = NSHostingMenu(rootView: StatusMenu(browser: DeviceBrowser(model: model)))
        menu.update()
        let mac = try #require(menu.item(withTitle: "MacBook Air"))
        let two = NSImage(systemSymbolName: "2.circle", accessibilityDescription: nil)?.accessibilityDescription
        #expect(mac.image?.accessibilityDescription == two)
        #expect(mac.subtitle == "DFU")
        #expect(mac.submenu != nil)
        #expect(menu.item(withTitle: "iPad")?.subtitle == "Restoring 43%")
        let download = try #require(menu.item(withTitle: "Downloading iPadOS 27.2.1"))
        #expect(download.subtitle == "42%")
        #expect(!download.isEnabled)
        #expect(menu.items.allSatisfy { $0.badge == nil })
    }

    @Test func anOpenMenuKeepsTheBoardsAsTheyStoodWhenItOpened() async throws {
        var state = try Fixtures.state
        let (model, agent) = try await started(state)
        let menu = NSHostingMenu(rootView: StatusMenu(browser: DeviceBrowser(model: model)))
        menu.update()
        state.boards[0].ports[1].mode = .normal
        let next = state
        let shown = await whileMenuTracks {
            Task { await agent.publish(next) }
        } until: {
            model.state == next
        } then: {
            menu.update()
            return menu.item(withTitle: "MacBook Air")?.subtitle
        }
        #expect(shown == "DFU")
        // Closed, the menu catches up.
        try await waitUntil {
            menu.update()
            return menu.item(withTitle: "MacBook Air")?.subtitle == nil
        }
    }

    @Test func aCommandFromAnOpenMenuReachesOnlyTheAttachmentItShowed() async throws {
        var state = try Fixtures.state
        let (model, agent) = try await started(state)
        let menu = NSHostingMenu(rootView: StatusMenu(browser: DeviceBrowser(model: model)))
        menu.update()
        let submenu = try #require(menu.item(withTitle: "MacBook Air")?.submenu)
        submenu.update()
        let restart = submenu.indexOfItem(withTitle: "Restart")
        try #require(restart >= 0)
        submenu.performActionForItem(at: restart)
        try await waitUntil { await agent.received.count == 1 }
        // Another Mac takes the port while the menu is open.
        state.boards[0].ports[1].generation += 1
        let next = state
        await whileMenuTracks {
            Task { await agent.publish(next) }
        } until: {
            model.state == next
        } then: {
            submenu.performActionForItem(at: restart)
        }
        try await Task.sleep(for: .milliseconds(200))
        #expect(await agent.received == [.reboot(board: "a0e012192cdfdb67", port: 2)])
    }

    private func started(_ state: AgentState) async throws -> (AgentModel, FakeAgent) {
        let agent = FakeAgent(reports: [state])
        let model = AgentModel(agent: agent)
        model.start()
        try await waitUntil { model.menuSnapshot != nil }
        return (model, agent)
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

    /// Runs the main loop in the mode AppKit tracks a menu in, as while one is
    /// open, and looks before it returns to the default mode. Spinning from a
    /// run loop callout lets the main queue drain meanwhile.
    private func whileMenuTracks<T: Sendable>(
        _ start: @escaping @MainActor () -> Void,
        until done: @escaping @MainActor () -> Bool,
        then look: @escaping @MainActor () -> T
    ) async -> T {
        await withCheckedContinuation { continuation in
            RunLoop.main.perform(inModes: [.default]) {
                MainActor.assumeIsolated {
                    start()
                    let limit = Date(timeIntervalSinceNow: 2)
                    while !done(), Date() < limit {
                        RunLoop.main.run(mode: .eventTracking, before: Date(timeIntervalSinceNow: 0.01))
                    }
                    continuation.resume(returning: look())
                }
            }
        }
    }
}
