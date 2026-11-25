import Observation
import SwiftUI

/// The main window's selection and what it does with it, shared by the
/// window's own controls and the Device menu.
@Observable
final class DeviceBrowser {
    let model: AgentModel
    var selection = Set<Device.ID>()
    var showsInspector = true
    /// The erase the user is being asked to confirm.
    var confirming: RestoreConfirmation?
    @ObservationIgnored private var planning: Task<Void, Never>?

    init(model: AgentModel) {
        self.model = model
    }

    /// Ask the agent what each device would be restored with, then ask the
    /// user to confirm the erase.
    func confirm(_ restore: RestoreConfirmation) {
        guard planning == nil else { return }
        planning = Task {
            defer { planning = nil }
            guard let planned = await model.plan(restore.hubs) else { return }
            var confirmation = restore
            confirmation.planned = planned
            confirming = confirmation
        }
    }

    var selected: [Device] {
        devices(in: selection)
    }

    /// What the host lacks for a selected device's restore.
    var lacking: AgentState.Prerequisite? {
        selected.lazy.compactMap(\.needs).first
    }

    /// The device the inspector shows: the selection, when it is one device.
    var inspected: Device? {
        let selected = selected
        return selected.count == 1 ? selected.first : nil
    }

    var actions: DeviceActions {
        actions(for: selection)
    }

    func devices(in ids: Set<Device.ID>) -> [Device] {
        model.state?.devices.filter { ids.contains($0.id) && $0.isSelectable } ?? []
    }

    func actions(for ids: Set<Device.ID>) -> DeviceActions {
        DeviceActions(devices(in: ids))
    }
}

extension FocusedValues {
    @Entry var deviceBrowser: DeviceBrowser?
}
