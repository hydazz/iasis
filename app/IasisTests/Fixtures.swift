import Foundation
@testable import Iasis

/// A state as the agent sends it: one board with a port in each condition
/// the panel has to show, and a download under way.
nonisolated enum Fixtures {
    static let stateLine = #"""
    {"state":{"boards":[{"serial":"a0e012192cdfdb67","firmware":"0.1.0","supply":"power supply 20 V, 67 W for targets","host":true,"restores":true,"brightness":50,"notice":null,"ports":[
    {"number":1,"light":{"hue":"off","pulse":false,"dim":false},"state":"empty","usb":"","link":"","contract":"","dfu":false,"reboot":false,"restore":false,"enabled":true,"job":null,"result":null},
    {"number":2,"light":{"hue":"blue","pulse":false,"dim":false},"state":"ready","usb":"DFU, debug USB 001a79d10e6bc01c","link":"USB 2","contract":"9 V 3 A","dfu":true,"reboot":true,"restore":true,"enabled":true,"job":null,"result":null},
    {"number":3,"light":{"hue":"purple","pulse":true,"dim":false},"state":"restoring 43%","usb":"system","link":"USB 2","contract":"5 V 3 A","dfu":false,"reboot":false,"restore":false,"enabled":true,"job":"Running","result":null},
    {"number":4,"light":{"hue":"green","pulse":false,"dim":false},"state":"ready","usb":"system","link":"USB 3","contract":"9 V 2.6 A","dfu":true,"reboot":true,"restore":true,"enabled":true,"job":null,"result":{"ecid":"0002250e3c04c01e","phase":"Reported","message":"Restore service reported completion"}},
    {"number":5,"light":{"hue":"red","pulse":false,"dim":false},"state":"fault","usb":"","link":"","contract":"","dfu":false,"reboot":false,"restore":false,"enabled":true,"job":null,"result":null}]}],
    "images":{"images":[{"path":"/cache/26B131-UniversalMac_27.2.1_26B131_Restore.ipsw","bytes":25595954706,"modified":{"secs_since_epoch":1795138779,"nanos_since_epoch":501524796},"product_version":"27.2.1","build_version":"26B131","product_types":["Mac13,1","Mac13,2"],"erase_hardware":[[24641,6]],"fetched":false}],
    "models":[{"identifier":"iPad13,18","name":"iPad (WiFi, 10th generation)","latest":{"version":"27.2.1","build":"24B156","url":"https://updates.cdn-apple.com/x/iPad_Fall_2022_27.2.1_24B156_Restore.ipsw","sha256":"b546","bytes":10941876409},"error":null,"ready":false}],
    "fetch":{"version":"27.2.1","build":"24B156","bytes":10941876409,"done":4704986855}},
    "restore_problem":null,"brightness":50}}
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
    private let reports: [AgentState]
    private let refusal: String?

    init(reports: [AgentState] = [], refusal: String? = nil) {
        self.reports = reports
        self.refusal = refusal
    }

    var received: [AgentRequest] {
        requests
    }

    nonisolated func states() -> AsyncThrowingStream<AgentState, any Error> {
        let reports = reports
        return AsyncThrowingStream { continuation in
            for report in reports {
                continuation.yield(report)
            }
            // The connection stays open, as a running agent's does.
        }
    }

    func send(_ request: AgentRequest) async throws {
        requests.append(request)
        if let refusal {
            throw AgentError.refused(refusal)
        }
    }
}
