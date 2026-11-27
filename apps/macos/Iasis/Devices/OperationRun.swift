import Foundation

extension AgentState.OperationRun {
    var inProgress: Bool {
        steps.contains { $0.state == .active || $0.state == .pending }
    }

    var activity: Activity? {
        if let active = steps.first(where: { $0.state == .active }) {
            return active.activity
        }
        if let problem = steps.first(where: { $0.state == .failed || $0.state == .unconfirmed }) {
            return problem.activity
        }
        if let pending = steps.first(where: { $0.state == .pending }) {
            return pending.activity
        }
        if let skipped = steps.first(where: { $0.state == .skipped }) {
            return skipped.activity
        }
        if steps.count > 1 {
            return Activity("Requested Steps Complete", .done)
        }
        return steps.first?.activity
    }
}

extension AgentState.OperationStep {
    var detail: String? {
        if let system {
            return "\(system.os) \(system.version) (\(system.build))"
        }
        if let language {
            let region = Locale(identifier: language.locale).region.flatMap { Locale.current.localizedString(forRegionCode: $0.identifier) }
            return "\(region ?? language.locale) · \(language.language)"
        }
        return nil
    }

    var activity: Activity {
        switch state {
        case .pending: return Activity("\(kind.title) Pending", .pending)
        case .active:
            if let progress = activeInstallProgress {
                let symbol = kind == .restore ? DeviceSymbol.restore : DeviceSymbol.revive
                return Activity("\(kind.activeTitle) \(progress.percent)%", .working(Double(progress.percent) / 100), symbol: "\(symbol).circle")
            }
            return Activity(kind.activeTitle, .working(nil))
        case .complete: return Activity(kind.completedTitle, .done)
        case .failed: return Activity("\(kind.title) Failed", .failed)
        case .unconfirmed: return Activity("\(kind.title) Unconfirmed", .uncertain)
        case .skipped: return Activity("\(kind.title) Skipped", .skipped)
        }
    }

    var activeInstallProgress: AgentState.Progress? {
        state == .active && (kind == .restore || kind == .revive) ? progress : nil
    }
}

extension AgentState.Progress {
    var activityCaption: String? {
        guard let first = name.first else { return nil }
        return first.uppercased() + name.dropFirst().lowercased() + "…"
    }
}

extension AgentState.StepKind {
    var title: String {
        switch self {
        case .restore: "Restore"
        case .revive: "Revive"
        case .setRegion: "Set Region"
        case .requestEnrollment: "Request Enrollment"
        }
    }

    fileprivate var activeTitle: String {
        switch self {
        case .restore: "Restoring"
        case .revive: "Reviving"
        case .setRegion: "Setting Language and Region"
        case .requestEnrollment: "Requesting Enrollment"
        }
    }

    fileprivate var completedTitle: String {
        switch self {
        case .restore: "Restore Complete"
        case .revive: "Revive Complete"
        case .setRegion: "Language and Region Set"
        case .requestEnrollment: "Enrollment Requested"
        }
    }
}
