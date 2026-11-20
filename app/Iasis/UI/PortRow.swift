import SwiftUI

/// One port: its light, what the board and the agent say about its target,
/// and the actions the target would take now.
struct PortRow: View {
    let port: AgentState.Port
    let id: PortID
    let model: AgentModel

    var body: some View {
        Group {
            if model.confirming == id {
                EraseConfirmation(port: port.number, model: model)
            } else {
                summary
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 3)
        .frame(minHeight: 30)
    }

    private var summary: some View {
        HStack(spacing: 8) {
            LightDot(light: port.light)
            Text(port.number, format: .number)
                .monospacedDigit()
                .foregroundStyle(.secondary)
            VStack(alignment: .leading, spacing: 0) {
                Text(port.state.capitalizingFirst)
                    .foregroundStyle(occupied ? .primary : .secondary)
                if !detail.isEmpty {
                    Text(detail)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
            }
            Spacer(minLength: 0)
            if let result = port.result, port.job == nil {
                EndingMark(ending: result)
            }
            actions
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Port \(port.number), \(port.state)")
    }

    private var occupied: Bool {
        port.light.hue != .off
    }

    /// What is on the port's USB, its link and its power contract.
    private var detail: String {
        [port.usb, port.link, port.contract]
            .filter { !$0.isEmpty }
            .joined(separator: " · ")
    }

    private var actions: some View {
        Menu("Port \(port.number) Actions", systemImage: "ellipsis.circle") {
            Button("Enter DFU") {
                model.perform(.dfu(board: id.board, port: id.port))
            }
            .disabled(!port.dfu)
            Button("Restart") {
                model.perform(.reboot(board: id.board, port: id.port))
            }
            .disabled(!port.reboot)
            Button("Erase and Restore…", role: .destructive) {
                model.askToRestore(id)
            }
            .disabled(!port.restore)
        }
        .labelStyle(.iconOnly)
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
    }
}

/// The question before an erase, in the row it concerns. The panel closes
/// when it loses focus, which a separate alert would take from it.
private struct EraseConfirmation: View {
    let port: Int
    let model: AgentModel

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundStyle(.yellow)
            Text("Erase the device on port \(port)?")
            Spacer(minLength: 0)
            Button("Cancel", role: .cancel) {
                model.cancelRestore()
            }
            Button("Erase", role: .destructive) {
                model.confirmRestore()
            }
            .buttonStyle(.borderedProminent)
            .tint(.red)
        }
        .controlSize(.small)
    }
}

/// How the port's last restore ended, once the board has forgotten it.
private struct EndingMark: View {
    let ending: AgentState.Ending

    var body: some View {
        Image(systemName: ending.phase == .reported ? "checkmark.circle.fill" : "exclamationmark.circle.fill")
            .foregroundStyle(ending.phase == .reported ? .green : .red)
            .help(ending.message)
            .accessibilityLabel(ending.phase == .reported ? "Restored" : "Restore failed")
    }
}

private extension String {
    var capitalizingFirst: String {
        prefix(1).uppercased() + dropFirst()
    }
}
