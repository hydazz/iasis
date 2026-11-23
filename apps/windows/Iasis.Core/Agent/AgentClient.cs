using System.IO.Pipes;
using System.Runtime.CompilerServices;
using System.Text;
using System.Text.Json;

namespace Iasis.Agent;

/// The agent's named pipe: the state it streams, and one connection per request.
public static class AgentClient
{
    public const string PipeName = "iasis";
    private static readonly byte[] EndOfLine = "\n"u8.ToArray();

    private static async Task<NamedPipeClientStream> Connect(CancellationToken token)
    {
        var pipe = new NamedPipeClientStream(".", PipeName, PipeDirection.InOut, PipeOptions.Asynchronous);
        try
        {
            await pipe.ConnectAsync(2000, token);
            return pipe;
        }
        catch
        {
            await pipe.DisposeAsync();
            throw;
        }
    }

    private static async Task Send(Stream stream, Request request, CancellationToken token)
    {
        await stream.WriteAsync(JsonSerializer.SerializeToUtf8Bytes(request, AgentJson.Default.Request), token);
        await stream.WriteAsync(EndOfLine, token);
        await stream.FlushAsync(token);
    }

    private static Reply Decode(string line) =>
        JsonSerializer.Deserialize(line, AgentJson.Default.Reply) ?? throw new JsonException("an empty reply");

    /// Ask the agent once.
    public static async Task<Reply> Ask(Request request, CancellationToken token = default)
    {
        await using var pipe = await Connect(token);
        await Send(pipe, request, token);
        using var reader = new StreamReader(pipe, Encoding.UTF8, leaveOpen: true);
        return Decode(await reader.ReadLineAsync(token) ?? throw new IOException("The agent closed the connection."));
    }

    /// The agent's state now and each time it changes, until the connection ends.
    public static async IAsyncEnumerable<AgentState> Watch([EnumeratorCancellation] CancellationToken token = default)
    {
        await using var pipe = await Connect(token);
        await Send(pipe, new WatchRequest(), token);
        using var reader = new StreamReader(pipe, Encoding.UTF8, leaveOpen: true);
        while (await reader.ReadLineAsync(token) is { } line)
        {
            switch (Decode(line))
            {
                case StateReply state:
                    yield return state.State;
                    break;
                case ErrorReply error:
                    throw new AgentRefusedException(error.Message);
            }
        }
    }
}

/// The agent will not serve this user.
public sealed class AgentRefusedException(string message) : Exception(message);
