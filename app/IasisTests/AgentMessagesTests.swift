import Foundation
@testable import Iasis
import Testing

struct AgentMessagesTests {
    @Test func aStateDecodesAsTheAgentSendsIt() throws {
        let state = try Fixtures.state
        let board = try #require(state.boards.first)
        #expect(board.serial == "a0e012192cdfdb67")
        #expect(board.ports.map(\.number) == [1, 2, 3, 4, 5])
        #expect(board.ports[1].light == .init(hue: .blue, pulse: false, dim: false))
        #expect(board.ports[2].job == .running)
        #expect(board.ports[2].light.pulse)
        #expect(board.ports[3].result?.phase == .reported)
        #expect(state.images.images.first?.buildVersion == "26B131")
        #expect(state.images.models.first?.latest?.build == "24B156")
        let fraction = try #require(state.images.fetch?.fraction)
        #expect(abs(fraction - 0.43) < 0.001)
        #expect(state.restoreProblem == nil)
    }

    /// Against the agent running on this Mac, when `IASIS_LIVE_AGENT` is set.
    @Test(.enabled(if: ProcessInfo.processInfo.environment["IASIS_LIVE_AGENT"] != nil))
    func aRunningAgentsStateDecodes() async throws {
        let asked = ContinuousClock.now
        var states = AgentClient().states().makeAsyncIterator()
        let state = try #require(try await states.next())
        #expect(state.boards.allSatisfy { $0.ports.count == 5 || $0.notice != nil })
        // The agent answers at once; a reader must not sit on the answer.
        #expect(ContinuousClock.now - asked < .seconds(1))
        try await AgentClient().send(.refresh)
    }

    @Test func theOtherRepliesDecode() throws {
        #expect(try Fixtures.decode(#""ok""#) == .ok)
        #expect(try Fixtures.decode(#"{"error":"no such board"}"#) == .error("no such board"))
    }

    @Test func aHueFromANewerAgentIsAnUnidentifiedDevice() throws {
        let light = try JSONDecoder().decode(
            AgentState.Light.self,
            from: Data(#"{"hue":"teal","pulse":false,"dim":false}"#.utf8)
        )
        #expect(light.hue == .white)
    }

    @Test(arguments: [
        (AgentRequest.watch, #"{"op":"watch"}"#),
        (.restore(board: "a0e0", ports: [2, 3]), #"{"board":"a0e0","op":"restore","ports":[2,3]}"#),
        (.dfu(board: "a0e0", port: 2), #"{"board":"a0e0","op":"dfu","port":2}"#),
        (.power(board: "a0e0", port: 1, on: false), #"{"board":"a0e0","on":false,"op":"power","port":1}"#),
        (.brightness(percent: 30), #"{"op":"brightness","percent":30}"#),
        (.importImage(path: "/tmp/a.ipsw"), #"{"op":"import","path":"\/tmp\/a.ipsw"}"#),
        (.remove(build: "26B131"), #"{"build":"26B131","op":"remove"}"#),
    ])
    func aRequestEncodesAsTheAgentReadsIt(request: AgentRequest, expected: String) throws {
        let encoder = JSONEncoder()
        encoder.outputFormatting = .sortedKeys
        #expect(try String(decoding: encoder.encode(request), as: UTF8.self) == expected)
    }
}
