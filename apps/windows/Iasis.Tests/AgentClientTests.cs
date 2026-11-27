using System.Net.Sockets;
using System.Text;
using System.Text.Json.Nodes;
using Iasis.Agent;
using Xunit;

namespace Iasis.Tests;

public class AgentClientTests
{
    [Fact]
    public async Task SubmittedResultsStayOnTheConnectionThatSentTheRequest()
    {
        using var timeout = new CancellationTokenSource(TimeSpan.FromSeconds(10));
        var token = timeout.Token;
        var path = Path.Combine(Path.GetTempPath(), $"ia{Guid.NewGuid():N}"[..14]);
        using var listener = new Socket(AddressFamily.Unix, SocketType.Stream, ProtocolType.Unspecified);
        listener.Bind(new UnixDomainSocketEndPoint(path));
        listener.Listen(1);
        var hubs = new[] { new Selection("abcd", 1, [new(2, 4, null)]) };
        try
        {
            var serving = Serve();
            var frames = new List<AgentState>();
            await foreach (var frame in new AgentClient(path).Watch(token, new ReviveRequest(hubs, false)))
                frames.Add(frame);
            await serving;
            Assert.Equal(2, frames.Count);
            Assert.Null(Assert.Single(frames[0].Submitted!));
            Assert.Empty(frames[1].Hubs);
            Assert.True(frames[1].PostRestore.TryEnrollment);
            Assert.Equal(RestorePhase.Reported, Assert.Single(frames[1].Submitted!)!.Phase);
        }
        finally { File.Delete(path); }

        async Task Serve()
        {
            using var socket = await listener.AcceptAsync(token);
            using var stream = new NetworkStream(socket);
            using var reader = new StreamReader(stream, leaveOpen: true);
            using var writer = new StreamWriter(stream, new UTF8Encoding(false), leaveOpen: true) { AutoFlush = true };
            var submit = JsonNode.Parse((await reader.ReadLineAsync(token))!)!;
            Assert.Equal("revive", (string?)submit["op"]);
            Assert.Equal(4, (int?)submit["hubs"]![0]!["targets"]![0]!["generation"]);
            await writer.WriteLineAsync("\"ok\"");
            Assert.Equal("watch", (string?)JsonNode.Parse((await reader.ReadLineAsync(token))!)!["op"]);
            var frame = JsonNode.Parse("""
                {"state":{"version":"0.1.0","hubs":[],"submitted":[null],"images":{"images":[]},
                "post_restore":{"shutdown_macs":false,"setup_language":null,"try_enrollment":true},"sharing":{"enabled":false,"ports":[]},"supervision":{},"missing":[]}}
                """)!;
            await writer.WriteLineAsync(frame.ToJsonString());
            frame["state"]!["submitted"]![0] = JsonNode.Parse("""
                {"operation":"revive","phase":"Reported","message":"Complete"}
                """);
            await writer.WriteLineAsync(frame.ToJsonString());
        }
    }
}
