using System.Net.Sockets;
using System.Runtime.CompilerServices;
using System.Text;
using System.Text.Json;

namespace Iasis.Agent;

/// The agent's socket: the state it streams, and one connection per request.
public static class AgentClient
{
    /// Where the Windows service listens.
    public static readonly string SocketPath = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.CommonApplicationData),
        "Iasis",
        "cache",
        "agent.sock");

    private static readonly byte[] EndOfLine = "\n"u8.ToArray();

    private static async Task<Stream> Connect(CancellationToken token)
    {
        var socket = new Socket(AddressFamily.Unix, SocketType.Stream, ProtocolType.Unspecified);
        try
        {
            using var timeout = CancellationTokenSource.CreateLinkedTokenSource(token);
            timeout.CancelAfter(TimeSpan.FromSeconds(2));
            await socket.ConnectAsync(new UnixDomainSocketEndPoint(SocketPath), timeout.Token);
            return new NetworkStream(socket, ownsSocket: true);
        }
        catch (SocketException error) when (error.SocketErrorCode == SocketError.AccessDenied)
        {
            socket.Dispose();
            throw new AgentRefusedException("Iasis serves Administrators and the person signed in at this computer.");
        }
        catch
        {
            socket.Dispose();
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
        await using var stream = await Connect(token);
        await Send(stream, request, token);
        using var reader = new StreamReader(stream, Encoding.UTF8, leaveOpen: true);
        return Decode(await reader.ReadLineAsync(token) ?? throw new IOException("The agent closed the connection."));
    }

    /// The agent's state now and each time it changes, until the connection ends.
    public static async IAsyncEnumerable<AgentState> Watch([EnumeratorCancellation] CancellationToken token = default)
    {
        await using var stream = await Connect(token);
        await Send(stream, new WatchRequest(), token);
        using var reader = new StreamReader(stream, Encoding.UTF8, leaveOpen: true);
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
