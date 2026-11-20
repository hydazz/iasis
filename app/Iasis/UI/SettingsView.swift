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
    @State private var brightness = 50.0
    @State private var dragging = false
    @State private var revealsDevices = FinderReveal.isEnabled

    var body: some View {
        Form {
            Section("Lights") {
                Slider(value: $brightness, in: 10 ... 100, step: 5) {
                    Text("Brightness")
                } minimumValueLabel: {
                    Image(systemName: "sun.min")
                } maximumValueLabel: {
                    Image(systemName: "sun.max")
                } onEditingChanged: { editing in
                    dragging = editing
                    if !editing {
                        model.perform(.brightness(percent: Int(brightness)))
                    }
                }
                .disabled(model.state == nil)
            }
            Section {
                Toggle("Open Finder when a device connects", isOn: $revealsDevices)
            } footer: {
                Text("Finder opens a window for each device that enters DFU. This is an undocumented macOS setting and applies to every Apple device you connect.")
            }
        }
        .formStyle(.grouped)
        .onChange(of: model.state?.brightness, initial: true) { _, agent in
            // The agent's level is the truth, except under the user's hand.
            if let agent, !dragging {
                brightness = Double(agent)
            }
        }
        .onChange(of: revealsDevices) { _, reveals in
            FinderReveal.set(reveals)
        }
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
