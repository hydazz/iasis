import AppKit
@testable import Iasis
import SwiftUI
import Testing

struct DevicesWindowTests {
    @Test func progressKeepsTheSelectedRowsInTheDeviceTable() async throws {
        var state = try Fixtures.state
        let agent = FakeAgent(reports: [state])
        let model = AgentModel(agent: agent)
        model.start()
        let host = NSHostingView(rootView: DevicesView(model: model))
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 720, height: 420), styleMask: [.titled], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        defer { window.close() }
        window.setFrameOrigin(NSPoint(x: -10000, y: -10000))
        window.contentView = host
        window.orderBack(nil)
        for _ in 0 ..< 100 where table(in: host)?.numberOfRows != 5 {
            try await Task.sleep(for: .milliseconds(10))
        }
        let list = try #require(table(in: host))
        #expect(list.numberOfRows == 5)
        let indices = IndexSet([1, 3])
        list.selectRowIndexes(indices, byExtendingSelection: false)
        #expect(list.selectedRowIndexes == indices)
        state.boards[0].ports[2].stage = .init(name: "Sending filesystem", percent: 23)
        await agent.publish(state)
        for _ in 0 ..< 100 where model.state != state {
            try await Task.sleep(for: .milliseconds(10))
        }
        host.layoutSubtreeIfNeeded()
        #expect(model.state == state)
        #expect(table(in: host) === list)
        #expect(list.selectedRowIndexes == indices)
    }

    private func table(in view: NSView) -> NSTableView? {
        if let table = view as? NSTableView {
            return table
        }
        return view.subviews.lazy.compactMap { table(in: $0) }.first
    }
}
