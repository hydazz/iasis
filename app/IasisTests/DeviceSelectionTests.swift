import Foundation
@testable import Iasis
import Testing

struct DeviceSelectionTests {
    @Test func restorePinsEachSelectedAttachmentAndNamesEveryDevice() throws {
        var state = try Fixtures.state
        state.boards[0].ports[1].restore = true
        let devices = state.boards[0].devices
        let ids = Set(devices.filter { [2, 4].contains($0.port.number) }.map(\.id))
        let prompt = try #require(DeviceSelection(ids, state: state).restore)
        let board = state.boards[0]
        #expect(prompt.message.contains("Repair Bench"))
        #expect(prompt.message.contains("Port 2 · MacBook Air (13-inch, M5)"))
        #expect(prompt.message.contains("Port 4 · Unidentified Device"))
        #expect(prompt.request == .restore(board: board.serial, session: 77, targets: [
            RestoreTarget(port: 2, generation: 2, ecid: "001a79d10e6bc01c"),
            RestoreTarget(port: 4, generation: 4),
        ]))

        state.boards[0].ports[1].ecid = "0000000000001234"
        #expect(DeviceSelection(ids, state: state).restore == nil)
        #expect(RestoreConfirmation(board: state.boards[0], title: "Bench", ports: [board.ports[1]]) == nil)
        state.boards[0] = board
        state.boards[0].ports[1].generation += 1
        #expect(DeviceSelection(ids, state: state).restore == nil)
        state.boards[0] = board
        state.boards[0].session = 78
        #expect(DeviceSelection(ids, state: state).restore == nil)
    }

    @Test func aSelectionRequiresEveryDeviceToBeEligibleAndOnOneBoard() throws {
        var state = try Fixtures.state
        let board = state.boards[0]
        let mixed = Set(board.devices.filter { [2, 4].contains($0.port.number) }.map(\.id))
        #expect(DeviceSelection(mixed, state: state).restore == nil)
        var other = board
        other.serial = "0123456789abcdef"
        state.boards.append(other)
        let ids = Set(state.boards.flatMap(\.devices).filter { $0.port.number == 4 }.map(\.id))
        #expect(DeviceSelection(ids, state: state).restore == nil)
    }

    @Test func aLastSeenDeviceCannotIdentifyTheCurrentRestoreTarget() throws {
        var board = try Fixtures.state.boards[0]
        board.ports[3].lastSeen = .init(device: "iPad", ecid: "001a79d10e6bc01c")
        let port = board.ports[3]
        let prompt = try #require(RestoreConfirmation(board: board, title: "Bench", ports: [port]))
        #expect(!prompt.message.contains("iPad"))
        #expect(prompt.request == .restore(board: board.serial, session: 77, targets: [.init(port: 4, generation: 4)]))
    }

    @Test func aPortKeepsTheWorkerResultAndLogAfterItsDeviceLeaves() throws {
        var board = try Fixtures.state.boards[0]
        board.ports[3].occupied = false
        board.ports[3].state = "empty"
        let row = try #require(board.devices.first { $0.port.number == 4 })
        #expect(row.status == "Restore complete")
        #expect(row.port.result?.ecid == "0002250e3c04c01e")
        #expect(row.port.result?.logDirectory == "/tmp/iasis-logs/repair")
        #expect(RestoreConfirmation(board: board, title: "Bench", ports: [row.port]) == nil)
        #expect(!board.devices.contains { $0.port.number == 1 })
    }
}
