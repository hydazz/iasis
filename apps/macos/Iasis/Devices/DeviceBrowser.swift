import Observation
import SwiftUI

/// The main window's selection and what it does with it, shared by the
/// window's own controls and the Device menu.
@Observable
final class DeviceBrowser {
    let model: AgentModel
    var selection = Set<Device.ID>()
    var selectedSetup: SetupAction? {
        SetupAction.menuCheck(for: selected)
    }

    let setupCheck = SetupCheck()
    var advancing: SetupDraft?
    var showsInspector = true
    var portToReveal: Device.ID?
    var restorePrompt: RestorePrompt?
    @ObservationIgnored private var planning: Task<Void, Never>?

    init(model: AgentModel) {
        self.model = model
    }

    func openDevice(_ id: Device.ID) {
        selection = [id]
        showsInspector = true
        portToReveal = id
    }

    func chooseSetup(for ids: Set<Device.ID>, checked: SetupAction) {
        let devices = devices(in: ids)
        guard advancing == nil, SetupAction.menuCheck(for: devices) == checked, let defaults = model.state?.postRestore else { return }
        advancing = SetupDraft(devices: devices, targets: checked, defaults: defaults)
    }

    func advance(_ draft: SetupDraft) async {
        guard advancing === draft, !draft.submitting, let action = draft.action(in: self), draft.check.accepts(action) else { return }
        draft.submitting = true
        defer { draft.submitting = false }
        draft.check.clear()
        setupCheck.clear()
        do {
            try await model.advanceSetup(action, checkOnly: false)
            advancing = nil
        } catch {
            draft.submissionError = error.localizedDescription
        }
        await setupCheck.refresh(selectedSetup, model: model)
    }

    func begin(_ operation: AgentState.Operation, on restore: RestoreConfirmation?, showDevices: () -> Void = {}) {
        guard let restore, let state = model.state else { return }
        let warnings = state.hubs.compactMap { hub -> String? in
            guard restore.hubs.contains(where: { $0.hub == hub.serial }), let warning = hub.restorePowerWarning else { return nil }
            return "\(state.title(for: hub))\n\(warning)"
        }
        if operation == .restore || !warnings.isEmpty {
            showDevices()
            guard restorePrompt == nil, planning == nil else { return }
        }
        if warnings.isEmpty {
            proceed(with: operation, on: restore)
        } else {
            restorePrompt = .power(warnings.joined(separator: "\n\n"), operation, restore)
        }
    }

    func proceed(with operation: AgentState.Operation, on restore: RestoreConfirmation) {
        if operation == .revive {
            model.perform([.revive(hubs: restore.hubs, latest: false)])
            return
        }
        planning = Task {
            defer { planning = nil }
            guard let planned = await model.plan(restore.hubs) else { return }
            var confirmation = restore
            confirmation.planned = planned
            restorePrompt = .erase(confirmation)
        }
    }

    var selected: [Device] {
        devices(in: selection)
    }

    /// What the host lacks for a selected device's restore.
    var lacking: AgentState.Prerequisite? {
        selected.lazy.compactMap(\.needs).first
    }

    /// The selected physical port, including while it is empty.
    var inspected: Device? {
        let selected = selected
        return selected.count == 1 ? selected.first : nil
    }

    var actions: DeviceActions {
        DeviceActions(selected)
    }

    func devices(in ids: Set<Device.ID>) -> [Device] {
        let devices = model.state?.devices.filter { ids.contains($0.id) } ?? []
        return devices.count == ids.count ? devices : []
    }
}

enum RestorePrompt {
    case power(String, AgentState.Operation, RestoreConfirmation)
    case erase(RestoreConfirmation)

    var title: String {
        switch self {
        case .power: "Check Hub Power"
        case let .erase(confirmation): confirmation.title
        }
    }

    var message: String {
        switch self {
        case let .power(warning, _, _): warning
        case let .erase(confirmation): confirmation.message
        }
    }
}

@Observable
final class SetupCheck {
    private(set) var checked: SetupAction?
    private(set) var refusal: String?
    @ObservationIgnored private var revision: UInt = 0

    func accepts(_ action: SetupAction?) -> Bool {
        action != nil && checked == action && refusal == nil
    }

    func refresh(_ action: SetupAction?, model: AgentModel) async {
        clear()
        let currentRevision = revision
        guard let action else { return }
        let reason: String?
        do {
            try await model.advanceSetup(action, checkOnly: true)
            reason = nil
        } catch {
            reason = error.localizedDescription
        }
        guard !Task.isCancelled, currentRevision == revision else { return }
        checked = action
        refusal = reason
    }

    func clear() {
        revision &+= 1
        checked = nil
        refusal = nil
    }
}

extension FocusedValues {
    @Entry var deviceBrowser: DeviceBrowser?
}
