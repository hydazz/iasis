import SwiftUI
import UniformTypeIdentifiers

struct SettingsView: View {
    let model: AgentModel

    var body: some View {
        TabView {
            Tab("General", systemImage: "gearshape") {
                GeneralSettings(model: model)
            }
            Tab("Images", systemImage: "internaldrive") {
                ImageSettings(model: model)
            }
        }
        .frame(width: 460, height: 380)
    }
}

private struct GeneralSettings: View {
    let model: AgentModel
    @State private var selection: String?
    @State private var revealsDevices = FinderReveal.isEnabled

    var body: some View {
        Form {
            Section("Boards") {
                if let state = model.state, !state.boards.isEmpty {
                    let selected = state.boards.first { $0.serial == selection } ?? state.boards[0]
                    Picker("Board", selection: Binding(get: { selected.serial }, set: { selection = $0 })) {
                        ForEach(state.boards) { board in
                            Text(state.title(for: board)).tag(board.serial)
                        }
                    }
                    LabeledContent("Serial", value: selected.serial)
                    if selected.ports.isEmpty {
                        Text(selected.notice ?? "Board unavailable")
                    } else {
                        BoardSettings(board: selected, model: model)
                            .id(selected.serial)
                            .disabled(selected.settingsBusy)
                        if selected.settingsBusy {
                            Text("Settings are locked while the board is busy.")
                                .foregroundStyle(.secondary)
                        }
                    }
                } else {
                    Text(model.state == nil ? "Agent Not Running" : "No Boards Connected")
                }
            }
            Section {
                Toggle("Open Finder when a device connects", isOn: $revealsDevices)
            } footer: {
                Text("Finder opens a window for each device that enters DFU. This is an undocumented macOS setting and applies to every Apple device you connect.")
            }
        }
        .formStyle(.grouped)
        .onChange(of: revealsDevices) { _, reveals in
            FinderReveal.set(reveals)
        }
    }
}

private struct BoardSettings: View {
    let board: AgentState.Board
    let model: AgentModel
    @State private var draft: String?
    @State private var brightness: Double?

    var body: some View {
        LabeledContent("Name") {
            TextField("Board name", text: Binding(get: { draft ?? board.name }, set: { draft = $0 }), prompt: Text("Iasis"))
                .labelsHidden()
                .onSubmit(saveName)
                .help("Up to 16 printable ASCII characters. An empty name resets the board name.")
            Button("Save", action: saveName)
                .disabled(draft == nil || draft == board.name)
        }
        Slider(
            value: Binding(get: { brightness ?? Double(board.brightness) }, set: { brightness = $0 }),
            in: 10 ... 100,
            step: 1
        ) {
            Text("Light brightness")
        } onEditingChanged: { editing in
            if !editing, let brightness {
                model.perform(.brightness(board: board.serial, percent: Int(brightness)))
                self.brightness = nil
            }
        }
        .accessibilityValue("\(Int(brightness ?? Double(board.brightness))) percent")
        .onChange(of: board.name) { _, name in
            if draft == name {
                draft = nil
            }
        }
    }

    private func saveName() {
        guard let draft, draft != board.name else { return }
        model.perform(.rename(board: board.serial, name: draft))
    }
}

private struct ImageSettings: View {
    let model: AgentModel
    @State private var identifier = ""
    @State private var choosingFile = false

    var body: some View {
        Form {
            if let images = model.state?.images {
                Section("Kept Current") {
                    ForEach(images.models) { kept in
                        LabeledContent {
                            Button("Stop Keeping", systemImage: "minus.circle") {
                                model.perform(.forget(model: kept.identifier))
                            }
                            .labelStyle(.iconOnly)
                            .buttonStyle(.borderless)
                        } label: {
                            Text(kept.name)
                            Text(status(of: kept, fetch: images.fetch))
                        }
                    }
                    HStack {
                        TextField("Model", text: $identifier, prompt: Text("iPad13,18"))
                            .labelsHidden()
                            .onSubmit(keep)
                        Button("Keep", action: keep)
                            .disabled(identifier.isEmpty)
                    }
                }
                Section("Images") {
                    ForEach(images.images) { image in
                        LabeledContent {
                            Button("Remove", systemImage: "trash") {
                                model.perform(.remove(build: image.buildVersion))
                            }
                            .labelStyle(.iconOnly)
                            .buttonStyle(.borderless)
                        } label: {
                            Text("\(image.productVersion) (\(image.buildVersion))")
                            Text(summary(of: image))
                        }
                    }
                    HStack {
                        Button("Add Image…") {
                            choosingFile = true
                        }
                        Spacer()
                        Button("Check for Updates") {
                            model.perform(.refresh)
                        }
                    }
                }
            } else {
                ContentUnavailableView("Agent Not Running", systemImage: "bolt.slash")
            }
        }
        .formStyle(.grouped)
        .fileImporter(
            isPresented: $choosingFile,
            allowedContentTypes: [UTType(filenameExtension: "ipsw") ?? .data]
        ) { chosen in
            if case let .success(file) = chosen {
                model.perform(.importImage(path: file.path(percentEncoded: false)))
            }
        }
    }

    private func keep() {
        model.perform(.keep(model: identifier))
        identifier = ""
    }

    private func status(of kept: AgentState.Model, fetch: AgentState.Fetch?) -> String {
        guard let latest = kept.latest else {
            return kept.error ?? "Asking for the newest release"
        }
        let release = "\(latest.version) (\(latest.build))"
        if kept.ready {
            return "\(release), ready"
        }
        if let error = kept.error {
            return "\(release): \(error)"
        }
        if let fetch, fetch.build == latest.build {
            return "\(release), downloading \(fetch.fraction.formatted(.percent.precision(.fractionLength(0))))"
        }
        return "\(release), waiting to download"
    }

    private func summary(of image: AgentState.Image) -> String {
        let size = image.bytes.formatted(.byteCount(style: .file))
        let models = image.productTypes.count
        let origin = image.fetched ? "downloaded" : "added"
        return "\(size), \(origin), \(models) model\(models == 1 ? "" : "s")"
    }
}
