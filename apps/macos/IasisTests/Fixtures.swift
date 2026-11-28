import Foundation
@testable import Iasis

/// A state as the agent sends it: one hub with a port in each condition
/// the panel has to show, and a download under way.
nonisolated enum Fixtures {
    static let stateLine = #"""
    {"state":{"version":"0.1.0","hubs":[{"serial":"a0e012192cdfdb67","name":"Repair Bench","session":77,"firmware":"0.1.0","upstream_mbps":10000,"supply":"brick","pool":{"budget_mw":70443,"allocated_mw":68970},"host":true,"restores":true,"brightness":50,"notice":null,"ports":[
    {"number":1,"light":{"hue":"off","pulse":false,"dim":false},"occupied":false,"generation":1,"usb":null,"link":null,"power":null,"allocated_mw":0,"short":false,"fault":null,"dfu":false,"reboot":false,"restore":false,"enabled":true,"job":null,"identifying":false,"waiting_for_power":false,"powered":false,"wrong_dfu_port":false,"run":null},
    {"number":2,"light":{"hue":"blue","pulse":false,"dim":false},"occupied":true,"generation":2,"device":"MacBook Air (13-inch, M5)","mode":"kis_dfu","ecid":"001a79d10e6bc01c","usb":"apple","link":"usb2","power":{"source":"pd","millivolts":9000,"milliamps":3000},"allocated_mw":28350,"short":false,"fault":null,"dfu":false,"reboot":true,"restore":false,"enabled":true,"job":null,"identifying":false,"waiting_for_power":false,"powered":true,"wrong_dfu_port":false,"run":null},
    {"number":3,"light":{"hue":"purple","pulse":true,"dim":false},"occupied":true,"generation":3,"device":"iPad","mode":"restored","usb":"apple","link":"usb2","power":{"source":"pd","millivolts":5000,"milliamps":3000},"allocated_mw":16050,"short":true,"fault":null,"dfu":false,"reboot":false,"restore":false,"enabled":true,"job":{"phase":"Running","operation":"restore"},"identifying":false,"waiting_for_power":false,"powered":true,"wrong_dfu_port":false,"run":{"ecid":null,"steps":[{"kind":"restore","state":"active","system":{"os":"iPadOS","version":"27.2.1","build":"24B156"},"progress":{"name":"Sending Filesystem","percent":43,"steps":["Detecting Device","Preparing Device","Sending Filesystem","Verifying Filesystem"],"step":2}},{"kind":"set_region","state":"pending","language":{"language":"en-AU","locale":"en_AU"}},{"kind":"request_enrollment","state":"pending"}]}},
    {"number":4,"light":{"hue":"green","pulse":false,"dim":false},"occupied":true,"generation":4,"ecid":"0002250e3c04c01e","usb":"apple","link":"usb3","power":{"source":"pd","millivolts":9000,"milliamps":2600},"allocated_mw":24570,"short":false,"fault":null,"dfu":true,"reboot":true,"restore":true,"enabled":true,"job":null,"identifying":false,"waiting_for_power":false,"powered":true,"wrong_dfu_port":false,"run":{"ecid":"0002250e3c04c01e","steps":[{"kind":"restore","state":"complete","system":{"os":"macOS","version":"27.2.1","build":"26B131"},"progress":{"name":"Complete","percent":100,"steps":["Detecting Device","Preparing Device","Sending Filesystem","Verifying Filesystem"],"step":4}}]}},
    {"number":5,"light":{"hue":"red","pulse":false,"dim":false},"occupied":true,"generation":5,"usb":null,"link":null,"power":null,"allocated_mw":0,"short":false,"fault":"Protection tripped","dfu":false,"reboot":false,"restore":false,"enabled":true,"job":null,"identifying":false,"waiting_for_power":false,"powered":false,"wrong_dfu_port":false,"run":null}]}],
    "images":{"images":[
    {"id":"UniversalMac_27.2.1_26B131_Restore.ipsw","os":"macOS","version":"27.2.1","build":"26B131","bytes":25595954706,"models":["MacBook Air (M1, Late 2020)","Mac mini (M1, Late 2020)","MacBook Pro (M3 Max, 14-inch, Nov 2023)"],"ready":true,"error":null,"path":"/Users/Shared/Iasis/images/UniversalMac_27.2.1_26B131_Restore.ipsw","fetched":true,"in_use":false},
    {"id":"iPad_Fall_2022_27.2.1_24B156_Restore.ipsw","os":"iPadOS","version":"27.2.1","build":"24B156","bytes":10941876409,"models":["iPad (WiFi, 10th generation)"],"ready":false,"error":null,"path":null,"fetched":true,"in_use":false}],
    "fetch":{"id":"iPad_Fall_2022_27.2.1_24B156_Restore.ipsw","version":"27.2.1","build":"24B156","bytes":10941876409,"done":4704986855}},
    "post_restore":{"shutdown_macs":false,"setup_language":null,"try_enrollment":false},
    "sharing":{"enabled":true,"problem":null,
    "ports":[{"hub":"a0e012192cdfdb67","port":3,"active":true},{"hub":"a0e012192cdfdb67","port":4,"active":false,"problem":"Cannot open USB interface"}]},
    "supervision":{"name":"Iasis on bench","certificate":"MIIBszCCAVmgAwIBAgIIb3Vy","problem":null},
    "missing":[]}}
    """#

    static func decode(_ line: String) throws -> AgentReply {
        try AgentReply(line: Data(line.utf8))
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
    private var refused: (AgentRequest, String)?
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

    func fail(with error: any Error) {
        stream.continuation.finish(throwing: error)
    }

    func send(_ request: AgentRequest) async throws {
        requests.append(request)
        if let refused, refused.0 == request {
            throw AgentError.refused(refused.1)
        }
    }

    func refuse(_ request: AgentRequest, reason: String = "The selection cannot advance through setup.") {
        refused = (request, reason)
    }

    func catalog() async throws -> [AgentState.CatalogModel] {
        []
    }

    func releases(of _: String) async throws -> [AgentState.Release] {
        []
    }

    func exportSupervision(password _: String) async throws -> String {
        ""
    }

    func plan(_ hubs: [RestoreSelection]) async throws -> [AgentState.Planned] {
        requests.append(.plan(hubs: hubs))
        return []
    }

    func importImage(_: URL, progress _: @escaping @Sendable (Int64) -> Void) async throws {}
}
