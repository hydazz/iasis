import AppKit
@testable import Iasis
import SwiftUI
import Testing

struct MenuTests {
    @Test func inspectingAnEmptyPortFollowsItsNextAttachment() async throws {
        var state = try Fixtures.state
        let (model, agent) = try await started(state)
        let browser = DeviceBrowser(model: model)
        let port = state.devices[0].id
        let menu = NSHostingMenu(rootView: DeviceActionButtons(browser: browser, contextSelection: [port]))
        menu.update()
        #expect(menu.items.filter { !$0.isSeparatorItem }.isEmpty)
        browser.selection = [port]
        #expect(browser.inspected?.id == port)
        #expect(browser.inspected?.name == "Empty")
        #expect(browser.actions == DeviceActions())

        var next = state.hubs[0].ports[1]
        next.number = port.port
        next.generation += 1
        state.hubs[0].ports[0] = next
        await agent.publish(state)
        try await waitUntil { model.state == state }
        #expect(browser.inspected?.id == port)
        #expect(browser.inspected?.port.ecid == next.ecid)
        let target = RestoreTarget(port: port.port, generation: next.generation, ecid: next.ecid)
        #expect(browser.actions.restart == [.reboot(hubs: [.init(hub: port.hub, session: 77, targets: [target])])])
    }

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
        let menu = NSHostingMenu(rootView: DeviceActionButtons(browser: browser, contextSelection: ids))
        menu.update()
        #expect(menu.items.filter { !$0.isSeparatorItem }.map(\.title) == ["Restart"])
        browser.selection = ids.union([state.devices[0].id])
        #expect(browser.actions == DeviceActions())
    }

    @Test func aPowerWarningPrecedesPlanningAndTheEraseConfirmation() async throws {
        var state = try Fixtures.state
        state.hubs[0].ports[1].restore = true
        state.hubs[0].restorePowerWarning = "Connect a power supply."
        var cart = state.hubs[0]
        cart.serial = "817a23bd502453b4"
        cart.name = "Cart"
        cart.restorePowerWarning = "Use a larger power supply."
        var spare = cart
        spare.serial = "spare"
        spare.name = "Spare"
        state.hubs += [cart, spare]
        let (model, agent) = try await started(state)
        let browser = DeviceBrowser(model: model)
        browser.selection = Set([state.devices[1].id, state.devices[3].id, state.hubDevices[1].devices[3].id])
        let hubs = try #require(browser.actions.restore?.hubs)
        let window = NSWindow(contentViewController: NSHostingController(rootView: DevicesView(browser: browser)))
        defer { window.orderOut(nil) }
        window.setFrameOrigin(NSPoint(x: -2000, y: -2000))
        window.orderBack(nil)

        let menu = NSHostingMenu(rootView: DeviceActionButtons(browser: browser))
        menu.update()
        let restore = menu.indexOfItem(withTitle: "Restore…")
        try #require(restore >= 0)
        menu.performActionForItem(at: restore)
        #expect(browser.restorePrompt?.message == "Repair Bench\nConnect a power supply.\n\nCart\nUse a larger power supply.")
        try await press("Cancel", in: window)
        try await waitUntil { browser.restorePrompt == nil && window.attachedSheet == nil }
        #expect(await agent.received.isEmpty)

        menu.performActionForItem(at: restore)
        try await press("Continue", in: window)
        try await waitUntil { browser.restorePrompt?.title == "Restore 3 Devices?" }
        #expect(await agent.received == [.plan(hubs: hubs)])
        try await press("Restore", in: window)
        try await waitUntil { await agent.received == [.plan(hubs: hubs), .restore(hubs: hubs, latest: false, after: model.restorePreferences.afterErase)] }
    }

    @Test(arguments: [false, true])
    func statusMenuReviveOpensTheWindowOnlyForAPowerWarning(warns: Bool) async throws {
        var state = try Fixtures.state
        state.hubs[0].ports[1].restore = true
        state.hubs[0].restorePowerWarning = warns ? "Connect a power supply." : nil
        let (model, agent) = try await started(state)
        let browser = DeviceBrowser(model: model)
        let window = NSWindow(contentViewController: NSHostingController(rootView: DevicesView(browser: browser)))
        defer { window.orderOut(nil) }
        window.setFrameOrigin(NSPoint(x: -2000, y: -2000))
        let status = StatusMenu(browser: browser, showDevices: { window.orderBack(nil) }, showSettings: {})
        status.menuNeedsUpdate(status.menu)
        let submenu = try #require(status.menu.item(withTitle: "MacBook Air")?.submenu)
        submenu.performActionForItem(at: submenu.indexOfItem(withTitle: "Revive"))
        #expect(window.isVisible == warns)
        if warns {
            #expect(await agent.received.isEmpty)
            try await press("Continue", in: window)
        }
        let hubs = try #require([state.devices[1]].selections)
        try await waitUntil { await agent.received == [.revive(hubs: hubs, latest: false)] }
        #expect(browser.restorePrompt == nil)
    }

    @Test func advanceSetupRunsImmediatelyForTheEntireEligibleSelection() async throws {
        var state = try Fixtures.state
        state.hubs[0].ports[1].advanceSetup = true
        state.hubs[0].ports[3].advanceSetup = true
        let (model, agent) = try await started(state)
        let browser = DeviceBrowser(model: model)
        let pair = [state.devices[1], state.devices[3]]
        browser.selection = Set(pair.map(\.id))
        let hubs = try #require(pair.selections)
        let context = NSHostingMenu(rootView: DeviceActionButtons(browser: browser, contextSelection: browser.selection))
        let commands = NSHostingMenu(rootView: DeviceActionButtons(browser: browser))
        context.update()
        commands.update()
        let title = "Advance Setup"
        let index = context.indexOfItem(withTitle: title)
        try #require(index >= 0 && context.item(at: index)?.isEnabled == true)
        #expect(commands.item(withTitle: title)?.isEnabled == true)
        #expect(await agent.received.isEmpty)
        context.performActionForItem(at: index)
        try await waitUntil { await agent.received == [.advanceSetup(hubs: hubs, setup: model.restorePreferences.setup)] }

        state.hubs[0].ports[3].advanceSetup = false
        await agent.publish(state)
        try await waitUntil { model.state == state }
        context.update()
        commands.update()
        #expect(context.item(withTitle: title) == nil)
        #expect(commands.item(withTitle: title)?.isEnabled == false)
        #expect(browser.actions.advanceSetup.isEmpty)
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
        let browser = DeviceBrowser(model: model)
        browser.showsInspector = false
        var opened = false
        let status = StatusMenu(browser: browser, showDevices: { opened = true }, showSettings: {})
        let menu = status.menu
        status.menuNeedsUpdate(menu)
        let mac = try #require(menu.item(withTitle: "MacBook Air"))
        let two = NSImage(systemSymbolName: "2.circle", accessibilityDescription: nil)?.accessibilityDescription
        #expect(mac.image?.accessibilityDescription == two)
        #expect(mac.subtitle == "DFU")
        let submenu = try #require(mac.submenu)
        let open = try #require(submenu.item(withTitle: "Open Device"))
        submenu.performActionForItem(at: submenu.index(of: open))
        #expect(opened && browser.showsInspector)
        #expect(browser.inspected?.port.number == 2)
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
        state.hubs[0].ports[2].run?.steps[0].progress?.percent = 84
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
        state.hubs[0].ports[1].device = "iPad Air"
        state.hubs[0].ports[1].advanceSetup = true
        let original = DeviceActions([state.devices[1]])
        let (model, agent) = try await started(state)
        let browser = DeviceBrowser(model: model)
        let status = StatusMenu(browser: browser, showDevices: {}, showSettings: {})
        let menu = status.menu
        status.menuNeedsUpdate(menu)
        let row = try #require(menu.item(withTitle: "iPad Air"))
        let submenu = try #require(row.submenu)
        let restart = submenu.indexOfItem(withTitle: "Restart")
        let info = submenu.indexOfItem(withTitle: "Open Device")
        let setup = submenu.indexOfItem(withTitle: "Advance Setup")
        let commands = [restart, info, setup]
        try #require(commands.allSatisfy { $0 >= 0 && submenu.item(at: $0)?.isEnabled == true })
        status.menuWillOpen(menu)
        submenu.performActionForItem(at: restart)
        try await waitUntil { await agent.received == original.restart }
        submenu.performActionForItem(at: setup)
        let beforeReplacement = original.restart + [.advanceSetup(hubs: original.advanceSetup, setup: model.restorePreferences.setup)]
        try await waitUntil { await agent.received == beforeReplacement }
        if reconnectedHub {
            state.hubs[0].session = 78
        } else {
            state.hubs[0].ports[1].generation += 1
        }
        state.hubs[0].ports[1].device = "iPhone SE"
        state.hubs[0].ports[1].ecid = "0000000000001234"
        await agent.publish(state)
        try await waitUntil { row.subtitle == "Disconnected" }
        #expect(commands.allSatisfy { submenu.item(at: $0)?.isEnabled == false })
        for index in commands {
            submenu.performActionForItem(at: index)
        }
        let replacement = try #require(menu.item(withTitle: "iPhone SE")?.submenu)
        let newActions = DeviceActions([state.devices[1]])
        replacement.performActionForItem(at: replacement.indexOfItem(withTitle: "Restart"))
        try await waitUntil { await agent.received == beforeReplacement + newActions.restart }
        replacement.performActionForItem(at: replacement.indexOfItem(withTitle: "Advance Setup"))
        try await waitUntil { await agent.received == beforeReplacement + newActions.restart + [.advanceSetup(hubs: newActions.advanceSetup, setup: model.restorePreferences.setup)] }
        #expect(browser.selection.isEmpty)
        status.menuDidClose(menu)
    }

    @Test func setupChoicesPersistLocallyAndAreCapturedWhenAnActionStarts() async throws {
        let file = URL.temporaryDirectory.appending(path: UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: file) }
        let agent = FakeAgent(reports: [])
        let settings = AgentModel(agent: agent, preferencesFile: file)
        let setup = SetupOptions(language: "en-AU", locale: "en_AU", enroll: true)
        settings.restorePreferences.setup = setup
        settings.restorePreferences.shutdownMacs = true

        let model = AgentModel(agent: agent, preferencesFile: file)
        let hubs = [RestoreSelection(hub: "a0e0", session: 1, targets: [.init(port: 1, generation: 1)])]
        #expect(await agent.received.isEmpty)
        model.advanceSetup(hubs)
        try await waitUntil { await agent.received == [.advanceSetup(hubs: hubs, setup: setup)] }
        model.restore(hubs, latest: false)
        try await waitUntil { await agent.received.last == .restore(hubs: hubs, latest: false, after: .init(shutdownMacs: true)) }
        model.restorePreferences.advanceAfterErase = true
        model.restore(hubs, latest: true)
        try await waitUntil { await agent.received.last == .restore(hubs: hubs, latest: true, after: .init(shutdownMacs: true, setup: setup)) }
    }

    private func started(_ state: AgentState) async throws -> (AgentModel, FakeAgent) {
        let agent = FakeAgent(reports: [state])
        let model = AgentModel(agent: agent)
        model.start()
        try await waitUntil { model.state != nil }
        return (model, agent)
    }

    private func press(_ title: String, in window: NSWindow) async throws {
        func button(in view: NSView?) -> NSButton? {
            guard let view else { return nil }
            if let button = view as? NSButton, button.title == title {
                return button
            }
            return view.subviews.lazy.compactMap { button(in: $0) }.first
        }
        try await waitUntil { button(in: window.attachedSheet?.contentView) != nil }
        let button = try #require(button(in: window.attachedSheet?.contentView))
        button.performClick(nil)
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
