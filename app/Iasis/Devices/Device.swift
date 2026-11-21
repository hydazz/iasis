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
        } else {
            port.device ?? "Unidentified Device"
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
        return kinds.first { name.hasPrefix($0.0) }?.1 ?? "questionmark.square.dashed"
    }

    /// The last operation, while its device is still the one on the port.
    var result: AgentState.Ending? {
        guard port.occupied, let result = port.result else { return nil }
        return result.ecid == nil || result.ecid == port.ecid ? result : nil
    }

    var activity: Activity {
        guard port.occupied, port.enabled else { return .idle }
        if let stage = port.stage {
            let fraction = stage.percent.map { Double($0) / 100 }
            return Activity(stage.percent.map { "\(stage.name) \($0)%" } ?? stage.name, .working(fraction))
        }
        switch port.state {
        case "fault": return Activity(port.fault ?? "Fault", .failed)
        case "preparing": return Activity("Preparing", .working(nil))
        case "restoring": return Activity("Restoring", .working(nil))
        case "reviving": return Activity("Reviving", .working(nil))
        case "downloading image": return Activity("Downloading image", .working(nil))
        case "DFU sent": return Activity("Entering DFU", .working(nil))
        case "reboot sent": return Activity("Restarting", .working(nil))
        case "identifying": return Activity("Identifying", .working(nil))
        case "attaching": return Activity("Connecting", .working(nil))
        case "DFU unverified": return Activity("DFU not confirmed", .uncertain)
        case "wrong port": return Activity("Not a DFU port", .caution)
        case "waiting for power": return Activity("Waiting for power", .caution)
        default: break
        }
        if let result {
            let operation = result.operation.title
            switch result.phase {
            case .reported: return Activity("\(operation) complete", .done)
            case .failed: return Activity("\(operation) failed", .failed)
            case .unconfirmed: return Activity("\(operation) outcome unknown", .uncertain)
            case .preparing, .running: break
            }
        }
        switch port.mode {
        case "dfu", "kis_dfu": return Activity("DFU", .none)
        case "recovery": return Activity("Recovery", .none)
        default: return port.state == "charging" ? Activity("Charging only", .none) : .idle
        }
    }

    /// The contract's power in whole watts, as the board's display gives it.
    var watts: String? {
        port.contractMw.map { "\(($0 + 500) / 1000) W" }
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
            return BoardDevices(board: board, title: title, devices: board.ports.map {
                Device(boardTitle: title, serial: board.serial, session: board.session, port: $0)
            })
        }
    }

    var devices: [Device] {
        boardDevices.flatMap(\.devices)
    }
}

/// What can be done with a set of devices now. DFU, restart and power go to
/// each device that takes them; a restore needs every device to be eligible,
/// since it erases them.
struct DeviceActions: Equatable {
    var dfu: [AgentRequest] = []
    var restart: [AgentRequest] = []
    var revive: AgentRequest?
    var restore: RestoreConfirmation?
    var power: [AgentRequest] = []
    /// The ports are on, so the power action turns them off.
    var powered = true
    /// A Mac in DFU over Apple Debug USB on a host without Apple Configurator,
    /// which is what restores it.
    var needsConfigurator = false

    init() {}

    init(_ devices: [Device], restoreProblem: String?) {
        dfu = devices.filter(\.port.dfu).map { .dfu(board: $0.serial, port: $0.port.number) }
        restart = devices.filter(\.port.reboot).map { .reboot(board: $0.serial, port: $0.port.number) }
        if devices.count == 1, let device = devices.first, device.port.revive,
           let session = device.session, let ecid = device.port.ecid
        {
            revive = .revive(
                board: device.serial,
                session: session,
                port: device.port.number,
                generation: device.port.generation,
                ecid: ecid
            )
        }
        restore = restoreProblem == nil ? RestoreConfirmation(devices) : nil
        let enabled = Set(devices.map(\.port.enabled))
        if enabled.count == 1, let on = enabled.first {
            powered = on
            power = devices.map { .power(board: $0.serial, port: $0.port.number, on: !on) }
        }
        needsConfigurator = devices.contains { $0.port.mode == "kis_dfu" && !$0.port.restore }
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
