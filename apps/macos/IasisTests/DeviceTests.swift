import Foundation
@testable import Iasis
import Testing

struct DeviceTests {
    @Test func internetSharingFollowsTheActivePortOnItsHub() throws {
        var state = try Fixtures.state
        var other = state.hubs[0]
        other.serial = "817a23bd502453b4"
        state.hubs.append(other)

        #expect(state.devices.filter(\.sharesInternet).map(\.id) == [
            Device.ID(hub: "a0e012192cdfdb67", port: 3),
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
        #expect(devices[1].symbol == "laptopcomputer")
        #expect(devices[0].symbol == nil)

        // Progress, a new attachment and another device leave the row where it was.
        state.hubs[0].ports[2].run?.steps[0].progress?.percent = 44
        state.hubs[0].ports[3].generation += 1
        state.hubs[0].ports[3].ecid = "0000000000001234"
        #expect(state.devices.map(\.id) == devices.map(\.id))

        state.hubs[0].ports[0].enabled = false
        #expect(state.devices[0].name == "Off")
    }

    @Test func aPeripheralWithNoAppleModeIsAnOrdinaryDevice() throws {
        var state = try Fixtures.state
        state.hubs[0].ports[0].occupied = true
        state.hubs[0].ports[0].identifying = true
        #expect(state.devices[0].name == "Identifying…")
        state.hubs[0].ports[0].identifying = false
        #expect(state.devices[0].name == "Unknown Device")
        state.hubs[0].ports[0].link = .usb2
        #expect(state.devices[0].name == "Unknown Device")
        state.hubs[0].ports[0].device = "USB JTAG/serial debug unit"
        #expect(state.devices[0].name == "USB JTAG/serial debug unit")
        #expect(state.devices[0].activity == .idle)
    }

    @Test func precisePowerReadingsDistinguishAnAllowanceFromAContract() {
        let usb = AgentState.PortPower(source: .usb, millivolts: 5000, milliamps: 900)
        #expect(usb.title == "USB Allowance")
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
        #expect(activity(state, 3) == Activity("Restoring 43%", .working(0.43), symbol: "arrow.counterclockwise.circle"))
        #expect(activity(state, 5) == Activity("Protection tripped", .failed))
        state.hubs[0].ports[4].enabled = false
        #expect(activity(state, 5) == Activity("Protection tripped", .failed))

        #expect(activity(state, 4) == Activity("Restore Complete", .done))
        state.hubs[0].ports[3].run?.steps[0].state = .failed
        #expect(activity(state, 4) == Activity("Restore Failed", .failed))
        state.hubs[0].ports[3].run = nil
        #expect(activity(state, 4) == .idle)

        state.hubs[0].ports[1].mode = nil
        #expect(activity(state, 2) == .idle)
        state.hubs[0].ports[1].request = .restarting
        #expect(activity(state, 2) == Activity("Restarting", .working(nil)))
    }

    @Test func onlyTheActiveInstallReportsItsPercentAndActivity() throws {
        var state = try Fixtures.state
        var run = try #require(state.devices[2].port.run)
        #expect(run.steps.map(\.kind) == [.restore, .setRegion, .activate, .requestEnrollment])
        #expect(run.steps[0].detail == "iPadOS 27.2.1 (24B156)")
        #expect(run.steps[1].detail?.contains("en-AU") == true)
        #expect(run.steps[0].activeInstallProgress?.percent == 43)
        #expect(run.steps[0].activeInstallProgress?.activityCaption == "Sending filesystem…")
        run.steps[0].progress?.name = "Sending Image"
        #expect(run.steps[0].activeInstallProgress?.activityCaption == "Sending image…")
        run.steps[0].state = .failed
        #expect(run.steps[0].activeInstallProgress == nil)
        run.steps[0].state = .complete
        #expect(run.steps[0].activeInstallProgress == nil)
        run.steps[1].state = .active
        state.hubs[0].ports[2].run = run
        #expect(state.devices[2].activity == Activity("Setting Language and Region", .working(nil)))
        run.steps[1].state = .failed
        run.steps[1].error = "The device does not support this region."
        run.steps[2].state = .skipped
        run.steps[3].state = .skipped
        #expect(run.activity == Activity("Set Region Failed", .failed))

        run.steps = [.init(kind: .setRegion, state: .complete), .init(kind: .activate, state: .active)]
        #expect(run.activity == Activity("Activating", .working(nil)))
        run.steps = [.init(kind: .setRegion, state: .skipped), .init(kind: .activate, state: .skipped)]
        #expect(run.activity == Activity("Already Set Up", .done))

        run.steps = [.init(kind: .requestEnrollment, state: .active)]
        state.hubs[0].ports[3].run = run
        state.hubs[0].ports[3].mode = .normal
        #expect(state.devices[3].activity == Activity("Requesting Enrollment", .working(nil)))
        run.steps[0].state = .complete
        #expect(run.activity == Activity("Enrollment Requested", .done))
        run.steps[0].state = .unconfirmed
        #expect(run.activity == Activity("Request Enrollment Unconfirmed", .uncertain))
        run.steps = [.init(kind: .setRegion, state: .complete), .init(kind: .requestEnrollment, state: .onDevice, error: "Sign in to continue enrollment.")]
        #expect(!run.inProgress)
        #expect(run.activity == Activity("Continue on Device", .onDevice))
        #expect(run.steps.last?.error == "Sign in to continue enrollment.")
    }

    @Test func downloadsCountTheDevicesWaitingForThatImage() throws {
        var state = try Fixtures.state
        state.hubs[0].ports[2].awaiting = state.images.fetch?.id
        #expect(state.devices[2].activity == Activity("Waiting for Image", .working(nil), symbol: DeviceSymbol.download))
        #expect(state.waitingForDownload == 1)
        state.hubs[0].ports[3].awaiting = state.images.fetch?.id
        state.hubs[0].ports[4].awaiting = "UniversalMac_27.2.1_26B131_Restore.ipsw"
        #expect(state.waitingForDownload == 2)
    }

    @Test func anActiveRunFollowsThePortAcrossReboots() throws {
        var state = try Fixtures.state
        state.hubs[0].ports[2].occupied = false
        state.hubs[0].ports[2].device = nil
        state.hubs[0].ports[2].job = nil
        let waiting = state.devices[2]
        #expect(waiting.name == "Waiting for Device")
        #expect(waiting.activity.text == "Restoring 43%")

        state.hubs[0].ports[2].run = state.hubs[0].ports[3].run
        #expect(state.devices[2].run == nil)
        #expect(state.devices[2].activity == .idle)
        state.hubs[0].ports[2].occupied = true
        state.hubs[0].ports[2].ecid = "0000000000001234"
        #expect(state.devices[2].run == nil)
    }

    @Test func aRestorePinsEveryAttachmentAcrossHubs() throws {
        var state = try Fixtures.state
        state.hubs[0].ports[1].restore = true
        var other = state.hubs[0]
        other.serial = "817a23bd502453b4"
        other.name = "Cart"
        other.session = 9
        state.hubs.append(other)

        let chosen = state.devices.filter { [2, 4].contains($0.port.number) }
        let actions = DeviceActions(chosen)
        let confirmation = try #require(actions.restore)
        #expect(confirmation.title == "Restore 4 Devices?")
        #expect(confirmation.message.contains("Repair Bench, Port 2: MacBook Air (13-inch, M5)"))
        #expect(confirmation.message.contains("Cart, Port 4: Unknown Device"))
        let targets = [
            RestoreTarget(port: 2, generation: 2, ecid: "001a79d10e6bc01c"),
            RestoreTarget(port: 4, generation: 4, ecid: "0002250e3c04c01e"),
        ]
        let hubs = [
            RestoreSelection(hub: "a0e012192cdfdb67", session: 77, targets: targets),
            RestoreSelection(hub: "817a23bd502453b4", session: 9, targets: targets),
        ]
        #expect(confirmation.hubs == hubs)
        // With nothing to choose between, there is one way to go on.
        #expect(confirmation.choices == [.init(title: "Restore", latest: false)])

        // One device that cannot be restored withholds the erase from all.
        #expect(DeviceActions(state.devices.filter { [3, 4].contains($0.port.number) }).restore == nil)
        state.hubs[1].session = nil
        #expect(DeviceActions(state.devices.filter { [2, 4].contains($0.port.number) }).restore == nil)
        #expect(RestoreConfirmation([state.devices[1]])?.title == "Restore MacBook Air?")
    }

    @Test func aRestoreAsksWhichImageOnlyWhenTheStoreHasAnOlderOne() throws {
        var state = try Fixtures.state
        state.hubs[0].ports[1].restore = true
        var confirmation = try #require(RestoreConfirmation(state.devices.filter { [2, 4].contains($0.port.number) }))
        let hub = "a0e012192cdfdb67"
        let release = { (version: String, build: String) in
            AgentState.Release(os: "macOS", version: version, build: build, bytes: 25_600_000_000)
        }
        let newest = release("27.2.1", "26B131")
        let older = release("26.6", "25G80")

        // The newest is in the store, or nothing is: no question.
        confirmation.planned = [
            .init(hub: hub, port: 2, offer: .init(latest: newest, held: newest)),
            .init(hub: hub, port: 4, offer: .init(latest: newest)),
        ]
        #expect(confirmation.choices.map(\.title) == ["Restore"])
        #expect(confirmation.message.contains("Port 2: MacBook Air (13-inch, M5) · macOS 27.2.1\n"))
        #expect(confirmation.message.hasSuffix("Port 4: Unknown Device · macOS 27.2.1, 25.6 GB to download"))

        // An older image in the store is a choice against the newest release.
        confirmation.planned[0].offer = .init(latest: newest, held: older)
        #expect(confirmation.choices == [
            .init(title: "Download macOS 27.2.1", latest: true),
            .init(title: "Use macOS 26.6", latest: false),
        ])
        #expect(confirmation.message.contains("macOS 26.6 downloaded, macOS 27.2.1 available (25.6 GB)"))
        // A build is compared as a number too, and a newer image in the store is no choice.
        #expect(AgentState.Offer(latest: release("27.2.1", "26B131"), held: release("27.2.1", "26B99")).newer != nil)
        #expect(AgentState.Offer(latest: older, held: newest).newer == nil)

        // The catalog unreachable: the image in the store serves, and a
        // device with none cannot be restored.
        confirmation.planned[0].offer = .init(held: older, problem: "no route")
        confirmation.planned[1].offer = nil
        #expect(confirmation.choices.map(\.title) == ["Restore"])
        #expect(confirmation.message.contains("macOS 26.6, newer releases not checked"))
        confirmation.planned[1].offer = .init(problem: "asking the image catalog: no route")
        #expect(confirmation.choices.isEmpty)
        #expect(confirmation.title == "Cannot Restore 2 Devices")
        #expect(confirmation.message.hasSuffix("asking the image catalog: no route"))
    }

    @Test func actionsRequireEverySelectedDevice() throws {
        var state = try Fixtures.state
        let hub = "a0e012192cdfdb67"
        let all = DeviceActions(state.devices)
        let mac = RestoreTarget(port: 2, generation: 2, ecid: "001a79d10e6bc01c")
        let other = RestoreTarget(port: 4, generation: 4, ecid: "0002250e3c04c01e")
        #expect(all.dfu.isEmpty)
        #expect(all.restart.isEmpty)
        #expect(all.restore == nil)
        let pair = DeviceActions([state.devices[1], state.devices[3]])
        #expect(pair.dfu.isEmpty)
        #expect(pair.restart == [.reboot(hubs: [RestoreSelection(hub: hub, session: 77, targets: [mac, other])])])
        #expect(DeviceActions([state.devices[3]]).dfu == [.dfu(hubs: [RestoreSelection(hub: hub, session: 77, targets: [other])])])

        state.hubs[0].ports[1].restore = true
        let one = DeviceActions([state.devices[1]])
        #expect(one.restore?.hubs == [RestoreSelection(hub: hub, session: 77, targets: [mac])])
    }

    @Test func hubsWithOneNameAreToldApart() throws {
        var state = try Fixtures.state
        var other = state.hubs[0]
        other.serial = "817a23bd502453b4"
        other.name = other.name.uppercased()
        state.hubs.append(other)
        #expect(state.hubDevices.map(\.title) == ["Repair Bench · db67", "REPAIR BENCH · 53b4"])
    }
}
