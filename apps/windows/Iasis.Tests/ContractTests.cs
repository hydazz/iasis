using System.Runtime.CompilerServices;
using System.Text.Json;
using System.Text.Json.Nodes;
using System.Text.Json.Serialization.Metadata;
using Iasis.Agent;
using Xunit;

namespace Iasis.Tests;

/// The agent's example of every message, from `crates/api/fixtures`, reads
/// and writes back unchanged.
public class ContractTests
{
    [Fact]
    public void TheStateRoundTrips() => RoundTrip("state.json", AgentJson.Default.AgentState);

    [Fact]
    public void EveryRequestRoundTrips() => RoundTrip("requests.json", AgentJson.Default.IReadOnlyListRequest);

    [Fact]
    public void EveryReplyRoundTrips() => RoundTrip("replies.json", AgentJson.Default.IReadOnlyListReply);

    [Fact]
    public void AnUploadSaysHowManyBytesFollow()
    {
        Assert.Equal(19_000_000_000, new ImportRequest("Mac.ipsw", 19_000_000_000).Payload());
        Assert.Equal(0, new UpdateRequest("817a23bd502453b4", null).Payload());
    }

    private static void RoundTrip<T>(string name, JsonTypeInfo<T> info)
    {
        var json = File.ReadAllText(Fixture(name));
        var decoded = JsonSerializer.Deserialize(json, info)!;
        var encoded = JsonSerializer.Serialize(decoded, info);
        Assert.True(
            JsonNode.DeepEquals(WithoutNulls(JsonNode.Parse(json)), WithoutNulls(JsonNode.Parse(encoded))),
            $"{name} changed on the way through:\n{encoded}");
    }

    private static string Fixture(string name, [CallerFilePath] string here = "") =>
        Path.Combine(Path.GetDirectoryName(here)!, "..", "..", "..", "crates", "api", "fixtures", name);

    /// The agent reads an absent member as null.
    private static JsonNode? WithoutNulls(JsonNode? node)
    {
        switch (node)
        {
            case JsonObject members:
                foreach (var key in members.Where(member => member.Value is null).Select(member => member.Key).ToList())
                {
                    members.Remove(key);
                }
                foreach (var member in members)
                {
                    WithoutNulls(member.Value);
                }
                break;
            case JsonArray items:
                foreach (var item in items)
                {
                    WithoutNulls(item);
                }
                break;
        }
        return node;
    }
}
