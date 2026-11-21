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

        var title: String {
            device.port.occupied ? device.shortName : "Port \(device.port.number) Off"
        }
    }

    /// Nil while no agent answers.
    let boards: [Board]?
    let restoreProblem: String?
    /// How far an image download that no port is waiting for has come.
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
            "\($0.version), \($0.fraction.formatted(.percent.precision(.fractionLength(0))))"
        }
    }

    /// The titles of the menu's items, in order. A menu item is replaced
    /// when its title changes, and one replaced under the pointer loses its
    /// open submenu. Everything else an item shows changes it in place.
    var titles: [String] {
        guard let boards else { return [] }
        var titles = boards.flatMap { board in
            [board.title] + [board.notice].compactMap(\.self) + board.devices.flatMap { entry in
                [entry.title] + (entry.actions.needsConfigurator ? ["Configurator"] : [])
            }
        }
        titles += [restoreProblem, download.map { _ in "Download" }].compactMap(\.self)
        return titles
    }
}
