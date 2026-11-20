import Foundation
@testable import Iasis
import Testing

struct AgentMessagesTests {
    @Test func aStateDecodesAsTheAgentSendsIt() throws {
        let state = try Fixtures.state
        let board = try #require(state.boards.first)
        #expect(board.serial == "a0e012192cdfdb67")
        #expect(board.ports.map(\.number) == [1, 2, 3, 4, 5])
        #expect(board.name == "Repair Bench")
        #expect(board.visiblePorts.map(\.number) == [2, 3, 4, 5])
        #expect(board.ports[1].menuTitle == "Port 2 · MacBook Air · DFU")
        #expect(board.ports[1].symbol == "laptopcomputer")
        #expect(board.ports[2].menuTitle == "Port 3 · iPad · Restoring")
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

    @Test(arguments: [
        (AgentRequest.watch, #"{"op":"watch"}"#),
        (.restore(board: "a0e0", session: 77, targets: [.init(port: 2, generation: 4)]), #"{"board":"a0e0","op":"restore","session":77,"targets":[{"generation":4,"port":2}]}"#),
        (.revive(board: "a0e0", session: 77, port: 2, generation: 4, ecid: "0014692221e0801e"), #"{"board":"a0e0","ecid":"0014692221e0801e","generation":4,"op":"revive","port":2,"session":77}"#),
        (.dfu(board: "a0e0", port: 2), #"{"board":"a0e0","op":"dfu","port":2}"#),
        (.power(board: "a0e0", port: 1, on: false), #"{"board":"a0e0","on":false,"op":"power","port":1}"#),
        (.brightness(board: "a0e0", percent: 30), #"{"board":"a0e0","op":"brightness","percent":30}"#),
        (.rename(board: "a0e0", name: "Bench A"), #"{"board":"a0e0","name":"Bench A","op":"rename"}"#),
        (.importImage(path: "/tmp/a.ipsw"), #"{"op":"import","path":"\/tmp\/a.ipsw"}"#),
        (.remove(build: "26B131"), #"{"build":"26B131","op":"remove"}"#),
    ])
    func aRequestEncodesAsTheAgentReadsIt(request: AgentRequest, expected: String) throws {
        let encoder = JSONEncoder()
        encoder.outputFormatting = .sortedKeys
        #expect(try String(decoding: encoder.encode(request), as: UTF8.self) == expected)
    }
}
