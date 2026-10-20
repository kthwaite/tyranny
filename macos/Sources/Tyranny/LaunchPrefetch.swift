import Foundation
import TyrannyCore

/// The launch prefetch: a short-lived second connection that sends one `list_tracks` request
/// before the main queue is free, and puts the reply in a `LaunchPrefetchSlot`.
///
/// It runs off the main actor. It holds `WireClient`'s security line: the endpoint is validated
/// before the token is read, the token goes in the bearer header, redirects are rejected, and the
/// session is ephemeral. It sends nothing but that one request. It never retries and never starts
/// a managed server. Any error, a non-ok reply or the deadline leaves the slot empty.
enum LaunchPrefetch {
    /// The whole exchange, from start to reply.
    static let budget: Duration = .seconds(2)

    /// Starts the exchange and returns at once. `deadline` ends the exchange; claims wait no longer.
    static func start(endpoint: String, tokenPath: String,
                      request: [String: JSONValue]) -> (slot: LaunchPrefetchSlot, deadline: ContinuousClock.Instant) {
        let slot = LaunchPrefetchSlot(request: request)
        let deadline = ContinuousClock.now + budget
        Task.detached(priority: .userInitiated) {
            let reply = await fetch(endpoint: endpoint, tokenPath: tokenPath, request: request, deadline: deadline)
            await slot.fulfil(reply)
        }
        return (slot, deadline)
    }

    private static func fetch(endpoint: String, tokenPath: String, request: [String: JSONValue],
                              deadline: ContinuousClock.Instant) async -> LaunchPrefetchReply? {
        // Do not even open the credential until the complete destination is validated.
        guard let destination = try? WireClient.validateEndpoint(endpoint),
              let token = try? WireClient.readToken(at: tokenPath) else { return nil }
        // A rejected redirect fails the upgrade, and with it the first receive.
        let session = URLSession(configuration: WireClient.sessionConfiguration(timeout: TimeInterval(budget.components.seconds)),
                                 delegate: RedirectRejector {}, delegateQueue: nil)
        var upgrade = URLRequest(url: destination)
        upgrade.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        let socket = session.webSocketTask(with: upgrade)
        socket.maximumMessageSize = WireClient.responseLimit
        // The deadline closes the socket, which fails the receive or send in progress.
        let timer = Task {
            guard (try? await Task.sleep(until: deadline, clock: .continuous)) != nil else { return }
            socket.cancel(with: .goingAway, reason: nil)
        }
        defer {
            timer.cancel()
            socket.cancel(with: .goingAway, reason: nil)
            session.invalidateAndCancel()
        }
        socket.resume()
        return try? await exchange(on: socket, request: request)
    }

    /// Waits for the hello, sends the request with ID 1 and returns its ok reply's result (the
    /// bytes `WireClient.request` would return) with the hello's instance and the latest history
    /// revision seen. Nil for an error reply or any protocol surprise.
    private static func exchange(on socket: URLSessionWebSocketTask,
                                 request: [String: JSONValue]) async throws -> LaunchPrefetchReply? {
        guard case (.hello(let version, let instance, let snapshot), _)? = try await receive(on: socket),
              version == 9, !instance.isEmpty else { return nil }
        var history = snapshot.history_revision
        var sequence = snapshot.sequence
        var object = request
        object["id"] = .uint(1)
        object["command"] = .string("list_tracks")
        let bytes = try JSONEncoder().encode(object)
        guard bytes.count <= WireClient.requestLimit else { return nil }
        try await socket.send(.string(String(decoding: bytes, as: UTF8.self)))
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
    /// protocol v9 JSON text within the size limit.
    private static func receive(on socket: URLSessionWebSocketTask) async throws -> (IncomingMessage, Data)? {
        guard case .string(let text) = try await socket.receive(),
              text.utf8.count <= WireClient.responseLimit else { return nil }
        let data = Data(text.utf8)
        guard let message = try? JSONDecoder().decode(IncomingMessage.self, from: data) else { return nil }
        return (message, data)
    }
}
