import Foundation
@testable import Iasis
import Testing

struct AgentMessagesTests {
    @Test func aStateDecodesAsTheAgentSendsIt() throws {
        let state = try Fixtures.state
        let hub = try #require(state.hubs.first)
        #expect(hub.serial == "a0e012192cdfdb67")
        #expect(hub.ports.map(\.number) == [1, 2, 3, 4, 5])
        #expect(hub.name == "Repair Bench")
        #expect(hub.firmware == "0.1.0")
        #expect(hub.upstreamMbps == 10000)
        #expect(hub.pool == .init(budgetMw: 70443, allocatedMw: 68970))
        #expect(hub.ports[2].power == .init(source: .pd, millivolts: 5000, milliamps: 3000))
        #expect(hub.ports[2].short)
        #expect(hub.ports[4].fault == "Protection tripped")
        #expect(hub.ports[1].ecid == "001a79d10e6bc01c")
        #expect(hub.ports[2].run?.steps[0].progress?.percent == 43)
        #expect(hub.ports[3].run?.steps.first?.state == .complete)
        #expect(state.images.images.map(\.title) == ["macOS 27.2.1", "iPadOS 27.2.1"])
        #expect(state.images.images.map(\.ready) == [true, false])
        #expect(state.images.fetching?.build == "24B156")
        let fraction = try #require(state.images.fetch?.fraction)
        #expect(abs(fraction - 0.43) < 0.001)
        #expect(state.sharing.enabled)
        #expect(state.sharing.ports[0] == .init(hub: hub.serial, port: 3, active: true))
    }

    /// Against the agent running on this Mac, when `IASIS_LIVE_AGENT` is set.
    @Test(.enabled(if: ProcessInfo.processInfo.environment["IASIS_LIVE_AGENT"] != nil))
    func aRunningAgentsStateDecodes() async throws {
        let asked = ContinuousClock.now
        var states = AgentClient().states().makeAsyncIterator()
        let state = try #require(try await states.next())
        #expect(state.hubs.allSatisfy { $0.ports.count == 5 || $0.notice != nil })
        // The agent answers at once; a reader must not sit on the answer.
        #expect(ContinuousClock.now - asked < .seconds(1))
        _ = try await AgentClient().catalog()
    }

    @Test func theOtherRepliesDecode() throws {
        #expect(try Fixtures.decode(#""ok""#) == .ok)
        #expect(try Fixtures.decode(#"{"error":"no such hub"}"#) == .error("no such hub"))
        let catalog = #"{"catalog":[{"identifier":"iPad13,18","name":"iPad (10th generation)","hardware":[[32816,30]]}]}"#
        #expect(try Fixtures.decode(catalog) == .catalog([.init(identifier: "iPad13,18", name: "iPad (10th generation)")]))
        let releases = #"{"releases":[{"os":"macOS","version":"26.6","build":"25G80","bytes":3}]}"#
        #expect(try Fixtures.decode(releases) == .releases([.init(os: "macOS", version: "26.6", build: "25G80", bytes: 3)]))
        let plan = #"{"plan":[{"hub":"a0e0","port":2,"offer":{"latest":{"os":"macOS","version":"27.2.1","build":"26B131","bytes":4},"held":null,"problem":null}},{"hub":"a0e0","port":3,"offer":null}]}"#
        let latest = AgentState.Release(os: "macOS", version: "27.2.1", build: "26B131", bytes: 4)
        #expect(try Fixtures.decode(plan) == .plan([
            .init(hub: "a0e0", port: 2, offer: .init(latest: latest)),
            .init(hub: "a0e0", port: 3),
        ]))
    }

    @Test(arguments: [
        (AgentRequest.watch, #"{"op":"watch"}"#),
        (
            .restore(hubs: [.init(hub: "a0e0", session: 77, targets: [.init(port: 2, generation: 4), .init(port: 3, generation: 1, ecid: "0014692221e0801e")])], latest: true, after: AfterErase(shutdownMacs: false)),
            #"{"after":{"shutdown_macs":false},"hubs":[{"hub":"a0e0","session":77,"targets":[{"generation":4,"port":2},{"ecid":"0014692221e0801e","generation":1,"port":3}]}],"latest":true,"op":"restore"}"#
        ),
        (
            .revive(hubs: [.init(hub: "a0e0", session: 77, targets: [.init(port: 2, generation: 4)])], latest: false),
            #"{"hubs":[{"hub":"a0e0","session":77,"targets":[{"generation":4,"port":2}]}],"latest":false,"op":"revive"}"#
        ),
        (
            .dfu(hubs: [.init(hub: "a0e0", session: 77, targets: [.init(port: 2, generation: 4)])]),
            #"{"hubs":[{"hub":"a0e0","session":77,"targets":[{"generation":4,"port":2}]}],"op":"dfu"}"#
        ),
        (.power(hub: "a0e0", port: 1, on: false), #"{"hub":"a0e0","on":false,"op":"power","port":1}"#),
        (.sharing(enabled: true), #"{"enabled":true,"op":"sharing"}"#),
        (.sharing(enabled: false), #"{"enabled":false,"op":"sharing"}"#),
        (.brightness(hub: "a0e0", percent: 30), #"{"hub":"a0e0","op":"brightness","percent":30}"#),
        (.rename(hub: "a0e0", name: "Bench A"), #"{"hub":"a0e0","name":"Bench A","op":"rename"}"#),
        (.importImage(name: "a.ipsw", bytes: 3), #"{"bytes":3,"name":"a.ipsw","op":"import"}"#),
        (.remove(image: "a.ipsw"), #"{"image":"a.ipsw","op":"remove"}"#),
        (.download(model: "Macmini9,1", build: "25G80"), #"{"build":"25G80","model":"Macmini9,1","op":"download"}"#),
        (.plan(hubs: [.init(hub: "a0e0", session: 77, targets: [.init(port: 2, generation: 4)])]), #"{"hubs":[{"hub":"a0e0","session":77,"targets":[{"generation":4,"port":2}]}],"op":"plan"}"#),
        (.releases(model: "Macmini9,1"), #"{"model":"Macmini9,1","op":"releases"}"#),
        (.catalog, #"{"op":"catalog"}"#),
    ])
    func aRequestEncodesAsTheAgentReadsIt(request: AgentRequest, expected: String) throws {
        let encoder = JSONEncoder()
        encoder.outputFormatting = .sortedKeys
        #expect(try String(decoding: encoder.encode(request), as: UTF8.self) == expected)
    }
}
