import CryptoKit
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
            SupervisionSection(model: model)
            Section {
                Toggle("Open Finder for DFU Devices", isOn: $revealsDevices)
            } footer: {
                Text("Finder opens a window for each device that enters DFU. This is an undocumented macOS setting, and turning it off also stops Finder opening for other Apple devices.")
            }
        }
        .formStyle(.grouped)
        .fixedSize(horizontal: false, vertical: true)
        .onChange(of: revealsDevices) { _, reveals in
            FinderReveal.set(reveals)
        }
    }
}

/// The identity the agent pairs with as supervisor, in one row: its name, its
/// certificate's fingerprint, and a menu to export it (the certificate for an
/// enrollment profile, or with the private key to back it up or move it) or
/// import another.
private struct SupervisionSection: View {
    let model: AgentModel
    @State private var exporting: ExportedFile?
    @State private var choosingFile = false
    @State private var sheet: IdentitySheet?

    var body: some View {
        let supervision = model.state?.supervision
        let name = supervision?.name ?? "Unnamed"
        Section {
            LabeledContent {
                Menu {
                    Button("Export…") { sheet = .export }
                        .disabled(supervision?.certificate == nil)
                    Button("Import…") { choosingFile = true }
                    Divider()
                    Button("Copy Fingerprint") {
                        NSPasteboard.general.clearContents()
                        NSPasteboard.general.setString(supervision?.fingerprint ?? "", forType: .string)
                    }
                    .disabled(supervision?.fingerprint == nil)
                } label: {
                    Image(systemName: "ellipsis.circle")
                }
                .menuStyle(.borderlessButton)
                .menuIndicator(.hidden)
                .fixedSize()
                .disabled(model.state == nil)
            } label: {
                if supervision?.certificate != nil {
                    Text(name)
                    if let fingerprint = supervision?.fingerprint {
                        Text(fingerprint)
                            .lineLimit(1)
                            .truncationMode(.middle)
                            .help("SHA-256 \(fingerprint)")
                    }
                } else {
                    Text(supervision?.problem ?? "No Identity")
                }
            }
        } header: {
            Text("Supervision Identity")
        } footer: {
            Text("Supervised iPhones and iPads whose enrollment profile lists this certificate as a supervising host pair with this Mac without asking for Trust.")
        }
        .fileExporter(
            isPresented: Binding { exporting != nil } set: {
                if !$0 {
                    exporting = nil
                }
            },
            document: exporting,
            contentType: exporting?.type ?? .data,
            defaultFilename: exporting?.name
        ) { _ in }
        .fileImporter(isPresented: $choosingFile, allowedContentTypes: [.pkcs12]) { result in
            guard case let .success(file) = result else { return }
            let scoped = file.startAccessingSecurityScopedResource()
            defer {
                if scoped {
                    file.stopAccessingSecurityScopedResource()
                }
            }
            if let archive = try? Data(contentsOf: file) {
                sheet = .import(archive)
            }
        }
        .sheet(item: $sheet) { request in
            switch request {
            case .export:
                ExportSheet { choice in
                    sheet = nil
                    switch choice {
                    case nil:
                        break
                    case .certificate:
                        if let certificate = supervision?.certificate.flatMap({ Data(base64Encoded: $0) }) {
                            exporting = ExportedFile(name: "\(name).cer", data: certificate, type: .x509Certificate)
                        }
                    case let .identity(password):
                        Task {
                            if let archive = await model.exportSupervision(password: password)
                                .flatMap({ Data(base64Encoded: $0) })
                            {
                                exporting = ExportedFile(name: "\(name).p12", data: archive, type: .pkcs12)
                            }
                        }
                    }
                }
            case let .import(archive):
                ImportSheet { password in
                    sheet = nil
                    if let password {
                        model.perform([.importSupervision(pkcs12: archive.base64EncodedString(), password: password)])
                    }
                }
            }
        }
    }
}

/// The sheet the identity row shows: what to export, or the password of an
/// archive to import.
private enum IdentitySheet: Identifiable {
    case export
    case `import`(Data)

    var id: Int {
        switch self {
        case .export: 0
        case .import: 1
        }
    }
}

/// What an export holds: the certificate alone, or with the private key in
/// an archive under this password.
private enum ExportChoice {
    case certificate
    case identity(password: String)
}

private struct ExportSheet: View {
    let done: (ExportChoice?) -> Void
    @State private var includesKey = false
    @State private var password = ""
    @State private var again = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Export Supervision Identity").font(.headline)
            Picker("Export", selection: $includesKey) {
                Text("Certificate, for an enrollment profile").tag(false)
                Text("Certificate and private key, to back up or move the identity").tag(true)
            }
            .pickerStyle(.radioGroup)
            .labelsHidden()
            if includesKey {
                Text("Anyone with the file and its password can pair with every device that lists the certificate.")
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                SecureField("Password", text: $password)
                SecureField("Verify", text: $again)
            }
            HStack {
                Spacer()
                Button("Cancel", role: .cancel) { done(nil) }
                    .keyboardShortcut(.cancelAction)
                Button("Export…") { done(includesKey ? .identity(password: password) : .certificate) }
                    .keyboardShortcut(.defaultAction)
                    .disabled(includesKey && (password.isEmpty || password != again))
            }
        }
        .padding(20)
        .frame(width: 380)
    }
}

private struct ImportSheet: View {
    let done: (String?) -> Void
    @State private var password = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Import Supervision Identity").font(.headline)
            Text("This replaces the Mac's identity. Devices that list the current certificate ask for Trust until they enroll with the new one.")
                .fixedSize(horizontal: false, vertical: true)
            SecureField("Password", text: $password)
            HStack {
                Spacer()
                Button("Cancel", role: .cancel) { done(nil) }
                    .keyboardShortcut(.cancelAction)
                Button("Replace") { done(password) }
                    .keyboardShortcut(.defaultAction)
                    .disabled(password.isEmpty)
            }
        }
        .padding(20)
        .frame(width: 380)
    }
}

/// A file saved from the agent: a certificate in DER, or an identity in a
/// PKCS #12 archive.
private struct ExportedFile: FileDocument {
    static let readableContentTypes: [UTType] = [.x509Certificate, .pkcs12]
    var name: String
    var data: Data
    var type: UTType

    init(name: String, data: Data, type: UTType) {
        self.name = name
        self.data = data
        self.type = type
    }

    init(configuration: ReadConfiguration) throws {
        name = configuration.file.filename ?? ""
        data = configuration.file.regularFileContents ?? Data()
        type = configuration.contentType
    }

    func fileWrapper(configuration _: WriteConfiguration) throws -> FileWrapper {
        FileWrapper(regularFileWithContents: data)
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
                        if let notice = selected.notice {
                            Text(notice)
                        } else if selected.update?.required == true {
                            Text("Update IasisOS to use this board.")
                        } else {
                            Text("Board Unavailable")
                        }
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
    @FocusState private var naming: Bool
    @State private var lightLevel = LightLevelEdit()
    @State private var writing: Task<Void, Never>?

    var body: some View {
        TextField("Name", text: Binding(get: { draft ?? board.name }, set: { draft = $0 }), prompt: Text("Iasis"))
            .focused($naming)
            .onSubmit(saveName)
            .onChange(of: naming) { _, focused in
                if !focused {
                    saveName()
                }
            }
            .onDisappear(perform: saveName)
            .help("Up to 16 printable ASCII characters. An empty name resets the board name.")
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
    @State private var inspecting: AgentState.Image?

    var body: some View {
        Form {
            if let store = model.state?.images {
                Section {
                    ForEach(store.images) { image in
                        let remove: () -> Void = { model.perform([.remove(image: image.id)]) }
                        ImageRow(image: image, fetch: store.fetch, remove: remove)
                            .contextMenu {
                                if image.ready {
                                    Button("Get Info") { inspecting = image }
                                    Divider()
                                    Button("Remove Image", action: remove)
                                } else {
                                    Button("Cancel Download", action: remove)
                                }
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
        .sheet(item: $inspecting) { image in
            ImageInfo(image: image)
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

/// An image on one row: the system and version, what it is for and, until it
/// is here, how its download stands.
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
            if !image.ready {
                ActivityMark(activity: status)
                Button("Cancel Download", systemImage: "xmark.circle", action: remove)
                    .labelStyle(.iconOnly)
                    .buttonStyle(.borderless)
                    .help("Do not download this image")
            }
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
        if image.error != nil {
            return Activity("Not Downloaded", .caution)
        }
        if let fetch, fetch.id == image.id {
            let percent = "\(fetch.percent)%"
            return Activity("Downloading \(percent)", .working(fetch.fraction), symbol: DeviceSymbol.download)
        }
        return Activity("Waiting to Download", .working(0), symbol: DeviceSymbol.download)
    }
}

/// What an image's row leaves out: every model it restores.
private struct ImageInfo: View {
    let image: AgentState.Image
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(spacing: 0) {
            Form {
                Section {
                    LabeledContent("Build", value: image.build)
                    LabeledContent("Size", value: image.bytes.formatted(.byteCount(style: .file)))
                } header: {
                    Label(image.title, systemImage: DeviceSymbol.system(image.os))
                        .font(.headline)
                }
                Section("Models") {
                    ForEach(image.models, id: \.self) { model in
                        Text(model)
                    }
                }
            }
            .formStyle(.grouped)
            .textSelection(.enabled)
            Divider()
            HStack {
                Spacer()
                Button("Done") { dismiss() }
                    .keyboardShortcut(.defaultAction)
            }
            .padding(12)
        }
        .frame(width: 360, height: 420)
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

private extension AgentState.Supervision {
    /// The certificate's SHA-256 fingerprint, in colon-separated hexadecimal
    /// pairs, to check which identity an enrollment profile lists.
    var fingerprint: String? {
        guard let certificate = certificate.flatMap({ Data(base64Encoded: $0) }) else { return nil }
        return SHA256.hash(data: certificate).map { String(format: "%02X", $0) }.joined(separator: ":")
    }
}
