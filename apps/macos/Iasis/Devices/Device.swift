import Foundation

/// One port of one hub and what is on it, as every view shows it.
struct Device: Identifiable, Equatable {
    struct ID: Hashable {
        // periphery:ignore
        let hub: String
        // periphery:ignore
        let port: Int
    }

    let hubTitle: String
    let serial: String
    let session: UInt32?
    let port: AgentState.Port
    let sharing: AgentState.Sharing.Port?
    /// What the host lacks for this device's restore.
    var needs: AgentState.Prerequisite?

    static let dfuPortHelp = "Connect the cable to the Mac’s DFU port."

    var sharesInternet: Bool {
        sharing?.active == true
    }

    var id: ID {
        ID(hub: serial, port: port.number)
    }

    /// An active run stays on its reserved port while the target reboots.
    var isSelectable: Bool {
        port.occupied || !port.enabled || run?.inProgress == true
    }

    var run: AgentState.OperationRun? {
        guard let run = port.run, run.inProgress || port.occupied && port.enabled else { return nil }
        if let expected = run.ecid, let attached = port.ecid, expected != attached {
            return nil
        }
        return run
    }

    var name: String {
        if !port.enabled {
            "Off"
        } else if !port.occupied, run?.inProgress == true {
            port.device ?? "Waiting for Device"
        } else if !port.occupied {
            "Empty"
        } else if let name = port.device {
            name
        } else if port.identifying {
            "Identifying…"
        } else {
            "Unknown Device"
        }
    }

    /// The name without its qualifiers: MacBook Air, not MacBook Air (M1, 2020).
    var shortName: String {
        name.components(separatedBy: " (").first ?? name
    }

    var symbol: String? {
        port.occupied ? DeviceSymbol.kind(of: port.device ?? "") : nil
    }

    var activity: Activity {
        if let run, run.inProgress, let activity = run.activity {
            return port.awaiting == nil ? activity : Activity("Waiting for Image", .working(nil), symbol: DeviceSymbol.download)
        }
        if let fault = port.fault {
            return Activity(fault, .failed)
        }
        guard port.occupied, port.enabled else { return .idle }
        if port.awaiting != nil {
            return Activity("Waiting for Image", .working(nil), symbol: DeviceSymbol.download)
        }
        if let job = port.job, job.phase == .preparing || job.phase == .running {
            return Activity(job.phase, operation: job.operation)
        }
        switch port.request {
        case .enteringDfu: return Activity("Entering DFU", .working(nil))
        case .restarting: return Activity("Restarting", .working(nil))
        case .dfuUnconfirmed: return Activity("DFU Not Confirmed", .uncertain)
        case nil: break
        }
        if port.waitingForPower {
            return Activity("Waiting for Power", .caution)
        }
        if let activity = run?.activity {
            return activity
        }
        if let job = port.job {
            return Activity(job.phase, operation: job.operation)
        }
        switch port.mode {
        case .dfu, .kisDfu: return Activity("DFU", .none)
        case .recovery: return Activity("Recovery", .none)
        default: return .idle
        }
    }

    var restoreTarget: RestoreTarget {
        RestoreTarget(port: port.number, generation: port.generation, ecid: port.ecid)
    }

    var canCheckSetup: Bool {
        port.enabled && port.occupied && port.usb == .apple && port.mode == .normal
            && port.ecid != nil && port.fault == nil && port.request == nil && port.awaiting == nil
            && port.job?.phase != .preparing && port.job?.phase != .running && run?.inProgress != true
    }
}

extension [Device] {
    /// The devices' attachments by hub, as the agent checks them before it
    /// acts on them; nil when a device's hub has no session.
    var selections: [RestoreSelection]? {
        var hubs: [RestoreSelection] = []
        for device in self {
            guard let session = device.session else { return nil }
            if let index = hubs.firstIndex(where: { $0.hub == device.serial }) {
                hubs[index].targets.append(device.restoreTarget)
            } else {
                hubs.append(RestoreSelection(hub: device.serial, session: session, targets: [device.restoreTarget]))
            }
        }
        return hubs
    }
}

/// What a port is doing, in words and as one mark.
struct Activity: Equatable {
    enum Mark: Equatable {
        case none, pending, skipped
        /// Under way, with the fraction done when it is known.
        case working(Double?)
        case done
        case failed
        case uncertain
        case caution
    }

    static let idle = Activity("", .none)

    let text: String
    let mark: Mark
    /// The work under way, in a ring that fills as it runs.
    let symbol: String

    init(_ phase: AgentState.RestorePhase, operation: AgentState.Operation) {
        switch phase {
        case .preparing: self.init("Preparing", .working(nil))
        case .running: self.init(operation.running, .working(nil))
        case .reported: self.init("\(operation.title) Complete", .done)
        case .failed: self.init("\(operation.title) Failed", .failed)
        case .unconfirmed: self.init("\(operation.title) Outcome Unknown", .uncertain)
        }
    }

    init(_ text: String, _ mark: Mark, symbol: String = "circle") {
        self.text = text
        self.mark = mark
        self.symbol = symbol
    }
}

/// A hub and its five ports.
struct HubDevices: Identifiable, Equatable {
    let hub: AgentState.Hub
    let title: String
    let devices: [Device]

    var id: String {
        hub.serial
    }
}

extension AgentState {
    var hubDevices: [HubDevices] {
        hubs.map { hub in
            let title = title(for: hub)
            return HubDevices(hub: hub, title: title, devices: hub.ports.map { port in
                let portSharing = sharing.ports.first { $0.hub == hub.serial && $0.port == port.number }
                let needs = missing.first { $0.id == port.needs }
                return Device(hubTitle: title, serial: hub.serial, session: hub.session, port: port, sharing: portSharing, needs: needs)
            })
        }
    }

    var devices: [Device] {
        hubDevices.flatMap(\.devices)
    }
}

/// An action is available when it applies to the whole selection.
struct DeviceActions: Equatable {
    var dfu: [AgentRequest] = []
    var restart: [AgentRequest] = []
    var revive: AgentRequest?
    var restore: RestoreConfirmation?

    init() {}

    init(_ devices: [Device]) {
        guard !devices.isEmpty, devices.allSatisfy(\.isSelectable) else { return }
        if let hubs = devices.selections {
            if devices.allSatisfy(\.port.dfu) {
                dfu = [.dfu(hubs: hubs)]
            }
            if devices.allSatisfy(\.port.reboot) {
                restart = [.reboot(hubs: hubs)]
            }
        }
        restore = RestoreConfirmation(devices)
        revive = restore.map { .revive(hubs: $0.hubs, latest: false) }
    }
}

struct SetupAction: Equatable {
    let hubs: [RestoreSelection]
    var settings: AgentState.SetupLanguage?
    var enroll: Bool

    static func menuCheck(for devices: [Device]) -> Self? {
        guard !devices.isEmpty, devices.allSatisfy(\.canCheckSetup), let hubs = devices.selections else { return nil }
        return Self(hubs: hubs, settings: nil, enroll: false)
    }
}

/// An erase the user is asked to confirm: the attachments as shown, on any
/// number of hubs, and what the agent says each would be restored with.
struct RestoreConfirmation: Equatable {
    /// One way to go on: its button and the request that makes.
    struct Choice: Equatable, Identifiable {
        let title: String
        let request: AgentRequest

        var id: String {
            title
        }
    }

    let devices: [Device]
    let hubs: [RestoreSelection]
    var planned: [AgentState.Planned] = []

    init?(_ devices: [Device]) {
        guard !devices.isEmpty,
              devices.allSatisfy({ $0.port.occupied && $0.port.restore }),
              let hubs = devices.selections
        else { return nil }
        self.devices = devices
        self.hubs = hubs
    }

    var title: String {
        let subject = devices.count == 1 ? devices[0].shortName : "\(devices.count) Devices"
        return blocked ? "Cannot Restore \(subject)" : "Restore \(subject)?"
    }

    var message: String {
        let erased = "All data on \(devices.count == 1 ? "this device" : "these devices") will be erased and the system software reinstalled."
        return ([erased, ""] + devices.map(line)).joined(separator: "\n")
    }

    /// Where an image in the store is older than the newest release, the
    /// choice between them. Otherwise the one way to go on, or none when a
    /// device has nothing to be restored with.
    var choices: [Choice] {
        guard !blocked else { return [] }
        let newer = devices.compactMap(offer).filter { $0.newer != nil }
        guard !newer.isEmpty else {
            return [Choice(title: "Restore", request: .restore(hubs: hubs, latest: false))]
        }
        let latest = Set(newer.compactMap(\.latest?.title))
        let held = Set(newer.compactMap(\.held?.title))
        return [
            Choice(
                title: latest.count == 1 ? "Download \(latest.first ?? "")" : "Download Latest",
                request: .restore(hubs: hubs, latest: true)
            ),
            Choice(
                title: held.count == 1 ? "Use \(held.first ?? "")" : "Use Downloaded",
                request: .restore(hubs: hubs, latest: false)
            ),
        ]
    }

    private func offer(for device: Device) -> AgentState.Offer? {
        planned.first { $0.hub == device.serial && $0.port == device.port.number }?.offer
    }

    /// A device has no image in the store and the catalog offers none.
    private var blocked: Bool {
        devices.compactMap(offer).contains { $0.held == nil && $0.latest == nil }
    }

    private func line(for device: Device) -> String {
        let place = "\(device.hubTitle), Port \(device.port.number): \(device.name)"
        // A device the agent has not identified gets its image once in DFU.
        guard let offer = offer(for: device) else { return place }
        let size = { (release: AgentState.Release) in release.bytes.formatted(.byteCount(style: .file)) }
        switch (offer.held, offer.latest) {
        case let (held?, latest?) where offer.newer != nil:
            return "\(place) · \(held.title) downloaded, \(latest.title) available (\(size(latest)))"
        case let (held?, latest):
            return "\(place) · \(held.title)\(latest == nil ? ", newer releases not checked" : "")"
        case let (nil, latest?):
            return "\(place) · \(latest.title), \(size(latest)) to download"
        case (nil, nil):
            return "\(place) · \(offer.problem ?? "No image restores this device")"
        }
    }
}

extension AgentState.PortPower {
    var title: String {
        source == .pd ? "PD Contract" : "USB Power"
    }

    var reading: String {
        let volts = scaled(millivolts, by: 1000)
        let current = (1 ..< 1000).contains(milliamps) ? "\(milliamps) mA" : "\(scaled(milliamps, by: 1000)) A"
        let watts = scaled(millivolts * milliamps, by: 1_000_000)
        return "\(volts) V \(current) (\(watts) W)"
    }
}

private func scaled(_ value: Int, by divisor: Int) -> String {
    (Decimal(value) / Decimal(divisor))
        .formatted(.number.grouping(.never).precision(.fractionLength(0 ... 6)))
}

func wholeWatts(_ milliwatts: Int) -> Int {
    (milliwatts + 500) / 1000
}
