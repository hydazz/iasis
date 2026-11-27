import SwiftUI
import UniformTypeIdentifiers

struct ImageSettings: View {
    let model: AgentModel
    @State private var selection = Set<AgentState.Image.ID>()
    @State private var choosingRelease = false
    @State private var choosingFile = false
    @State private var inspecting: AgentState.Image?

    var body: some View {
        Form {
            if let store = model.state?.images {
                Section {
                    List(selection: $selection) {
                        ForEach(store.images) { image in
                            ImageRow(image: image, fetch: store.fetch) { remove([image.id]) }
                                .swipeActions {
                                    Button(removal(of: [image]), systemImage: "trash", role: .destructive) {
                                        remove([image.id])
                                    }
                                    .disabled(image.inUse)
                                }
                        }
                        if let importing = model.importing {
                            ImportRow(importing: importing)
                                .selectionDisabled()
                        }
                    }
                    .contextMenu(forSelectionType: AgentState.Image.ID.self) { ids in
                        menu(for: store.images.filter { ids.contains($0.id) })
                    } primaryAction: { ids in
                        let chosen = store.images.filter { ids.contains($0.id) }
                        if chosen.count == 1, let image = chosen.first, image.ready {
                            inspecting = image
                        }
                    }
                    .onDeleteCommand { remove(selection) }
                    .overlay {
                        if store.images.isEmpty, model.importing == nil {
                            Text("No Images")
                                .foregroundStyle(.secondary)
                        }
                    }
                    .frame(height: 240)
                    HStack(spacing: 4) {
                        Menu {
                            Button("Download…") { choosingRelease = true }
                            Button("From File…") { choosingFile = true }
                        } label: {
                            Label("Add Image", systemImage: "plus")
                                .labelStyle(.iconOnly)
                        }
                        .menuIndicator(.hidden)
                        .fixedSize()
                        .help("Add an image")
                        Button("Remove", systemImage: "minus") { remove(selection) }
                            .labelStyle(.iconOnly)
                            .disabled(removable(selection).isEmpty)
                            .help("Remove the selected images")
                        Spacer()
                        Text(summary(of: store.images))
                            .foregroundStyle(.secondary)
                    }
                    .buttonStyle(.borderless)
                } footer: {
                    Text("Restores use the newest compatible, signed image here or download the latest release. Images stay until you remove them.")
                }
            } else {
                Section {
                    Text("Agent Not Running")
                        .foregroundStyle(.secondary)
                }
            }
        }
        .formStyle(.grouped)
        .fixedSize(horizontal: false, vertical: true)
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

    @ViewBuilder private func menu(for chosen: [AgentState.Image]) -> some View {
        let held = chosen.filter(\.ready)
        if !held.isEmpty, held.count == chosen.count {
            Button("Show in Finder") {
                NSWorkspace.shared.activateFileViewerSelecting(held.compactMap { $0.path.map { URL(filePath: $0) } })
            }
            if held.count == 1, let image = held.first {
                Button("Get Info") { inspecting = image }
            }
            Divider()
        }
        if !chosen.isEmpty {
            Button(removal(of: chosen)) { remove(Set(chosen.map(\.id))) }
                .disabled(chosen.contains(where: \.inUse))
        }
    }

    private func removal(of chosen: [AgentState.Image]) -> String {
        if chosen.allSatisfy({ !$0.ready }) {
            return chosen.count == 1 ? "Cancel Download" : "Cancel Downloads"
        }
        return chosen.count == 1 ? "Remove Image" : "Remove Images"
    }

    /// The selected images that can go: none a restore is reading.
    private func removable(_ ids: Set<AgentState.Image.ID>) -> [AgentState.Image] {
        model.state?.images.images.filter { ids.contains($0.id) && !$0.inUse } ?? []
    }

    private func remove(_ ids: Set<AgentState.Image.ID>) {
        let going = removable(ids)
        guard !going.isEmpty else { return }
        model.perform(going.map { .remove(image: $0.id) })
        selection.subtract(going.map(\.id))
    }

    private func summary(of images: [AgentState.Image]) -> String {
        let held = images.filter(\.ready)
        guard !held.isEmpty else { return "0 images" }
        let size = held.reduce(0) { $0 + $1.bytes }.formatted(.byteCount(style: .file))
        return "\(held.count) image\(held.count == 1 ? "" : "s") · \(size)"
    }
}

extension AgentState.Image {
    var deviceTitle: String {
        let names = models.map { $0.replacing("WiFi", with: "Wi-Fi") }
        if os == "macOS", names.count != 1 {
            return "Mac"
        }
        guard let first = names.first else { return id }
        guard names.count > 1 else { return first }
        if names.count == 2 {
            if let wifi = names.first(where: { $0.contains("Wi-Fi") }),
               names.contains(wifi.replacing("Wi-Fi", with: "Cellular"))
            {
                let family = wifi.replacing("(Wi-Fi, ", with: "(")
                    .replacing(", Wi-Fi, ", with: ", ")
                    .replacing(", Wi-Fi)", with: ")")
                    .replacing(" (Wi-Fi)", with: "")
                if family != wifi {
                    return family
                }
            }
            return names.formatted(.list(type: .and))
        }
        return "\(first) + \(names.count - 1) more"
    }
}

private struct ImageRow: View {
    let image: AgentState.Image
    let fetch: AgentState.Fetch?
    let cancel: () -> Void

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: DeviceSymbol.system(image.os))
                .font(.title2)
                .foregroundStyle(.secondary)
                .frame(width: 30)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text(image.deviceTitle)
                    .lineLimit(2)
                    .help(image.models.joined(separator: "\n"))
                Text("\(image.title) · \(image.build) · \(image.bytes.formatted(.byteCount(style: .file)))")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .monospacedDigit()
                if !image.ready, let error = image.error {
                    Text(error)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                        .help(error)
                }
            }
            Spacer(minLength: 4)
            if !image.ready {
                ActivityMark(activity: status)
                Button("Cancel Download", systemImage: "xmark.circle", action: cancel)
                    .labelStyle(.iconOnly)
                    .buttonStyle(.borderless)
                    .help("Do not download this image")
            }
        }
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

/// What an image's row leaves out: where it came from and every model it
/// restores.
private struct ImageInfo: View {
    let image: AgentState.Image
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(spacing: 0) {
            Form {
                Section {
                    LabeledContent("Build", value: image.build)
                    LabeledContent("Size", value: image.bytes.formatted(.byteCount(style: .file)))
                    LabeledContent("Source", value: image.fetched ? "Downloaded from Apple" : "Added from a file")
                } header: {
                    Label(image.title, systemImage: DeviceSymbol.system(image.os))
                        .font(.headline)
                } footer: {
                    if image.inUse {
                        Text("A restore is using this image. It can be removed once the restore ends.")
                    }
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
