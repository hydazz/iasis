import AppKit
@testable import Iasis
import SwiftUI
import Testing

struct MenuTests {
    @Test func deviceCommandsUseTheSelectedPort() async throws {
        let state = try Fixtures.state
        let (model, agent) = try await started(state)
        let browser = DeviceBrowser(model: model)
        browser.selection = [state.devices[1].id]
        let menu = NSHostingMenu(rootView: DeviceActionButtons(browser: browser))
        menu.update()
        let restart = menu.indexOfItem(withTitle: "Restart")
        try #require(restart >= 0)
        menu.performActionForItem(at: restart)
        for _ in 0 ..< 100 where await agent.received.isEmpty {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(await agent.received == [.reboot(hubs: [RestoreSelection(hub: "a0e012192cdfdb67", session: 77, targets: [RestoreTarget(port: 2, generation: 2, ecid: "001a79d10e6bc01c")])])])
    }

    @Test func contextMenuShowsOnlyActionsSharedByTheWholeSelection() async throws {
        let state = try Fixtures.state
        let (model, _) = try await started(state)
        let browser = DeviceBrowser(model: model)
        let ids = Set([state.devices[1].id, state.devices[3].id])
        let menu = NSHostingMenu(rootView: DeviceActionButtons(browser: browser, contextSelection: ids, checksOnOpen: true))
        menu.update()
        #expect(menu.items.filter { !$0.isSeparatorItem }.map(\.title) == ["Restart"])
        browser.selection = ids.union([state.devices[0].id])
        #expect(browser.actions == DeviceActions())
    }

    @Test func oneMenuCheckOpensSetupForThePinnedSelection() async throws {
        var state = try Fixtures.state
        state.hubs[0].ports[1].device = nil
        state.hubs[0].ports[1].mode = .normal
        state.hubs[0].ports[3].device = "iPhone 16"
        state.hubs[0].ports[3].mode = .normal
        let (model, agent) = try await started(state)
        let browser = DeviceBrowser(model: model)
        let pair = [state.devices[1], state.devices[3]]
        let hubs = try #require(pair.selections)
        let general = AgentRequest.advanceSetup(hubs: hubs, settings: nil, dep: false, checkOnly: true)
        browser.selection = Set(pair.map(\.id))
        let menu = DeviceRowMenu(browser: browser, id: pair[0].id).menu()
        menu.update()
        try await waitUntil { menu.item(withTitle: "Advance Setup…") != nil }
        #expect(await agent.received == [general])

        state.hubs[0].ports[3].light.dim.toggle()
        await agent.publish(state)
        try await waitUntil { model.state == state }
        menu.update()
        #expect(await agent.received == [general])
        menu.performActionForItem(at: menu.indexOfItem(withTitle: "Advance Setup…"))
        let draft = try #require(browser.advancing)
        #expect(draft.setLanguage && !draft.enroll)
        #expect(draft.language == .current)
        #expect(await agent.received == [general])
        let original = try #require(draft.action(in: browser))
        await draft.check.refresh(original, model: model)
        #expect(draft.check.accepts(original))

        state.hubs[0].ports[1].ecid = "0000000000000123"
        let replacement = try #require([state.devices[1], state.devices[3]].selections)
        let refusal = AgentRequest.advanceSetup(hubs: replacement, settings: nil, dep: false, checkOnly: true)
        await agent.refuse(refusal)
        await agent.publish(state)
        try await waitUntil { await agent.received.contains(refusal) && menu.item(withTitle: "Advance Setup…") == nil }
        #expect(draft.action(in: browser) == nil)
        await browser.advance(draft)
        #expect(await !agent.received.contains(.advanceSetup(hubs: hubs, settings: .current, dep: false, checkOnly: false)))

        state.hubs[0].ports[3].job = .init(operation: .restore, phase: .running)
        await agent.publish(state)
        try await waitUntil { model.state == state }
        #expect(browser.selectedSetup == nil)
    }

    @Test func setupChecksAndSendsOnlyTheChoicesInTheSheet() async throws {
        var state = try Fixtures.state
        state.hubs[0].ports[3].mode = .normal
        let language = AgentState.SetupLanguage(language: "de-DE", locale: "de_DE")
        state.postRestore.setupLanguage = language
        state.postRestore.tryEnrollment = true
        let (model, agent) = try await started(state)
        let browser = DeviceBrowser(model: model)
        let devices = [state.devices[3]]
        let targets = try #require(SetupAction.menuCheck(for: devices))
        let ids = Set(devices.map(\.id))
        browser.chooseSetup(for: ids, checked: targets)
        let draft = try #require(browser.advancing)
        #expect(draft.language == language && draft.setLanguage && draft.enroll)
        let both = try #require(draft.action(in: browser))
        let reason = "Hub Repair Bench, Port 4: the selected region is unavailable."
        await agent.refuse(.advanceSetup(hubs: targets.hubs, settings: language, dep: true, checkOnly: true), reason: reason)

        let window = NSWindow(contentViewController: NSHostingController(rootView: SetupSheet(browser: browser, draft: draft)))
        defer { window.orderOut(nil) }
        window.setFrameOrigin(NSPoint(x: -2000, y: -2000))
        window.orderBack(nil)
        try await waitUntil { draft.check.checked == both }
        #expect(!draft.check.accepts(both) && draft.check.refusal == reason)
        await browser.advance(draft)
        #expect(await !agent.received.contains(.advanceSetup(hubs: targets.hubs, settings: language, dep: true, checkOnly: false)))

        draft.setLanguage = false
        draft.enroll = false
        #expect(draft.action(in: browser) == nil)
        draft.enroll = true
        let chosen = try #require(draft.action(in: browser))
        try await waitUntil { draft.check.accepts(chosen) }
        #expect(await agent.received.contains(.advanceSetup(hubs: targets.hubs, settings: nil, dep: true, checkOnly: true)))
        let signIn = "Hub Repair Bench, Port 4: the device requires sign-in to enroll."
        let enrollment = AgentRequest.advanceSetup(hubs: targets.hubs, settings: nil, dep: true, checkOnly: false)
        await agent.refuse(enrollment, reason: signIn)
        await browser.advance(draft)
        #expect(await agent.received.contains(enrollment))
        #expect(browser.advancing === draft && draft.submissionError == signIn)

        draft.setLanguage = true
        draft.enroll = false
        let languageOnly = try #require(draft.action(in: browser))
        try await waitUntil { draft.check.accepts(languageOnly) }
        #expect(draft.submissionError == nil)
        await browser.advance(draft)
        #expect(browser.advancing == nil)
        #expect(await agent.received.contains(.advanceSetup(hubs: targets.hubs, settings: language, dep: false, checkOnly: false)))
        #expect(model.state?.postRestore == state.postRestore)
        #expect(await !agent.received.contains(.setupLanguage(nil)))
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
