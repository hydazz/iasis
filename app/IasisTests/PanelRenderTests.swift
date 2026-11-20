import AppKit
import Foundation
@testable import Iasis
import SwiftUI
import Testing

struct PanelRenderTests {
    /// Lay the panel out as the menu bar would, and keep a picture of it
    /// where `IASIS_RENDER_DIR` says.
    @Test func thePanelLaysOutEveryPortCondition() async throws {
        let model = try AgentModel(agent: FakeAgent(reports: [Fixtures.state]))
        model.start()
        for _ in 0 ..< 200 where model.state == nil {
            try await Task.sleep(for: .milliseconds(10))
        }
        model.askToRestore(PortID(board: "a0e012192cdfdb67", port: 2))

        let panel = MenuView(model: model).background(Color(nsColor: .windowBackgroundColor))
        let host = NSHostingView(rootView: panel)
        host.appearance = NSAppearance(named: .aqua)
        host.frame = NSRect(origin: .zero, size: host.fittingSize)
        let window = NSWindow(contentRect: host.frame, styleMask: [.borderless], backing: .buffered, defer: false)
        window.contentView = host
        host.layoutSubtreeIfNeeded()
        #expect(host.fittingSize.width == 340)
        #expect(host.fittingSize.height > 150)

        guard let directory = ProcessInfo.processInfo.environment["IASIS_RENDER_DIR"] else {
            return
        }
        let bitmap = try #require(host.bitmapImageRepForCachingDisplay(in: host.bounds))
        host.cacheDisplay(in: host.bounds, to: bitmap)
        let picture = try #require(bitmap.representation(using: .png, properties: [:]))
        try picture.write(to: URL(filePath: directory).appending(path: "panel.png"))
    }
}
