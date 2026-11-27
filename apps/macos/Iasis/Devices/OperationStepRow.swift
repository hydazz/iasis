import SwiftUI

struct OperationStepRow: View {
    let step: AgentState.OperationStep
    let waitingForImage: Bool

    var body: some View {
        let progress = step.activeInstallProgress
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            mark
                .frame(width: 16)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 4) {
                HStack(alignment: .firstTextBaseline) {
                    Text(step.kind.title)
                        .foregroundStyle(step.state == .pending || step.state == .skipped ? .secondary : .primary)
                    Spacer(minLength: 2)
                    if let progress {
                        Text("\(progress.percent)%").monospacedDigit().foregroundStyle(.secondary)
                    }
                }
                if let detail = step.detail {
                    Text(detail).font(.callout).foregroundStyle(.secondary)
                }
                if let progress {
                    ProgressView(value: Double(progress.percent), total: 100)
                        .progressViewStyle(.linear)
                        .controlSize(.mini)
                        .tint(.accentColor)
                        .accessibilityHidden(true)
                }
                if let error = step.error {
                    Text(error).font(.callout).textSelection(.enabled)
                } else if waitingForImage, step.kind == .restore || step.kind == .revive, step.state == .active || step.state == .pending {
                    Text("Waiting for image…").font(.callout).foregroundStyle(.secondary)
                } else if let caption = progress?.activityCaption {
                    Text(caption).font(.callout).foregroundStyle(.secondary)
                }
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityValue(step.state.title)
    }

    @ViewBuilder private var mark: some View {
        if step.state == .pending {
            Image(systemName: "circle.fill").imageScale(.large).foregroundStyle(.tertiary)
        } else if step.state == .active, step.kind == .restore || step.kind == .revive {
            Image(systemName: step.kind == .restore ? DeviceSymbol.restore : DeviceSymbol.revive)
        } else {
            ActivityMark(activity: step.activity)
        }
    }
}

private extension AgentState.StepState {
    var title: String {
        switch self {
        case .pending: "Pending"
        case .active: "In Progress"
        case .complete: "Complete"
        case .onDevice: "Continue on Device"
        case .failed: "Failed"
        case .unconfirmed: "Unconfirmed"
        case .skipped: "Skipped"
        }
    }
}
