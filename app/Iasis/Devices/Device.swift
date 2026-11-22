import Foundation

/// One port of one board and what is on it, as every view shows it.
struct Device: Identifiable, Equatable {
    struct ID: Hashable {
        // periphery:ignore
        let board: String
        // periphery:ignore
        let port: Int
    }

    let boardTitle: String
    let serial: String
    let session: UInt32?
    let port: AgentState.Port
    let sharing: AgentState.Sharing.Port?

    static let dfuPortHelp = "Connect the cable to the Mac’s DFU port."

    var sharesInternet: Bool {
        sharing?.active == true
    }

    var id: ID {
        ID(board: serial, port: port.number)
    }

    /// The port holds a device or is switched off; an empty port offers nothing.
    var isSelectable: Bool {
        port.occupied || !port.enabled
    }

    var name: String {
        if !port.enabled {
            "Off"
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
        guard port.occupied else { return nil }
        let name = (port.device ?? "").lowercased()
        let kinds = [
            ("ipad", "ipad"),
            ("iphone", "iphone"),
            ("macbook", "laptopcomputer"),
            ("mac mini", "macmini"),
            ("mac studio", "macstudio"),
            ("mac pro", "macpro.gen3"),
            ("imac", "desktopcomputer"),
            ("mac", "desktopcomputer"),
            ("homepod mini", "homepodmini"),
            ("homepod", "homepod"),
            ("audioaccessory", "homepod"),
            ("apple tv", "appletv"),
            ("appletv", "appletv"),
        ]
        return kinds.first { name.hasPrefix($0.0) }?.1 ?? "questionmark"
    }

    var activity: Activity {
        if let fault = port.fault {
            return Activity(fault, .failed)
        }
        guard port.occupied, port.enabled else { return .idle }
        if let stage = port.stage {
            let fraction = stage.percent.map { Double($0) / 100 }
            return Activity(stage.percent.map { "\(stage.name) \($0)%" } ?? stage.name, .working(fraction))
        }
        if let job = port.job {
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
        if let result = port.result {
            return Activity(result.phase, operation: result.operation)
        }
        switch port.mode {
        case "dfu", "kis_dfu": return Activity("DFU", .none)
        case "recovery": return Activity("Recovery", .none)
        default: return .idle
        }
    }

    var restoreTarget: RestoreTarget {
        RestoreTarget(port: port.number, generation: port.generation, ecid: port.ecid)
    }
}

/// What a port is doing, in words and as one mark.
struct Activity: Equatable {
    enum Mark: Equatable {
        case none
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

    init(_ phase: AgentState.RestorePhase, operation: AgentState.Operation) {
        switch phase {
        case .preparing: self.init("Preparing", .working(nil))
        case .running: self.init(operation == .revive ? "Reviving" : "Restoring", .working(nil))
        case .reported: self.init("\(operation.title) Complete", .done)
        case .failed: self.init("\(operation.title) Failed", .failed)
        case .unconfirmed: self.init("\(operation.title) Outcome Unknown", .uncertain)
        }
    }

    init(_ text: String, _ mark: Mark) {
        self.text = text
        self.mark = mark
    }
}

/// A board and its five ports.
struct BoardDevices: Identifiable, Equatable {
    let board: AgentState.Board
    let title: String
    let devices: [Device]

    var id: String {
        board.serial
    }
}

extension AgentState {
    var boardDevices: [BoardDevices] {
        boards.map { board in
            let title = title(for: board)
            return BoardDevices(board: board, title: title, devices: board.ports.map { port in
                let portSharing = sharing.ports.first { $0.board == board.serial && $0.port == port.number }
                return Device(boardTitle: title, serial: board.serial, session: board.session, port: port, sharing: portSharing)
            })
        }
    }

    var devices: [Device] {
        boardDevices.flatMap(\.devices)
    }
}

/// DFU and restart target eligible devices. Restore and revive require the whole selection.
struct DeviceActions: Equatable {
    var dfu: [AgentRequest] = []
    var restart: [AgentRequest] = []
    var revive: AgentRequest?
    var restore: RestoreConfirmation?

    init() {}

    init(_ devices: [Device]) {
        dfu = devices.filter(\.port.dfu).map { .dfu(board: $0.serial, port: $0.port.number) }
        restart = devices.filter(\.port.reboot).map { .reboot(board: $0.serial, port: $0.port.number) }
        restore = RestoreConfirmation(devices)
        if case let .restore(boards)? = restore?.request {
            revive = .revive(boards: boards)
        }
    }
}

/// An erase the user is asked to confirm: the attachments as shown, on any
/// number of boards.
struct RestoreConfirmation: Equatable {
    let title: String
    let message: String
    let request: AgentRequest

    init?(_ devices: [Device]) {
        guard let first = devices.first,
              devices.allSatisfy({ $0.port.occupied && $0.port.restore && $0.session != nil })
        else { return nil }
        var boards: [RestoreSelection] = []
        for device in devices {
            guard let session = device.session else { return nil }
            if let index = boards.firstIndex(where: { $0.board == device.serial }) {
                boards[index].targets.append(device.restoreTarget)
            } else {
                boards.append(RestoreSelection(board: device.serial, session: session, targets: [device.restoreTarget]))
            }
        }
        let single = devices.count == 1
        title = single ? "Restore \(first.shortName)?" : "Restore \(devices.count) Devices?"
        let erased = "All data on \(single ? "this device" : "these devices") will be erased and the system software reinstalled."
        let list = devices.map { "\($0.boardTitle), Port \($0.port.number): \($0.name)" }
        message = ([erased, ""] + list).joined(separator: "\n")
        request = .restore(boards: boards)
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
