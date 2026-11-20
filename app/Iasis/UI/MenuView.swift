import SwiftUI

/// The menu bar item: a fault takes over the symbol.
struct MenuBarLabel: View {
    let model: AgentModel

    var body: some View {
        Image(systemName: model.hasFault ? "exclamationmark.triangle" : "cable.connector.horizontal")
            .accessibilityLabel("Iasis")
    }
}

/// The panel under the menu bar item: every board and its ports.
struct MenuView: View {
    let model: AgentModel

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let state = model.state {
                if state.boards.isEmpty {
                    ContentUnavailableView(
                        "No Boards",
                        systemImage: "cable.connector.horizontal",
                        description: Text("Connect an Iasis board to this Mac.")
                    )
                    .padding(.vertical, 8)
                }
                ForEach(state.boards) { board in
                    BoardSection(board: board, model: model)
                    Divider()
                }
                StoreStatus(images: state.images, restoreProblem: state.restoreProblem)
            } else {
                ContentUnavailableView(
                    "Agent Not Running",
                    systemImage: "bolt.slash",
                    description: Text("Iasis starts its agent when you log in.")
                )
                .padding(.vertical, 8)
                Divider()
            }
            if let failure = model.failure {
                FailureBanner(message: failure) {
                    model.dismissFailure()
                }
                Divider()
            }
            MenuFooter()
        }
        .frame(width: 340)
    }
}

/// A download under way, and why restores are unavailable.
private struct StoreStatus: View {
    let images: AgentState.ImageStore
    let restoreProblem: String?

    var body: some View {
        if let fetch = images.fetch {
            VStack(alignment: .leading, spacing: 4) {
                Text("Downloading \(fetch.version) (\(fetch.build))")
                    .font(.caption)
                ProgressView(value: fetch.fraction)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            Divider()
        }
        if let restoreProblem {
            Label(restoreProblem, systemImage: "exclamationmark.triangle")
                .font(.caption)
                .foregroundStyle(.secondary)
                .padding(.horizontal, 12)
                .padding(.vertical, 8)
            Divider()
        }
    }
}

private struct FailureBanner: View {
    let message: String
    let dismiss: () -> Void

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Label(message, systemImage: "xmark.octagon")
                .font(.caption)
                .foregroundStyle(.red)
            Spacer(minLength: 0)
            Button("Dismiss", systemImage: "xmark", action: dismiss)
                .labelStyle(.iconOnly)
                .buttonStyle(.borderless)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
    }
}

private struct MenuFooter: View {
    @Environment(\.openSettings) private var openSettings

    var body: some View {
        HStack {
            Button("Settings…") {
                openSettings()
                // The app has no Dock icon to bring its window forward.
                NSApplication.shared.activate()
            }
            Spacer()
            Button("Quit Iasis") {
                NSApplication.shared.terminate(nil)
            }
        }
        .buttonStyle(.borderless)
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
    }
}
