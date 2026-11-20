import Foundation

/// Only information presented in the menu participates in menu tracking.
struct DeviceMenuState: Equatable {
    struct Board: Equatable, Identifiable {
        let id: String
        let title: String
        let notice: String?
        let ports: [Port]
    }

    struct Port: Equatable, Identifiable {
        let id: Int
        let title: String
        let symbol: String
        let detail: String?
        let configuratorRestore: Bool
        let dfu: AgentRequest?
        let restart: AgentRequest?
        let revive: AgentRequest?
        let restore: RestoreConfirmation?
    }

    let boards: [Board]
    let restoreProblem: String?
    let backgroundDownload: String?

    init(_ state: AgentState) {
        boards = state.boards.map { board in
            let title = state.title(for: board)
            return Board(id: board.serial, title: title, notice: board.notice, ports: board.visiblePorts.map { port in
                Port(
                    id: port.number,
                    title: port.menuTitle,
                    symbol: port.symbol,
                    detail: port.device != port.shortName ? port.device : nil,
                    configuratorRestore: port.mode == "kis_dfu" && !port.restore,
                    dfu: port.dfu ? .dfu(board: board.serial, port: port.number) : nil,
                    restart: port.reboot ? .reboot(board: board.serial, port: port.number) : nil,
                    revive: port.reviveRequest(on: board),
                    restore: state.restoreProblem == nil ? RestoreConfirmation(board: board, title: title, ports: [port]) : nil
                )
            })
        }
        restoreProblem = state.restoreProblem
        let fetchingForPort = state.boards.contains { $0.ports.contains { $0.state == "downloading image" } }
        backgroundDownload = fetchingForPort ? nil : state.images.fetch?.version
    }
}
