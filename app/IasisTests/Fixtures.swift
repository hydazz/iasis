import Foundation
@testable import Iasis

/// A state as the agent sends it: one board with a port in each condition
/// the panel has to show, and a download under way.
nonisolated enum Fixtures {
    static let stateLine = #"""
    {"state":{"boards":[{"serial":"a0e012192cdfdb67","name":"Repair Bench","session":77,"firmware":"0.1.0","upstream_mbps":10000,"supply":"power supply 20 V, 70 W for targets","pool":{"budget_mw":70443,"allocated_mw":68970},"host":true,"restores":true,"brightness":50,"settings_busy":false,"notice":null,"ports":[
    {"number":1,"light":{"hue":"off","pulse":false,"dim":false},"occupied":false,"generation":1,"usb":"","link":null,"power":null,"allocated_mw":0,"short":false,"fault":null,"dfu":false,"reboot":false,"restore":false,"enabled":true,"job":null,"result":null,"identifying":false,"waiting_for_power":false,"powered":false,"wrong_dfu_port":false},
    {"number":2,"light":{"hue":"blue","pulse":false,"dim":false},"occupied":true,"generation":2,"device":"MacBook Air (13-inch, M5)","mode":"kis_dfu","ecid":"001a79d10e6bc01c","usb":"DFU, debug USB 001a79d10e6bc01c","link":"usb2","power":{"source":"pd","millivolts":9000,"milliamps":3000},"allocated_mw":28350,"short":false,"fault":null,"dfu":false,"reboot":true,"restore":false,"enabled":true,"job":null,"result":null,"identifying":false,"waiting_for_power":false,"powered":true,"wrong_dfu_port":false},
    {"number":3,"light":{"hue":"purple","pulse":true,"dim":false},"occupied":true,"generation":3,"device":"iPad","mode":"mux","stage":{"name":"Sending Filesystem","percent":43},"usb":"system","link":"usb2","power":{"source":"pd","millivolts":5000,"milliamps":3000},"allocated_mw":16050,"short":true,"fault":null,"dfu":false,"reboot":false,"restore":false,"enabled":true,"job":{"phase":"Running","operation":"restore"},"result":null,"identifying":false,"waiting_for_power":false,"powered":true,"wrong_dfu_port":false},
    {"number":4,"light":{"hue":"green","pulse":false,"dim":false},"occupied":true,"generation":4,"ecid":"0002250e3c04c01e","usb":"system","link":"usb3","power":{"source":"pd","millivolts":9000,"milliamps":2600},"allocated_mw":24570,"short":false,"fault":null,"dfu":true,"reboot":true,"restore":true,"enabled":true,"job":null,"result":{"operation":"restore","log_directory":"/tmp/iasis-logs/repair","ecid":"0002250e3c04c01e","phase":"Reported","message":"Restore service reported completion"},"identifying":false,"waiting_for_power":false,"powered":true,"wrong_dfu_port":false},
    {"number":5,"light":{"hue":"red","pulse":false,"dim":false},"occupied":true,"generation":5,"usb":"","link":null,"power":null,"allocated_mw":0,"short":false,"fault":"Protection tripped","dfu":false,"reboot":false,"restore":false,"enabled":true,"job":null,"result":null,"identifying":false,"waiting_for_power":false,"powered":false,"wrong_dfu_port":false}]}],
    "images":{"images":[{"path":"/cache/26B131-UniversalMac_27.2.1_26B131_Restore.ipsw","bytes":25595954706,"modified":{"secs_since_epoch":1795138779,"nanos_since_epoch":501524796},"product_version":"27.2.1","build_version":"26B131","product_types":["Mac13,1","Mac13,2"],"erase_hardware":[[24641,6]],"fetched":false}],
    "models":[{"identifier":"iPad13,18","name":"iPad (WiFi, 10th generation)","latest":{"version":"27.2.1","build":"24B156","url":"https://updates.cdn-apple.com/x/iPad_Fall_2022_27.2.1_24B156_Restore.ipsw","sha256":"b546","bytes":10941876409},"error":null,"ready":false}],
    "fetch":{"version":"27.2.1","build":"24B156","bytes":10941876409,"done":4704986855}},
    "sharing":{"enabled":true,"problem":null,
    "ports":[{"board":"a0e012192cdfdb67","port":3,"active":true},{"board":"a0e012192cdfdb67","port":4,"active":false,"problem":"Cannot open USB interface"}]}}}
    """#

    static func decode(_ line: String) throws -> AgentReply {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(AgentReply.self, from: Data(line.utf8))
    }

    static var state: AgentState {
        get throws {
            guard case let .state(state) = try decode(stateLine) else {
                throw AgentError.unavailable
            }
            return state
        }
    }
}

/// An agent that reports what a test gives it and records what it is asked.
actor FakeAgent: AgentService {
    private var requests: [AgentRequest] = []
    private let stream = AsyncThrowingStream<AgentState, any Error>.makeStream()

    init(reports: [AgentState]) {
        for report in reports {
            stream.continuation.yield(report)
        }
    }

    var received: [AgentRequest] {
        requests
    }

    nonisolated func states() -> AsyncThrowingStream<AgentState, any Error> {
        stream.stream
    }

    func publish(_ state: AgentState) {
        stream.continuation.yield(state)
    }

    func send(_ request: AgentRequest) async throws {
        requests.append(request)
    }
}
