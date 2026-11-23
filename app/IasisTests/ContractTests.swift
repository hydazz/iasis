import Foundation
@testable import Iasis
import Testing

/// The agent's example of every message, from `crates/api/fixtures`, reads
/// into the models and writes back as the same JSON.
struct ContractTests {
    private static let fixtures = URL(filePath: #filePath)
        .deletingLastPathComponent()
        .appending(path: "../../crates/api/fixtures")
        .standardized

    @Test func theStateRoundTrips() throws {
        try roundTrip(AgentState.self, "state.json")
    }

    @Test func everyRequestRoundTrips() throws {
        try roundTrip([AgentRequest].self, "requests.json")
    }

    @Test func everyReplyRoundTrips() throws {
        try roundTrip([AgentReply].self, "replies.json")
    }

    private func roundTrip<Message: Codable>(_: Message.Type, _ name: String) throws {
        let json = try Data(contentsOf: Self.fixtures.appending(path: name))
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let encoder = JSONEncoder()
        encoder.keyEncodingStrategy = .convertToSnakeCase
        let again = try encoder.encode(decoder.decode(Message.self, from: json))
        #expect(try members(again) == members(json))
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
        return try present(JSONSerialization.jsonObject(with: json)) as! NSObject
    }
}
