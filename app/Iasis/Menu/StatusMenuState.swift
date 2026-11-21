import Foundation

/// The menu bar item's icon and menu content.
struct StatusMenuState: Equatable {
    struct Board: Equatable, Identifiable {
        let id: String
        let title: String
        let notice: String?
        let devices: [Entry]
    }

    /// A device and what its submenu offers.
    struct Entry: Equatable, Identifiable {
        let device: Device
        let actions: DeviceActions

        var id: Device.ID {
            device.id
        }
    }

    /// Nil while no agent answers.
    let boards: [Board]?
    let restoreProblem: String?
    /// An image download that no port is waiting for.
    let download: String?
    let status: MenuBarStatus

    init(_ state: AgentState?) {
        status = MenuBarStatus(state)
        guard let state else {
            boards = nil
            restoreProblem = nil
            download = nil
            return
        }
        boards = state.boardDevices.map { group in
            Board(
                id: group.id,
                title: group.title,
                notice: group.board.notice,
                devices: group.devices.filter(\.isSelectable).map {
                    Entry(device: $0, actions: DeviceActions([$0], restoreProblem: state.restoreProblem))
                }
            )
        }
        restoreProblem = state.restoreProblem
        let waitedFor = state.devices.contains { $0.port.state == "downloading image" }
        download = waitedFor ? nil : state.images.fetch.map {
            "Downloading \($0.version) (\($0.fraction.formatted(.percent.precision(.fractionLength(0)))))"
        }
    }
}
