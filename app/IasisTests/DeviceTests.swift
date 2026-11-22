import Foundation
@testable import Iasis
import Testing

struct DeviceTests {
    @Test func internetSharingFollowsTheActivePortOnItsBoard() throws {
        var state = try Fixtures.state
        var other = state.boards[0]
        other.serial = "817a23bd502453b4"
        state.boards.append(other)

        #expect(state.devices.filter(\.sharesInternet).map(\.id) == [
            Device.ID(board: "a0e012192cdfdb67", port: 3),
        ])
        #expect(state.devices[3].sharing?.problem == "Cannot open USB interface")
        #expect(state.devices[8].sharing?.problem == nil)
        state.sharing.ports = []
        #expect(state.devices.filter(\.sharesInternet).isEmpty)
    }

    @Test func everyPortIsARowThatKeepsItsPlace() throws {
        var state = try Fixtures.state
        let devices = state.devices
        #expect(devices.map(\.port.number) == [1, 2, 3, 4, 5])
        #expect(devices.map(\.name) == ["Empty", "MacBook Air (13-inch, M5)", "iPad", "Unknown Device", "Unknown Device"])
        #expect(devices.map(\.isSelectable) == [false, true, true, true, true])
        #expect(devices[1].symbol == "laptopcomputer")
        #expect(devices[0].symbol == nil)

        // Progress, a new attachment and another device leave the row where it was.
        state.boards[0].ports[2].stage?.percent = 44
        state.boards[0].ports[3].generation += 1
        state.boards[0].ports[3].ecid = "0000000000001234"
        #expect(state.devices.map(\.id) == devices.map(\.id))

        state.boards[0].ports[0].enabled = false
        #expect(state.devices[0].name == "Off")
        #expect(state.devices[0].isSelectable)
    }

    @Test func aPeripheralWithNoAppleModeIsAnOrdinaryDevice() throws {
        var state = try Fixtures.state
        state.boards[0].ports[0].occupied = true
        state.boards[0].ports[0].identifying = true
        #expect(state.devices[0].name == "Identifying…")
        state.boards[0].ports[0].identifying = false
        #expect(state.devices[0].name == "Unknown Device")
        state.boards[0].ports[0].link = .usb2
        #expect(state.devices[0].name == "Unknown Device")
        state.boards[0].ports[0].device = "USB JTAG/serial debug unit"
        #expect(state.devices[0].name == "USB JTAG/serial debug unit")
        #expect(state.devices[0].activity == .idle)
    }

    @Test func precisePowerReadingsDistinguishAnAllowanceFromAContract() {
        let usb = AgentState.PortPower(source: .usb, millivolts: 5000, milliamps: 900)
        #expect(usb.title == "USB Power")
        #expect(usb.reading == "5 V 900 mA (4.5 W)")
        var pd = AgentState.PortPower(source: .pd, millivolts: 20000, milliamps: 780)
        #expect(pd.title == "PD Contract")
        #expect(pd.reading == "20 V 780 mA (15.6 W)")
        pd.milliamps = 2250
        #expect(pd.reading == "20 V 2.25 A (45 W)")
        pd.milliamps = 0
        #expect(pd.reading == "20 V 0 A (0 W)")
    }

    @Test func aPortSaysWhatItIsDoingInWordsAndOneMark() throws {
        var state = try Fixtures.state
        let activity = { (state: AgentState, port: Int) in state.devices[port - 1].activity }
        #expect(activity(state, 1) == .idle)
        #expect(activity(state, 2) == Activity("DFU", .none))
        #expect(activity(state, 3) == Activity("Sending Filesystem 43%", .working(0.43)))
        #expect(activity(state, 5) == Activity("Protection tripped", .failed))
        state.boards[0].ports[4].enabled = false
        #expect(activity(state, 5) == Activity("Protection tripped", .failed))

        #expect(activity(state, 4) == Activity("Restore Complete", .done))
        state.boards[0].ports[3].result?.phase = .failed
        #expect(activity(state, 4) == Activity("Restore Failed", .failed))
        state.boards[0].ports[3].result = nil
        #expect(activity(state, 4) == .idle)

        state.boards[0].ports[1].mode = nil
        #expect(activity(state, 2) == .idle)
        state.boards[0].ports[1].request = .restarting
        #expect(activity(state, 2) == Activity("Restarting", .working(nil)))
    }

    @Test func aRestorePinsEveryAttachmentAcrossBoards() throws {
        var state = try Fixtures.state
        state.boards[0].ports[1].restore = true
        var other = state.boards[0]
        other.serial = "817a23bd502453b4"
        other.name = "Cart"
        other.session = 9
        state.boards.append(other)

        let chosen = state.devices.filter { [2, 4].contains($0.port.number) }
        let actions = DeviceActions(chosen, restoreProblem: nil)
        let confirmation = try #require(actions.restore)
        #expect(confirmation.title == "Restore 4 Devices?")
        #expect(confirmation.message.contains("Repair Bench, Port 2: MacBook Air (13-inch, M5)"))
        #expect(confirmation.message.contains("Cart, Port 4: Unknown Device"))
        let targets = [
            RestoreTarget(port: 2, generation: 2, ecid: "001a79d10e6bc01c"),
            RestoreTarget(port: 4, generation: 4, ecid: "0002250e3c04c01e"),
        ]
        #expect(confirmation.request == .restore(boards: [
            RestoreSelection(board: "a0e012192cdfdb67", session: 77, targets: targets),
            RestoreSelection(board: "817a23bd502453b4", session: 9, targets: targets),
        ]))

        // One device that cannot be restored withholds the erase from all.
        #expect(DeviceActions(state.devices.filter { [3, 4].contains($0.port.number) }, restoreProblem: nil).restore == nil)
        #expect(DeviceActions(chosen, restoreProblem: "idevicerestore is missing").restore == nil)
        state.boards[1].session = nil
        #expect(DeviceActions(state.devices.filter { [2, 4].contains($0.port.number) }, restoreProblem: nil).restore == nil)
        #expect(RestoreConfirmation([state.devices[1]])?.title == "Restore MacBook Air?")
    }

    @Test func actionsGoToTheDevicesThatTakeThem() throws {
        var state = try Fixtures.state
        let board = "a0e012192cdfdb67"
        let all = DeviceActions(state.devices.filter(\.isSelectable), restoreProblem: nil)
        #expect(all.dfu == [.dfu(board: board, port: 4)])
        #expect(all.restart == [.reboot(board: board, port: 2), .reboot(board: board, port: 4)])
        #expect(all.revive == nil)
        #expect(all.needsConfigurator)

        state.boards[0].ports[1].revive = true
        let mac = DeviceActions([state.devices[1]], restoreProblem: nil)
        #expect(mac.revive == .revive(board: board, session: 77, port: 2, generation: 2, ecid: "001a79d10e6bc01c"))
    }

    @Test func boardsWithOneNameAreToldApart() throws {
        var state = try Fixtures.state
        var other = state.boards[0]
        other.serial = "817a23bd502453b4"
        other.name = other.name.uppercased()
        state.boards.append(other)
        #expect(state.boardDevices.map(\.title) == ["Repair Bench · db67", "REPAIR BENCH · 53b4"])
    }
}
