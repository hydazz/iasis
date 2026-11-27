import SwiftUI

struct HubsSettings: View {
    let model: AgentModel
    @State private var selection: String?

    var body: some View {
        Form {
            if let state = model.state, !state.hubs.isEmpty {
                let selected = state.hubs.first { $0.serial == selection } ?? state.hubs[0]
                Section {
                    Picker("Hub", selection: Binding(get: { selected.serial }, set: { selection = $0 })) {
                        ForEach(state.hubs) { hub in
                            Text(state.title(for: hub)).tag(hub.serial)
                        }
                    }
                }
                if selected.ports.isEmpty {
                    Section {
                        if let notice = selected.notice {
                            Text(notice)
                        } else if selected.update?.required == true {
                            Text("Update IasisOS to use this hub.")
                        } else {
                            Text("Hub Unavailable")
                        }
                    }
                } else {
                    Section {
                        HubSettings(hub: selected, model: model)
                            .id(selected.serial)
                    }
                }
                Section("About") {
                    LabeledContent("Serial Number", value: selected.serial)
                    if !selected.firmware.isEmpty {
                        LabeledContent("IasisOS") {
                            Text(selected.firmware)
                            if let update = selected.update {
                                if model.updating.contains(selected.serial) {
                                    ProgressView()
                                        .controlSize(.small)
                                } else {
                                    Button("Update to \(update.version)") { model.updateFirmware(of: selected.serial) }
                                }
                            }
                        }
                    }
                }
                .textSelection(.enabled)
            } else {
                Section {
                    Text(model.state == nil ? "Agent Not Running" : "No Hubs Connected")
                        .foregroundStyle(.secondary)
                }
            }
        }
        .formStyle(.grouped)
        .fixedSize(horizontal: false, vertical: true)
    }
}

private struct HubSettings: View {
    let hub: AgentState.Hub
    let model: AgentModel
    @State private var draft: String?
    @FocusState private var naming: Bool
    @State private var lightLevel = LightLevelEdit()
    @State private var writing: Task<Void, Never>?

    var body: some View {
        TextField("Name", text: Binding(get: { draft ?? hub.name }, set: { draft = $0 }), prompt: Text("Iasis"))
            .focused($naming)
            .onSubmit(saveName)
            .onChange(of: naming) { _, focused in
                if !focused {
                    saveName()
                }
            }
            .onDisappear(perform: saveName)
            .help("Up to 16 printable ASCII characters. An empty name resets the hub name.")
        Slider(
            value: Binding(get: { lightLevel.value(reading: hub.brightness) }, set: { lightLevel.drag(to: $0) }),
            in: 10 ... 100
        ) {
            Text("Light Brightness")
        } onEditingChanged: { editing in
            if !editing, let percent = lightLevel.release(reading: hub.brightness) {
                let previous = writing
                writing = Task {
                    await previous?.value
                    if await !(model.send(.brightness(hub: hub.serial, percent: percent))) {
                        lightLevel.reconcile(percent)
                    }
                }
            }
        }
        .accessibilityValue("\(Int(lightLevel.value(reading: hub.brightness).rounded())) percent")
        .onChange(of: hub.brightness) { _, reading in lightLevel.reconcile(reading) }
        .task(id: lightLevel.submitted) {
            guard let percent = lightLevel.submitted else { return }
            await writing?.value
            // Another client can overwrite the setting between two watch samples.
            try? await Task.sleep(for: .seconds(2))
            if !Task.isCancelled {
                lightLevel.reconcile(percent)
            }
        }
        .onChange(of: hub.name) { _, name in
            if draft == name {
                draft = nil
            }
        }
    }

    private func saveName() {
        guard let draft, draft != hub.name else { return }
        model.perform([.rename(hub: hub.serial, name: draft)])
    }
}
