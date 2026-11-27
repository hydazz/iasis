@testable import Iasis
import Testing

struct ImageSettingsTests {
    @Test func imageTitlesDistinguishDeviceModels() throws {
        let images = try Fixtures.state.images.images
        #expect(images[0].deviceTitle == "Mac")
        var ipad = images[1]
        #expect(ipad.deviceTitle == "iPad (Wi-Fi, 10th generation)")

        ipad.models.append("iPad (Cellular, 10th generation)")
        #expect(ipad.deviceTitle == "iPad (10th generation)")

        ipad.models = ["iPad (WiFi, 9th generation)", "iPad (Cellular, 10th generation)"]
        #expect(ipad.deviceTitle.contains("9th generation"))
        #expect(ipad.deviceTitle.contains("10th generation"))
    }
}
