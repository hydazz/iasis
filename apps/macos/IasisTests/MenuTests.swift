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

    @Test func theNativeMenuIdentifiesThePortAndKeepsItsCommandsOnThatAttachment() async throws {
        var state = try Fixtures.state
        let agent = FakeAgent(reports: [state])
        let model = AgentModel(agent: agent)
        model.start()
        for _ in 0 ..< 100 where model.menuSnapshot == nil {
            try await Task.sleep(for: .milliseconds(10))
        }
        let menu = NSHostingMenu(rootView: StatusMenu(browser: DeviceBrowser(model: model)))
        menu.update()
        let item = try #require(menu.items.first { $0.title == "2  MacBook Air" })
        let submenu = try #require(item.submenu)
        submenu.update()
        #expect(submenu.item(withTitle: "Restart")?.isEnabled == true)
        #expect(item.subtitle == "DFU")
        state.boards[0].ports[1].mode = .mux
        state.boards[0].ports[1].wrongDfuPort = true
        state.boards[0].ports[1].reboot = false
        await agent.publish(state)
        for _ in 0 ..< 100 where item.subtitle != "PORT" {
            try await Task.sleep(for: .milliseconds(10))
            menu.update()
        }
        #expect(item.subtitle == "PORT")
        #expect(menu.item(withTitle: "2  MacBook Air")?.submenu === submenu)
        submenu.update()
        #expect(submenu.item(withTitle: "Restart")?.isEnabled == false)
        #expect(item.badge == nil)
        let port = state.devices[1]
        state.boards[0].ports[1].generation += 1
        #expect(!port.sameAttachment(as: state.devices[1]))
    }
}
