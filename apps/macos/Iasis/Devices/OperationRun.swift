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
        if let handoff = steps.first(where: { $0.state == .onDevice }) {
            return handoff.activity
        }
        if let pending = steps.first(where: { $0.state == .pending }) {
            return pending.activity
        }
        if let skipped = steps.first(where: { $0.state == .skipped }) {
            return skipped.kind == .setRegion ? Activity("Already Set Up", .done) : skipped.activity
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
            if let progress = activeRestoreProgress {
                let symbol = kind == .restore ? DeviceSymbol.restore : DeviceSymbol.revive
                return Activity(kind.activeTitle, .working(Double(progress.percent) / 100), symbol: "\(symbol).circle")
            }
            return Activity(kind.activeTitle, .working(nil))
        case .complete: return Activity(kind.completedTitle, .done)
        case .onDevice: return Activity("Continue on Device", .onDevice)
        case .failed: return Activity("\(kind.title) Failed", .failed)
        case .unconfirmed: return Activity("\(kind.title) Unconfirmed", .uncertain)
        case .skipped: return Activity("\(kind.title) Skipped", .skipped)
        }
    }

    var activeRestoreProgress: AgentState.Progress? {
        isActiveRestore ? progress : nil
    }

    var isActiveRestore: Bool {
        state == .active && (kind == .restore || kind == .revive)
    }
}

struct ActiveRestores {
    let count: Int
    let estimate: RestoreEstimate?

    init(devices: [Device]) {
        var finishes: [Date?] = []
        for device in devices {
            let run = device.run
            if let step = run?.steps.first(where: \.isActiveRestore) {
                finishes.append(device.port.awaiting == nil ? step.estimatedCompletionAt : nil)
            } else if run == nil, let job = device.port.job, job.phase == .running || job.phase == .preparing {
                finishes.append(nil)
            }
        }
        count = finishes.count
        let known = finishes.compactMap(\.self)
        if known.count == count, let first = known.min(), let last = known.max() {
            estimate = RestoreEstimate(finish: last, validUntil: first)
        } else {
            estimate = nil
        }
    }
}

struct RestoreEstimate {
    private let firstFinish: Date
    private let lastFinish: Date

    init(finish: Date, validUntil: Date? = nil) {
        firstFinish = validUntil ?? finish
        lastFinish = finish
    }

    func minutesRemaining(at date: Date) -> Int? {
        guard date < firstFinish else { return nil }
        let seconds = lastFinish.timeIntervalSince(date)
        return seconds < 60 ? 0 : Int((seconds / 60).rounded())
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
        case .activate: "Activate"
        case .requestEnrollment: "Request Enrollment"
        }
    }

    fileprivate var activeTitle: String {
        switch self {
        case .restore: "Restoring"
        case .revive: "Reviving"
        case .setRegion: "Setting Language and Region"
        case .activate: "Activating"
        case .requestEnrollment: "Requesting Enrollment"
        }
    }

    fileprivate var completedTitle: String {
        switch self {
        case .restore: "Restore Complete"
        case .revive: "Revive Complete"
        case .setRegion: "Language and Region Set"
        case .activate: "Activated"
        case .requestEnrollment: "Enrollment Requested"
        }
    }
}
