import Foundation

/// The agent, as the app talks to it.
nonisolated protocol AgentService: Sendable {
    /// The agent's state, now and each time it changes, until the agent goes
    /// away.
    func states() -> AsyncThrowingStream<AgentState, any Error>

    /// Make a request, which either works or throws the agent's reason.
    func send(_ request: AgentRequest) async throws

    /// Every device model the catalog lists.
    func catalog() async throws -> [AgentState.CatalogModel]

    /// The releases Apple signs for a model, newest first.
    func releases(of model: String) async throws -> [AgentState.Release]

    /// What a restore would install on each target.
    func plan(_ boards: [RestoreSelection]) async throws -> [AgentState.Planned]

    /// The host's supervision identity as a PKCS #12 archive in base64,
    /// protected by this password.
    func exportSupervision(password: String) async throws -> String

    /// Send an IPSW archive to the store, reporting the bytes sent as they
    /// go. The agent checks the archive before it answers.
    func importImage(_ file: URL, progress: @escaping @Sendable (Int64) -> Void) async throws
}

nonisolated enum AgentError: LocalizedError {
    case unavailable
    case refused(String)

    var errorDescription: String? {
        switch self {
        case .unavailable: "The Iasis agent is not running."
        case let .refused(reason): reason
        }
    }
}

/// The agent's local socket. Each request and each watch has a connection of
/// its own, carrying JSON a line at a time.
nonisolated struct AgentClient: AgentService {
    var socket: URL = .applicationSupportDirectory.appending(path: "iasis/agent.sock")

    func states() -> AsyncThrowingStream<AgentState, any Error> {
        let (states, continuation) = AsyncThrowingStream.makeStream(of: AgentState.self)
        let watching = Task {
            do {
                for try await line in try lines(answering: .watch) {
                    if case let .state(state) = try reply(in: line) {
                        continuation.yield(state)
                    }
                }
                continuation.finish()
            } catch {
                continuation.finish(throwing: error)
            }
        }
        continuation.onTermination = { _ in watching.cancel() }
        return states
    }

    func send(_ request: AgentRequest) async throws {
        _ = try await answer(to: request)
    }

    func catalog() async throws -> [AgentState.CatalogModel] {
        guard case let .catalog(models) = try await answer(to: .catalog) else {
            throw AgentError.unavailable
        }
        return models
    }

    func releases(of model: String) async throws -> [AgentState.Release] {
        guard case let .releases(releases) = try await answer(to: .releases(model: model)) else {
            throw AgentError.unavailable
        }
        return releases
    }

    func plan(_ boards: [RestoreSelection]) async throws -> [AgentState.Planned] {
        guard case let .plan(planned) = try await answer(to: .plan(boards: boards)) else {
            throw AgentError.unavailable
        }
        return planned
    }

    func exportSupervision(password: String) async throws -> String {
        guard case let .supervision(archive) = try await answer(to: .exportSupervision(password: password)) else {
            throw AgentError.unavailable
        }
        return archive
    }

    func importImage(_ file: URL, progress: @escaping @Sendable (Int64) -> Void) async throws {
        let handle = try FileHandle(forReadingFrom: file)
        let bytes = try Int64(handle.seekToEnd())
        try handle.seek(toOffset: 0)
        let request = AgentRequest.importImage(name: file.lastPathComponent, bytes: bytes)
        let reply = try await withCheckedThrowingContinuation { continuation in
            Thread.detachNewThread {
                defer { try? handle.close() }
                continuation.resume(with: Result { try upload(request, from: handle, progress: progress) })
            }
        }
        if case let .error(reason) = reply {
            throw AgentError.refused(reason)
        }
    }

    /// Send a request and the bytes it names, then read its one reply. An
    /// agent that refuses an upload says why before it closes the connection.
    private func upload(_ request: AgentRequest, from file: FileHandle, progress: (Int64) -> Void) throws -> AgentReply {
        let descriptor = try connect()
        defer { close(descriptor) }
        // A refusal closes the socket under a write, which must fail rather
        // than end the app.
        var on: Int32 = 1
        setsockopt(descriptor, SOL_SOCKET, SO_NOSIGPIPE, &on, socklen_t(MemoryLayout<Int32>.size))
        var message = try JSONEncoder().encode(request)
        message.append(0x0A)
        var sent: Int64 = 0
        if write(message, to: descriptor) {
            while sent < request.payload, let chunk = try file.read(upToCount: 1 << 20), !chunk.isEmpty {
                guard write(chunk, to: descriptor) else { break }
                sent += Int64(chunk.count)
                if sent % (32 << 20) == 0 || sent == request.payload {
                    progress(sent)
                }
            }
        }
        var line = Data()
        var byte: UInt8 = 0
        while read(descriptor, &byte, 1) == 1, byte != 0x0A {
            line.append(byte)
        }
        guard !line.isEmpty else {
            throw AgentError.unavailable
        }
        let answer = try reply(in: line)
        if case .error = answer {
            return answer
        }
        guard sent == request.payload else {
            throw AgentError.refused("The file ended early.")
        }
        return answer
    }

    /// Write all of `data`, or find that the agent has gone.
    private func write(_ data: Data, to descriptor: Int32) -> Bool {
        data.withUnsafeBytes { buffer in
            var offset = 0
            while offset < buffer.count {
                let count = Darwin.write(descriptor, buffer.baseAddress! + offset, buffer.count - offset)
                guard count > 0 else { return false }
                offset += count
            }
            return true
        }
    }

    private func answer(to request: AgentRequest) async throws -> AgentReply {
        for try await line in try lines(answering: request) {
            let reply = try reply(in: line)
            if case let .error(reason) = reply {
                throw AgentError.refused(reason)
            }
            return reply
        }
        throw AgentError.unavailable
    }

    private func reply(in line: Data) throws -> AgentReply {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(AgentReply.self, from: line)
    }

    /// Send a request and deliver each line of the answer as it arrives, on a
    /// thread of its own: a read waits for as long as the agent has nothing
    /// to say. `FileHandle.bytes` holds lines back until its buffer fills.
    private func lines(answering request: AgentRequest) throws -> AsyncThrowingStream<Data, any Error> {
        let descriptor = try connect()
        var message = try JSONEncoder().encode(request)
        message.append(0x0A)
        guard write(message, to: descriptor) else {
            close(descriptor)
            throw AgentError.unavailable
        }
        let (lines, continuation) = AsyncThrowingStream.makeStream(of: Data.self)
        // Ending the stream ends the read, and the thread then closes the socket.
        continuation.onTermination = { _ in shutdown(descriptor, SHUT_RDWR) }
        Thread.detachNewThread {
            defer { close(descriptor) }
            var pending = Data()
            var buffer = [UInt8](repeating: 0, count: 1 << 16)
            while true {
                let count = read(descriptor, &buffer, buffer.count)
                guard count > 0 else {
                    continuation.finish()
                    return
                }
                pending.append(contentsOf: buffer[..<count])
                while let end = pending.firstIndex(of: 0x0A) {
                    continuation.yield(Data(pending[pending.startIndex ..< end]))
                    pending.removeSubrange(pending.startIndex ... end)
                }
            }
        }
        return lines
    }

    private func connect() throws -> Int32 {
        let descriptor = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard descriptor >= 0 else {
            throw AgentError.unavailable
        }
        var address = sockaddr_un()
        address.sun_family = sa_family_t(AF_UNIX)
        let path = Array(socket.path(percentEncoded: false).utf8)
        // The path and its terminator must fit the address.
        guard path.count < MemoryLayout.size(ofValue: address.sun_path) else {
            close(descriptor)
            throw AgentError.unavailable
        }
        withUnsafeMutableBytes(of: &address.sun_path) { $0.copyBytes(from: path) }
        let connected = withUnsafePointer(to: &address) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.connect(descriptor, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard connected == 0 else {
            close(descriptor)
            throw AgentError.unavailable
        }
        return descriptor
    }
}
