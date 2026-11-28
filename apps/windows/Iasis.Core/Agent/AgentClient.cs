using System.Net.Sockets;
using System.Runtime.CompilerServices;
using System.Text;
using System.Text.Json;

namespace Iasis.Agent;

/// The agent's socket: the state it streams, and one connection per request.
public sealed class AgentClient(string socketPath)
{
    /// Where the Windows service listens.
    public static AgentClient Local { get; } = new(Path.Combine(
        Environment.GetEnvironmentVariable("IASIS_HOME") ?? Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.CommonApplicationData), "Iasis", "cache"),
        "agent.sock"));

    private static readonly byte[] EndOfLine = "\n"u8.ToArray();

    private async Task<NetworkStream> Connect(CancellationToken token)
    {
        var socket = new Socket(AddressFamily.Unix, SocketType.Stream, ProtocolType.Unspecified);
        try
        {
            using var timeout = CancellationTokenSource.CreateLinkedTokenSource(token);
            timeout.CancelAfter(TimeSpan.FromSeconds(2));
            await socket.ConnectAsync(new UnixDomainSocketEndPoint(socketPath), timeout.Token);
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

    private static async Task<Reply> ReadReply(StreamReader reader, CancellationToken token)
    {
        var reply = Decode(await reader.ReadLineAsync(token) ?? throw new IOException("The Iasis service closed the connection."));
        return reply is ErrorReply error ? throw new AgentRefusedException(error.Message) : reply;
    }

    /// An optional file follows its request as bytes on the same connection.
    public async Task<Reply> Ask(Request request, CancellationToken token = default, FileStream? file = null)
    {
        if ((file?.Length ?? 0) != request.Payload())
        {
            throw new IOException("The file size changed before the transfer.");
        }
        await using var stream = await Connect(token);
        await Send(stream, request, token);
        using var reader = new StreamReader(stream, Encoding.UTF8, leaveOpen: true);
        if (file is not null)
        {
            try
            {
                await file.CopyToAsync(stream, token);
            }
            catch (IOException)
            {
                // A refused upload can carry the agent's reason before the socket closes.
                try
                {
                    stream.Socket.Shutdown(SocketShutdown.Send);
                    await ReadReply(reader, token);
                }
                catch (SocketException) { }
                catch (IOException) { }
                throw;
            }
        }
        return await ReadReply(reader, token);
    }

    /// A submitted job's outcomes are only present on its own connection.
    public async IAsyncEnumerable<AgentState> Watch(
        [EnumeratorCancellation] CancellationToken token = default, Request? submitted = null)
    {
        await using var stream = await Connect(token);
        using var reader = new StreamReader(stream, Encoding.UTF8, leaveOpen: true);
        if (submitted is not null)
        {
            await Send(stream, submitted, token);
            if (await ReadReply(reader, token) is not OkReply)
            {
                throw new InvalidDataException("The Iasis service did not accept the request.");
            }
        }
        await Send(stream, new WatchRequest(), token);
        while (await reader.ReadLineAsync(token) is { } line)
        {
            switch (Decode(line))
            {
                case StateReply state:
                    yield return state.State;
                    break;
                case ErrorReply error:
                    throw new AgentRefusedException(error.Message);
                default:
                    throw new InvalidDataException("The Iasis service returned an unexpected reply.");
            }
        }
    }
}

/// The agent will not serve this user.
public sealed class AgentRefusedException(string message) : Exception(message);
