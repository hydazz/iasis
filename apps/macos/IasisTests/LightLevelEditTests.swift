@testable import Iasis
import Testing

struct LightLevelEditTests {
    @Test func aLateReadingCannotMoveTheThumbBackAfterRelease() {
        var level = LightLevelEdit()
        level.drag(to: 63.8)
        #expect(level.release(reading: 50) == 64)
        #expect(level.value(reading: 50) == 64)
        level.drag(to: 72.2)
        #expect(level.release(reading: 50) == 72)
        level.reconcile(64)
        #expect(level.value(reading: 64) == 72)
        level.reconcile(72)
        #expect(level.value(reading: 58) == 58)
    }

    @Test func aRejectedWriteRestoresTheBoardReading() {
        var level = LightLevelEdit()
        level.drag(to: 53)
        _ = level.release(reading: 40)
        level.reconcile(53)
        #expect(level.value(reading: 40) == 40)
    }
}
