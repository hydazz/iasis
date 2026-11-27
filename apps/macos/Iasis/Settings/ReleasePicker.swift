import SwiftUI

struct ReleasePicker: View {
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
