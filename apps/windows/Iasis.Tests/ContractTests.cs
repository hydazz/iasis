using System.Runtime.CompilerServices;
using System.Text.Json;
using System.Text.Json.Nodes;
using System.Text.Json.Serialization.Metadata;
using Iasis.Agent;
using Xunit;

namespace Iasis.Tests;

/// The agent's example of every message, from `crates/api/fixtures`: the app
/// reads each state and reply without losing a member, and writes each
/// request as the example has it.
public class ContractTests
{
    [Fact]
    public void TheStateRoundTrips() => RoundTrip("state.json", AgentJson.Default.AgentState);

    [Fact]
    public void EveryRequestRoundTrips() => RoundTrip("requests.json", AgentJson.Default.IReadOnlyListRequest);

    [Theory]
    [InlineData(true, false, false)]
    [InlineData(false, true, true)]
    [InlineData(true, true, false)]
    public void SetupCarriesTheChosenOptionsAndAttachments(bool language, bool dep, bool checkOnly)
    {
        var selected = new Selection("a", 4, [new Target(2, 7, "1234")]);
        var settings = language ? new SetupLanguage("en-AU", "en_AU") : null;
        var request = JsonSerializer.SerializeToNode(new AdvanceSetupRequest([selected], settings, dep, checkOnly), AgentJson.Default.Request)!.AsObject();
        Assert.Equal("advance_setup", (string?)request["op"]);
        Assert.Equal(settings?.Locale, (string?)request["settings"]?["locale"]);
        Assert.Equal(dep, (bool?)request["dep"]);
        Assert.Equal(checkOnly, (bool?)request["check_only"]);
        Assert.Equal("1234", (string?)request["hubs"]![0]!["targets"]![0]!["ecid"]);
    }

    [Fact]
    public void EveryReplyReadsWhole()
    {
        foreach (var example in JsonNode.Parse(File.ReadAllText(Fixture("replies.json")))!.AsArray())
        {
            var reply = JsonSerializer.Deserialize(example!.ToJsonString(), AgentJson.Default.Reply)!;
            Assert.True(
                JsonNode.DeepEquals(WithoutNulls(example.DeepClone()), WithoutNulls(Written(reply))),
                $"{example.ToJsonString()} read as {reply}");
        }
    }

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

    /// A reply as the agent writes it, from what the app read.
    private static JsonNode? Written(Reply reply) => reply switch
    {
        OkReply => JsonValue.Create("ok"),
        ErrorReply error => new JsonObject { ["error"] = error.Message },
        StateReply state => new JsonObject { ["state"] = JsonSerializer.SerializeToNode(state.State, AgentJson.Default.AgentState) },
        CatalogReply catalog => new JsonObject { ["catalog"] = JsonSerializer.SerializeToNode(catalog.Models, AgentJson.Default.IReadOnlyListCatalogModel) },
        ReleasesReply releases => new JsonObject { ["releases"] = JsonSerializer.SerializeToNode(releases.Releases, AgentJson.Default.IReadOnlyListRelease) },
        PlanReply plan => new JsonObject { ["plan"] = JsonSerializer.SerializeToNode(plan.Planned, AgentJson.Default.IReadOnlyListPlanned) },
        SupervisionReply supervision => new JsonObject { ["supervision"] = supervision.Pkcs12 },
        _ => throw new ArgumentException($"no example for {reply}"),
    };

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
