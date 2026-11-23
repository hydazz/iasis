import SwiftUI
import UniformTypeIdentifiers

struct SettingsView: View {
    let model: AgentModel

    var body: some View {
        TabView {
            Tab("General", systemImage: "gearshape") {
                GeneralSettings(model: model)
            }
            Tab("Boards", systemImage: "cpu") {
                BoardsSettings(model: model)
            }
            Tab("Images", systemImage: "internaldrive") {
                ImageSettings(model: model)
            }
        }
        .frame(width: 480)
    }
}

/// What applies to the app and its agent, whichever boards are connected.
private struct GeneralSettings: View {
    let model: AgentModel
    @State private var opensAtLogin = OpenAtLogin.isEnabled
    @State private var revealsDevices = FinderReveal.isEnabled

    var body: some View {
        Form {
            Section {
                Toggle("Open at Login", isOn: Binding {
                    opensAtLogin
                } set: { opens in
                    OpenAtLogin.set(opens)
                    opensAtLogin = OpenAtLogin.isEnabled
                })
            }
            Section {
                Toggle("Share Internet over USB", isOn: Binding {
                    model.state?.sharing.enabled == true
                } set: { enabled in
                    model.perform([.sharing(enabled: enabled)])
                })
                .disabled(model.state == nil)
                if let problem = model.state?.sharing.problem {
                    Label(problem, systemImage: "exclamationmark.triangle")
                        .textSelection(.enabled)
                }
            }
            Section {
                Toggle("Open Finder When a Device Connects", isOn: $revealsDevices)
            } footer: {
                Text("Finder opens a window for each device that enters DFU. This is an undocumented macOS setting and applies to every Apple device you connect.")
            }
        }
        .formStyle(.grouped)
        .fixedSize(horizontal: false, vertical: true)
        .onChange(of: revealsDevices) { _, reveals in
            FinderReveal.set(reveals)
        }
    }
}

/// What a board keeps: one board's settings, with a choice of board above
/// them when there are several.
private struct BoardsSettings: View {
    let model: AgentModel
    @State private var selection: String?

    var body: some View {
        Form {
            if let state = model.state, !state.boards.isEmpty {
                let selected = state.boards.first { $0.serial == selection } ?? state.boards[0]
                if state.boards.count > 1 {
                    Picker("Board", selection: Binding(get: { selected.serial }, set: { selection = $0 })) {
                        ForEach(state.boards) { board in
                            Text(state.title(for: board)).tag(board.serial)
                        }
                    }
                }
                if selected.ports.isEmpty {
                    Section {
                        Text(selected.notice ?? "Board Unavailable")
                    }
                } else {
                    Section {
                        BoardSettings(board: selected, model: model)
                            .id(selected.serial)
                            .disabled(selected.settingsBusy)
                    } footer: {
                        if selected.settingsBusy {
                            Text("Settings are locked while the board is busy.")
                        }
                    }
                }
                Section {
                    LabeledContent("Serial", value: selected.serial)
                    if !selected.firmware.isEmpty {
                        LabeledContent("IasisOS", value: selected.firmware)
                    }
                }
                .textSelection(.enabled)
            } else {
                Section {
                    Text(model.state == nil ? "Agent Not Running" : "No Boards Connected")
                        .foregroundStyle(.secondary)
                }
            }
        }
        .formStyle(.grouped)
        .fixedSize(horizontal: false, vertical: true)
    }
}

private struct BoardSettings: View {
    let board: AgentState.Board
    let model: AgentModel
    @State private var draft: String?
    @State private var lightLevel = LightLevelEdit()
    @State private var writing: Task<Void, Never>?

    var body: some View {
        LabeledContent("Name") {
            TextField("Board Name", text: Binding(get: { draft ?? board.name }, set: { draft = $0 }), prompt: Text("Iasis"))
                .labelsHidden()
                .onSubmit(saveName)
                .help("Up to 16 printable ASCII characters. An empty name resets the board name.")
            Button("Save", action: saveName)
                .disabled(draft == nil || draft == board.name)
        }
        Slider(
            value: Binding(get: { lightLevel.value(reading: board.brightness) }, set: { lightLevel.drag(to: $0) }),
            in: 10 ... 100
        ) {
            Text("Light Brightness")
        } onEditingChanged: { editing in
            if !editing, let percent = lightLevel.release(reading: board.brightness) {
                let previous = writing
                writing = Task {
                    await previous?.value
                    if await !(model.send(.brightness(board: board.serial, percent: percent))) {
                        lightLevel.reconcile(percent)
                    }
                }
            }
        }
        .accessibilityValue("\(Int(lightLevel.value(reading: board.brightness).rounded())) percent")
        .onChange(of: board.brightness) { _, reading in lightLevel.reconcile(reading) }
        .task(id: lightLevel.submitted) {
            guard let percent = lightLevel.submitted else { return }
            await writing?.value
            // Another client can overwrite the setting between two watch samples.
            try? await Task.sleep(for: .seconds(2))
            if !Task.isCancelled {
                lightLevel.reconcile(percent)
            }
        }
        .onChange(of: board.name) { _, name in
            if draft == name {
                draft = nil
            }
        }
    }

    private func saveName() {
        guard let draft, draft != board.name else { return }
        model.perform([.rename(board: board.serial, name: draft)])
    }
}

/// The restore images the agent holds or is fetching, one row for each
/// archive.
private struct ImageSettings: View {
    let model: AgentModel
    @State private var choosingRelease = false
    @State private var choosingFile = false

    var body: some View {
        Form {
            if let store = model.state?.images {
                Section {
                    ForEach(store.images) { image in
                        ImageRow(image: image, fetch: store.fetch) {
                            model.perform([.remove(image: image.id)])
                        }
                    }
                    if let importing = model.importing {
                        ImportRow(importing: importing)
                    }
                    HStack {
                        Text(summary(of: store.images))
                            .foregroundStyle(.secondary)
                        Spacer()
                        Menu("Add Image") {
                            Button("Download…") {
                                choosingRelease = true
                            }
                            Button("From File…") {
                                choosingFile = true
                            }
                        }
                        .fixedSize()
                    }
                } footer: {
                    Text("A restore uses the newest image here that Apple still signs, or downloads the newest release. Images stay until you remove them.")
                }
            } else {
                Section {
                    Text("Agent Not Running")
                        .foregroundStyle(.secondary)
                }
            }
        }
        .formStyle(.grouped)
        .frame(height: 440)
        .sheet(isPresented: $choosingRelease) {
            ReleasePicker(model: model)
        }
        .fileImporter(
            isPresented: $choosingFile,
            allowedContentTypes: [UTType(filenameExtension: "ipsw") ?? .data]
        ) { chosen in
            if case let .success(file) = chosen {
                model.importImage(file)
            }
        }
    }

    private func summary(of images: [AgentState.Image]) -> String {
        let held = images.filter(\.ready)
        guard !held.isEmpty else { return "No Images" }
        let size = held.reduce(0) { $0 + $1.bytes }.formatted(.byteCount(style: .file))
        return "\(held.count) image\(held.count == 1 ? "" : "s"), \(size)"
    }
}

/// An image on one row: the system and version, what it is for, and one mark
/// for whether it is here.
private struct ImageRow: View {
    let image: AgentState.Image
    let fetch: AgentState.Fetch?
    let remove: () -> Void

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: DeviceSymbol.system(image.os))
                .font(.title2)
                .foregroundStyle(.secondary)
                .frame(width: 30)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text(image.title)
                Text(detail)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .monospacedDigit()
                    .help(detail)
            }
            .lineLimit(1)
            Spacer(minLength: 4)
            ActivityMark(activity: status)
            Button(image.ready ? "Remove" : "Cancel", systemImage: image.ready ? "trash" : "xmark.circle", action: remove)
                .labelStyle(.iconOnly)
                .buttonStyle(.borderless)
                .help(image.ready ? "Delete this image" : "Do not download this image")
        }
    }

    private var detail: String {
        if !image.ready, let error = image.error {
            return error
        }
        var parts = [image.build, image.bytes.formatted(.byteCount(style: .file))]
        // macOS says which devices by itself; the others restore only some.
        if image.os != "macOS", image.models.count <= 2 {
            parts.append(image.models.joined(separator: ", "))
        }
        return parts.joined(separator: " · ")
    }

    private var status: Activity {
        if image.ready {
            return Activity("Downloaded", .done)
        }
        if image.error != nil {
            return Activity("Not Downloaded", .caution)
        }
        if let fetch, fetch.id == image.id {
            let percent = fetch.fraction.formatted(.percent.precision(.fractionLength(0)))
            return Activity("Downloading \(percent)", .working(fetch.fraction), symbol: DeviceSymbol.download)
        }
        return Activity("Waiting to Download", .working(0), symbol: DeviceSymbol.download)
    }
}

/// An archive being added from a file: sent to the agent, then checked.
private struct ImportRow: View {
    let importing: AgentModel.Importing

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "doc.zipper")
                .font(.title2)
                .foregroundStyle(.secondary)
                .frame(width: 30)
                .accessibilityHidden(true)
            Text(importing.name)
                .lineLimit(1)
                .truncationMode(.middle)
            Spacer(minLength: 4)
            ActivityMark(activity: activity)
        }
    }

    private var activity: Activity {
        guard importing.sent < importing.bytes else {
            return Activity("Checking", .working(nil))
        }
        let percent = importing.fraction.formatted(.percent.precision(.fractionLength(0)))
        return Activity("Adding \(percent)", .working(importing.fraction))
    }
}

/// A model from the catalog and, of the releases Apple signs for it, which
/// to download.
private struct ReleasePicker: View {
    let model: AgentModel
    @Environment(\.dismiss) private var dismiss
    @State private var catalog: [AgentState.CatalogModel]?
    @State private var problem: String?
    @State private var search = ""
    @State private var device: String?
    @State private var releases: Result<[AgentState.Release], AgentError>?
    @State private var release: String?

    var body: some View {
        VStack(spacing: 0) {
            TextField("Search", text: $search, prompt: Text("Search Models"))
                .textFieldStyle(.roundedBorder)
                .padding(12)
            Divider()
            HStack(spacing: 0) {
                List(matches, selection: $device) { entry in
                    Label {
                        Text(entry.name)
                    } icon: {
                        Image(systemName: DeviceSymbol.kind(of: entry.name))
                            .foregroundStyle(Color(nsColor: .secondaryLabelColor))
                    }
                    .help(entry.identifier)
                }
                .overlay {
                    if let problem {
                        ContentUnavailableView("Catalog Unavailable", systemImage: "wifi.exclamationmark", description: Text(problem))
                    } else if catalog == nil {
                        ProgressView()
                    } else if matches.isEmpty {
                        ContentUnavailableView.search(text: search)
                    }
                }
                Divider()
                releaseList
                    .frame(width: 250)
            }
            Divider()
            HStack {
                Spacer()
                Button("Cancel", role: .cancel) {
                    dismiss()
                }
                Button("Download") {
                    if let device, let release {
                        model.perform([.download(model: device, build: release)])
                    }
                    dismiss()
                }
                .keyboardShortcut(.defaultAction)
                .disabled(device == nil || release == nil)
            }
            .padding(12)
        }
        .frame(width: 620, height: 460)
        .task {
            do {
                catalog = try await model.catalog()
                    .sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
            } catch {
                problem = error.localizedDescription
            }
        }
        .task(id: device) {
            releases = nil
            release = nil
            guard let device else { return }
            do {
                let signed = try await model.releases(of: device)
                releases = .success(signed)
                release = signed.first?.build
            } catch {
                releases = .failure(.refused(error.localizedDescription))
            }
        }
    }

    @ViewBuilder private var releaseList: some View {
        switch releases {
        case let .success(releases) where !releases.isEmpty:
            List(releases, selection: $release) { release in
                VStack(alignment: .leading, spacing: 2) {
                    Text(release.version)
                    Text("\(release.build) · \(release.bytes.formatted(.byteCount(style: .file)))")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .monospacedDigit()
                }
            }
        case .success:
            ContentUnavailableView("No Signed Releases", systemImage: "signature", description: Text("Apple signs no release for this model."))
        case let .failure(error):
            ContentUnavailableView("Releases Unavailable", systemImage: "wifi.exclamationmark", description: Text(error.localizedDescription))
        case nil where device == nil:
            ContentUnavailableView("Select a Model", systemImage: "arrow.left")
        case nil:
            ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    private var matches: [AgentState.CatalogModel] {
        guard let catalog else { return [] }
        guard !search.isEmpty else { return catalog }
        return catalog.filter {
            $0.name.localizedStandardContains(search) || $0.identifier.localizedStandardContains(search)
        }
    }
}
