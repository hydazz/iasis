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
        #expect(await agent.received == [.reboot(hubs: [RestoreSelection(hub: "a0e012192cdfdb67", session: 77, targets: [RestoreTarget(port: 2, generation: 2, ecid: "001a79d10e6bc01c")])])])
    }

    @Test func theHubMenuUpdatesAHubThatRunsAnotherIasisOS() async throws {
        var state = try Fixtures.state
        let agent = FakeAgent(reports: [state])
        let model = AgentModel(agent: agent)
        model.start()
        for _ in 0 ..< 100 where model.state == nil {
            try await Task.sleep(for: .milliseconds(10))
        }
        let menu = NSHostingMenu(rootView: HubMenuItems(model: model))
        menu.update()
        let update = try #require(menu.item(withTitle: "Update IasisOS"))
        #expect(!update.isEnabled)
        state.hubs[0].update = .init(version: "0.2.0", required: false)
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
        #expect(await agent.received == [.update(hub: "a0e012192cdfdb67", bytes: nil)])
    }

    @Test func aDeviceShowsItsPortNameAndStatusAndTheDownloadItsPercent() async throws {
        let (model, _) = try await started(Fixtures.state)
        let status = StatusMenu(browser: DeviceBrowser(model: model), showDevices: {}, showSettings: {})
        let menu = status.menu
        status.menuNeedsUpdate(menu)
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

    @Test func progressAndCommandsUpdateInPlaceWhileTracking() async throws {
        var state = try Fixtures.state
        let (model, agent) = try await started(state)
        let status = StatusMenu(browser: DeviceBrowser(model: model), showDevices: {}, showSettings: {})
        let menu = status.menu
        status.menuNeedsUpdate(menu)
        let row = try #require(menu.item(withTitle: "iPad"))
        let submenu = try #require(row.submenu)
        let restart = try #require(submenu.item(withTitle: "Restart"))
        let download = try #require(menu.item(withTitle: "Downloading iPadOS 27.2.1"))
        #expect(!restart.isEnabled)
        status.menuWillOpen(menu)
        state.hubs[0].ports[2].stage?.percent = 84
        state.hubs[0].ports[2].reboot = true
        state.images.fetch?.done = 8_000_000_000
        let next = state
        await whileMenuTracks {
            Task { await agent.publish(next) }
        } until: {
            row.subtitle == "Restoring 84%" && restart.isEnabled && download.subtitle == "73%"
        }
        #expect(menu.item(withTitle: "iPad") === row)
        #expect(row.submenu === submenu)
        #expect(submenu.item(withTitle: "Restart") === restart)
        #expect(menu.item(withTitle: "Downloading iPadOS 27.2.1") === download)
        status.menuDidClose(menu)
    }

    @Test func aHubAndDownloadCanAppearInAnOpenEmptyMenu() async throws {
        let connected = try Fixtures.state
        var empty = connected
        empty.hubs = []
        empty.images.fetch = nil
        let (model, agent) = try await started(empty)
        let status = StatusMenu(browser: DeviceBrowser(model: model), showDevices: {}, showSettings: {})
        let menu = status.menu
        status.menuNeedsUpdate(menu)
        let message = try #require(menu.item(withTitle: "No Hubs Connected"))
        #expect(!message.isHidden)
        status.menuWillOpen(menu)
        await agent.publish(connected)
        try await waitUntil { menu.item(withTitle: "MacBook Air") != nil }
        #expect(message.isHidden)
        #expect(menu.item(withTitle: "Downloading iPadOS 27.2.1")?.isHidden == false)
        #expect(menu.item(withTitle: "MacBook Air")?.submenu?.item(withTitle: "Restart")?.isEnabled == true)
        status.menuDidClose(menu)
    }

    @Test(arguments: [false, true])
    func aReplacementCannotInheritTheOpenSubmenusActions(reconnectedHub: Bool) async throws {
        var state = try Fixtures.state
        let original = DeviceActions([state.devices[1]]).restart
        let (model, agent) = try await started(state)
        let browser = DeviceBrowser(model: model)
        let status = StatusMenu(browser: browser, showDevices: {}, showSettings: {})
        let menu = status.menu
        status.menuNeedsUpdate(menu)
        let row = try #require(menu.item(withTitle: "MacBook Air"))
        let submenu = try #require(row.submenu)
        let restart = submenu.indexOfItem(withTitle: "Restart")
        let info = submenu.indexOfItem(withTitle: "Get Info")
        try #require([restart, info].allSatisfy { $0 >= 0 })
        status.menuWillOpen(menu)
        submenu.performActionForItem(at: restart)
        try await waitUntil { await agent.received.count == 1 }
        if reconnectedHub {
            state.hubs[0].session = 78
        } else {
            state.hubs[0].ports[1].generation += 1
        }
        state.hubs[0].ports[1].device = "Mac mini"
        await agent.publish(state)
        try await waitUntil { row.subtitle == "Disconnected" }
        #expect([restart, info].allSatisfy { submenu.item(at: $0)?.isEnabled == false })
        for index in [restart, info] {
            submenu.performActionForItem(at: index)
        }
        let replacement = try #require(menu.item(withTitle: "Mac mini")?.submenu)
        replacement.performActionForItem(at: replacement.indexOfItem(withTitle: "Restart"))
        try await waitUntil { await agent.received.count >= 2 }
        #expect(browser.selection.isEmpty)
        #expect(await agent.received == original + DeviceActions([state.devices[1]]).restart)
        status.menuDidClose(menu)
    }

    private func started(_ state: AgentState) async throws -> (AgentModel, FakeAgent) {
        let agent = FakeAgent(reports: [state])
        let model = AgentModel(agent: agent)
        model.start()
        try await waitUntil { model.state != nil }
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
    private func whileMenuTracks(
        _ start: @escaping @MainActor () -> Void,
        until done: @escaping @MainActor () -> Bool
    ) async {
        await withCheckedContinuation { continuation in
            RunLoop.main.perform(inModes: [.default]) {
                MainActor.assumeIsolated {
                    start()
                    let limit = Date(timeIntervalSinceNow: 2)
                    while !done(), Date() < limit {
                        RunLoop.main.run(mode: .eventTracking, before: Date(timeIntervalSinceNow: 0.01))
                    }
                    #expect(done())
                    continuation.resume()
                }
            }
        }
    }
}
