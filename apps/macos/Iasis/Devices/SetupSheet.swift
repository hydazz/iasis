import SwiftUI

@Observable
final class SetupDraft: Identifiable {
    let title: String
    let language: AgentState.SetupLanguage
    let check = SetupCheck()
    var setLanguage: Bool {
        didSet { submissionError = nil }
    }

    var enroll: Bool {
        didSet { submissionError = nil }
    }

    var submitting = false
    var submissionError: String?
    private let ids: Set<Device.ID>
    private let targets: SetupAction

    init(devices: [Device], targets: SetupAction, defaults: AgentState.PostRestore) {
        ids = Set(devices.map(\.id))
        self.targets = targets
        title = devices.count == 1 ? "Advance Setup for \(devices[0].shortName)" : "Advance Setup for \(devices.count) Devices"
        language = defaults.setupLanguage ?? .current
        setLanguage = defaults.setupLanguage != nil || !defaults.tryEnrollment
        enroll = defaults.tryEnrollment
    }

    func action(in browser: DeviceBrowser) -> SetupAction? {
        guard setLanguage || enroll, SetupAction.menuCheck(for: browser.devices(in: ids)) == targets else { return nil }
        return SetupAction(hubs: targets.hubs, settings: setLanguage ? language : nil, enroll: enroll)
    }
}

struct SetupSheet: View {
    let browser: DeviceBrowser
    @Bindable var draft: SetupDraft

    var body: some View {
        let action = draft.action(in: browser)
        VStack(alignment: .leading, spacing: 14) {
            Text(draft.title).font(.headline)
            Toggle(isOn: $draft.setLanguage) {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Set language and region")
                    Text("\(draft.language.language), \(draft.language.locale)").font(.callout).foregroundStyle(.secondary)
                }
            }
            Toggle(isOn: $draft.enroll) {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Try enrollment")
                    Text("Request the assigned management configuration. Complete any required sign-in or Partially Set Up prompt on the device.")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            if let message = message(for: action) {
                ScrollView {
                    Text(message)
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .frame(maxHeight: 140)
                .fixedSize(horizontal: false, vertical: true)
            }
            HStack {
                if draft.submitting || (action != nil && draft.check.checked != action && draft.submissionError == nil) {
                    ProgressView().controlSize(.small)
                    Text(draft.submitting ? "Advancing setup…" : "Checking devices…")
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button("Cancel", role: .cancel) { browser.advancing = nil }
                    .keyboardShortcut(.cancelAction)
                Button("Advance Setup") { Task { await browser.advance(draft) } }
                    .keyboardShortcut(.defaultAction)
                    .disabled(!draft.check.accepts(action))
            }
        }
        .padding(20)
        .frame(width: 420)
        .disabled(draft.submitting)
        .interactiveDismissDisabled(draft.submitting)
        .task(id: action) {
            await draft.check.refresh(action, model: browser.model)
        }
    }

    private func message(for action: SetupAction?) -> String? {
        if let error = draft.submissionError {
            return error
        }
        if draft.submitting {
            return nil
        }
        if !draft.setLanguage, !draft.enroll {
            return "Choose a setup step."
        }
        guard let action else { return "The selected devices are no longer available for setup." }
        return draft.check.checked == action ? draft.check.refusal : nil
    }
}
