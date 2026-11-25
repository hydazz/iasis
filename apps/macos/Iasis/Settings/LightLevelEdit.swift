/// The watch can still have the previous hub reading after a save is acknowledged.
struct LightLevelEdit {
    private var dragValue: Double?
    private(set) var submitted: Int?

    func value(reading: Int) -> Double {
        dragValue ?? Double(submitted ?? reading)
    }

    mutating func drag(to value: Double) {
        dragValue = value
    }

    mutating func release(reading: Int) -> Int? {
        guard let dragValue else { return nil }
        self.dragValue = nil
        let percent = Int(dragValue.rounded())
        guard percent != (submitted ?? reading) else { return nil }
        submitted = percent
        return percent
    }

    mutating func reconcile(_ percent: Int) {
        if submitted == percent {
            submitted = nil
        }
    }
}
