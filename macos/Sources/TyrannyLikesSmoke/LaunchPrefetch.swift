import Foundation
import Network
import TyrannyLikesSmokeCore

/// The launch prefetch: a short-lived second connection that sends one `list_tracks` request
/// before the main queue is free, and puts the reply in a `LaunchPrefetchSlot`.
///
/// It runs off the main actor and validates the same private socket as `WireClient`.
/// It sends only that request, never retries and never starts a managed server.
/// Any error, a non-ok reply or the deadline leaves the slot empty.
enum LaunchPrefetch {
    /// The whole exchange, from start to reply.
    static let budget: Duration = .seconds(2)

    /// Starts the exchange and returns at once. `deadline` ends the exchange; claims wait no longer.
    static func start(socketPath: String,
                      request: [String: JSONValue]) -> (slot: LaunchPrefetchSlot, deadline: ContinuousClock.Instant) {
        let slot = LaunchPrefetchSlot(request: request)
        let deadline = ContinuousClock.now + budget
        Task.detached(priority: .userInitiated) {
            let reply = await fetch(socketPath: socketPath, request: request, deadline: deadline)
            await slot.fulfil(reply)
        }
        return (slot, deadline)
    }

    private static func fetch(socketPath: String, request: [String: JSONValue],
                              deadline: ContinuousClock.Instant) async -> LaunchPrefetchReply? {
        guard let path = try? WireClient.validateSocketPath(socketPath) else { return nil }
        let socket = NWConnection(to: .unix(path: path), using: .tcp)
        // The deadline closes the socket, which fails the receive or send in progress.
        let timer = Task {
            guard (try? await Task.sleep(until: deadline, clock: .continuous)) != nil else { return }
            socket.cancel()
        }
        defer {
            timer.cancel()
            socket.cancel()
        }
        socket.start(queue: .global(qos: .userInitiated))
        return try? await exchange(on: socket, request: request)
    }

    /// Waits for the hello, sends the request with ID 1 and returns its ok reply's result (the
    /// bytes `WireClient.request` would return) with the hello's instance and the latest history
    /// revision seen. Nil for an error reply or any protocol surprise.
    private static func exchange(on socket: NWConnection,
                                 request: [String: JSONValue]) async throws -> LaunchPrefetchReply? {
        guard case (.hello(let version, let instance, let snapshot), _)? = try await receive(on: socket),
              version == WireProtocol.version, !instance.isEmpty else { return nil }
        var history = snapshot.history_revision
        var sequence = snapshot.sequence
        var object = request
        object["id"] = .uint(1)
        object["command"] = .string("list_tracks")
        let bytes = try JSONEncoder().encode(object)
        guard bytes.count <= WireClient.requestLimit else { return nil }
        let frame = try JSONFrame.encode(bytes, limit: WireClient.requestLimit)
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, any Error>) in
            socket.send(content: frame, completion: .contentProcessed { error in
                if let error { continuation.resume(throwing: error) }
                else { continuation.resume() }
            })
        }
        while let (message, data) = try await receive(on: socket) {
            switch message {
            case .state(let state):
                // As `WireClient.apply`: only a newer snapshot counts.
                if state.sequence > sequence {
                    sequence = state.sequence
                    history = state.history_revision
                }
            case .success(1, let header) where header.kind == "list_tracks":
                return WireClient.result(ofReply: data).map {
                    LaunchPrefetchReply(body: $0, instanceID: instance, historyRevision: history)
                }
            case .hello, .success, .failure:
                return nil
            }
        }
        return nil
    }

    /// The next message and its bytes, decoded as `WireClient` decodes them. Nil when it is not
    /// `WireProtocol.version` JSON text within the size limit.
    private static func receive(on socket: NWConnection) async throws -> (IncomingMessage, Data)? {
        let header = try await receiveBytes(4, on: socket)
        let count = try JSONFrame.payloadLength(header, limit: WireClient.responseLimit)
        let data = try await receiveBytes(count, on: socket)
        var bytes = data.makeIterator()
        var utf8 = Unicode.UTF8.ForwardParser()
        validation: while true {
            switch utf8.parseScalar(from: &bytes) {
            case .valid: continue
            case .emptyInput: break validation
            case .error: return nil
            }
        }
        guard let message = try? JSONDecoder().decode(IncomingMessage.self, from: data) else { return nil }
        return (message, data)
    }

    private static func receiveBytes(_ count: Int, on socket: NWConnection) async throws -> Data {
        var bytes = Data()
        while bytes.count < count {
            let remaining = count - bytes.count
            let part: Data = try await withCheckedThrowingContinuation { continuation in
                socket.receive(minimumIncompleteLength: remaining, maximumLength: remaining) { data, _, _, error in
                    if let error { continuation.resume(throwing: error) }
                    else if let data, !data.isEmpty { continuation.resume(returning: data) }
                    else { continuation.resume(throwing: CancellationError()) }
                }
            }
            if bytes.isEmpty { bytes = part } else { bytes.append(part) }
        }
        return bytes
    }
}
