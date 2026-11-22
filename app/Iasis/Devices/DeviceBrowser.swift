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

    init(model: AgentModel) {
        self.model = model
    }

    var selected: [Device] {
        devices(in: selection)
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
