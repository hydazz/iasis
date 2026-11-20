import AppKit
@testable import Iasis
import SwiftUI
import Testing

struct MenuTests {
    @Test func deviceItemsHaveSymbolsAndOpenTheirPortActions() async throws {
        let state = try Fixtures.state
        let agent = FakeAgent(reports: [state])
        let model = AgentModel(agent: agent)
        model.start()
        for _ in 0 ..< 100 where model.state == nil {
            try await Task.sleep(for: .milliseconds(10))
        }
        let menu = NSHostingMenu(rootView: MenuView(model: model))
        menu.update()
        let items = menu.items
        #expect(items.contains { $0.title == "Repair Bench" })
        let mac = try #require(items.first { $0.title == "2. MacBook Air · DFU" })
        #expect(mac.image != nil)
        let commands = try #require(mac.submenu)
        commands.update()
        let restart = commands.indexOfItem(withTitle: "Restart Device")
        try #require(restart >= 0)
        commands.performActionForItem(at: restart)
        for _ in 0 ..< 100 where await agent.received.isEmpty {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(await agent.received == [.reboot(board: "a0e012192cdfdb67", port: 2)])
        #expect(commands.items.contains { $0.title == "Erase and Restore…" })
        #expect(!items.contains { $0.title.contains("Empty") })
    }

    @Test func duplicateNamesAndDisabledPortsRemainAddressable() throws {
        var state = try Fixtures.state
        var other = state.boards[0]
        other.serial = "817a23bd502453b4"
        other.name = other.name.uppercased()
        other.ports[0].enabled = false
        state.boards.append(other)
        #expect(state.title(for: state.boards[0]) == "Repair Bench · db67")
        #expect(state.title(for: other) == "REPAIR BENCH · 53b4")
        #expect(other.visiblePorts.first?.number == 1)
    }
}
