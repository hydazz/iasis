import Foundation
@testable import Iasis
import Testing

/// The agent's example of every message, from `crates/api/fixtures`: the app
/// reads each state and reply without losing a member, and writes each
/// request as the example has it.
struct ContractTests {
    private static let fixtures = URL(filePath: #filePath)
        .deletingLastPathComponent()
        .appending(path: "../../../crates/api/fixtures")
        .standardized

    @Test func theStateReadsWhole() throws {
        let json = try fixture("state.json")
        let reply = try AgentReply(line: Data(#"{"state":"#.utf8) + json + Data("}".utf8))
        guard case let .state(state) = reply else {
            Issue.record("read as \(reply)")
            return
        }
        #expect(try members(written(state)) == members(json))
    }

    @Test func everyReplyReadsWhole() throws {
        let examples = try #require(JSONSerialization.jsonObject(with: fixture("replies.json")) as? [Any])
        for example in examples {
            let line = try JSONSerialization.data(withJSONObject: example, options: .fragmentsAllowed)
            #expect(try members(written(AgentReply(line: line))) == members(line))
        }
    }

    @Test func everyRequestIsWrittenAsTheAgentReadsIt() throws {
        let hub = "817a23bd502453b4"
        let identity = "MIIJ4QIBAzCCCZcGCSqGSIb3DQEHAaCCCYgEggmEMIIJgDCCBDcGCSqGSIb3DQEH"
        let selection = [RestoreSelection(hub: hub, session: 3, targets: [.init(port: 2, generation: 7, ecid: "0002250e3c04c01e")])]
        let setup = SetupOptions(language: "en-AU", locale: "en_AU", enroll: true)
        let requests: [AgentRequest] = [
            .state,
            .watch,
            .plan(hubs: selection),
            .restore(hubs: selection, latest: true, after: AfterErase(shutdownMacs: true, setup: setup)),
            .revive(hubs: selection, latest: false),
            .dfu(hubs: selection),
            .reboot(hubs: selection),
            .advanceSetup(hubs: selection, setup: setup),
            .power(hub: hub, port: 2, on: true),
            .debugUsb(hub: hub, port: 1, on: true),
            .rename(hub: hub, name: "Bench A"),
            .brightness(hub: hub, percent: 50),
            .update(hub: hub, bytes: 518_144),
            .sharing(enabled: true),
            .importSupervision(pkcs12: identity, password: "correct horse"),
            .exportSupervision(password: "correct horse"),
            .download(model: "MacBookAir10,1", build: "26B131"),
            .releases(model: "MacBookAir10,1"),
            .importImage(name: "UniversalMac_27.2.1_26B131_Restore.ipsw", bytes: 19_000_000_000),
            .remove(image: "26B131-UniversalMac_27.2.1_26B131_Restore.ipsw"),
            .catalog,
            .diagnostics(hub: hub),
        ]
        let lines = try requests.map { try $0.line() }
        let sent = Data("[".utf8) + lines.joined(separator: Data(",".utf8)) + Data("]".utf8)
        #expect(try members(sent) == members(fixture("requests.json")))
    }

    private func fixture(_ name: String) throws -> Data {
        try Data(contentsOf: Self.fixtures.appending(path: name))
    }

    /// A reply as the agent writes it, from what the app read.
    private func written(_ reply: AgentReply) throws -> Data {
        switch reply {
        case .ok: try written("ok")
        case let .error(message): try written(["error": message])
        case let .state(state): try written(["state": state])
        case let .catalog(models): try written(["catalog": models])
        case let .releases(releases): try written(["releases": releases])
        case let .plan(planned): try written(["plan": planned])
        case let .supervision(identity): try written(["supervision": identity])
        }
    }

    /// The agent's names for what the app reads with `convertFromSnakeCase`.
    private func written(_ value: some Encodable) throws -> Data {
        let encoder = JSONEncoder()
        encoder.keyEncodingStrategy = .convertToSnakeCase
        encoder.dateEncodingStrategy = .iso8601
        return try encoder.encode(value)
    }

    /// The JSON without its null members, which the agent writes and Swift
    /// leaves out.
    private func members(_ json: Data) throws -> NSObject {
        func present(_ value: Any) -> Any {
            switch value {
            case let object as [String: Any]: object.filter { !($0.value is NSNull) }.mapValues(present)
            case let array as [Any]: array.map(present)
            default: value
            }
        }
        return try present(JSONSerialization.jsonObject(with: json, options: .fragmentsAllowed)) as! NSObject
    }
}
