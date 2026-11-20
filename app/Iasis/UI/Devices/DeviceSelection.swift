import Foundation

struct DeviceRow: Identifiable {
    enum ID: Hashable {
        case attachment(board: String, session: UInt32?, port: Int, generation: UInt32, ecid: String?)
    }

    let board: AgentState.Board
    let port: AgentState.Port

    var id: ID {
        .attachment(board: board.serial, session: board.session, port: port.number, generation: port.generation, ecid: port.ecid)
    }

    var name: String {
        port.device ?? (port.occupied ? "Unidentified Device" : "Disconnected Device")
    }

    var status: String {
        if let stage = port.stage {
            return stage.percent.map { "\(stage.name) · \($0)%" } ?? stage.name
        }
        if let result = port.result, !port.occupied || ["completed", "completion unknown", "failed"].contains(port.state) {
            return result.summary
        }
        if port.state == "ready" || port.state == "charging" {
            if port.inDFU {
                return "DFU"
            }
            if port.mode == "recovery" {
                return "Recovery"
            }
        }
        return port.state.prefix(1).uppercased() + port.state.dropFirst()
    }
}

extension AgentState.Board {
    var devices: [DeviceRow] {
        ports.filter { $0.occupied || $0.result != nil }.map { DeviceRow(board: self, port: $0) }
    }
}

extension AgentState.Ending {
    var summary: String {
        switch phase {
        case .reported: "\(operation.title) complete"
        case .failed: "\(operation.title) failed"
        case .unconfirmed: "\(operation.title) outcome unknown"
        case .preparing, .running: message
        }
    }
}

struct RestoreConfirmation: Equatable {
    let title: String
    let message: String
    let request: AgentRequest

    init?(board: AgentState.Board, title boardTitle: String, ports: [AgentState.Port]) {
        let sorted = ports.sorted { $0.number < $1.number }
        guard let first = sorted.first, let session = board.session,
              sorted.allSatisfy({ port in
                  port.occupied && port.restore && board.ports.contains { $0.target == port.target && $0.restore }
              })
        else { return nil }

        title = sorted.count == 1 ? "Restore \(first.shortName ?? "the device")?" : "Restore \(sorted.count) devices?"
        let devices = sorted.map { "Port \($0.number) · \($0.device ?? "Unidentified Device")" }
        message = (["All data on the selected \(sorted.count == 1 ? "device" : "devices") will be erased and the system software reinstalled.", "", boardTitle] + devices).joined(separator: "\n")
        request = .restore(board: board.serial, session: session, targets: sorted.map(\.target))
    }
}

struct DeviceSelection {
    let rows: [DeviceRow]
    let restore: RestoreConfirmation?

    init(_ ids: Set<DeviceRow.ID>, state: AgentState?) {
        let matches = state?.boards.flatMap(\.devices).filter { ids.contains($0.id) } ?? []
        guard matches.count == ids.count else {
            rows = []
            restore = nil
            return
        }
        rows = matches
        if let state, let board = rows.first?.board,
           state.restoreProblem == nil, rows.allSatisfy({ $0.board.serial == board.serial })
        {
            restore = RestoreConfirmation(board: board, title: state.title(for: board), ports: rows.map(\.port))
        } else {
            restore = nil
        }
    }

    var single: DeviceRow? {
        rows.count == 1 ? rows.first : nil
    }
}
