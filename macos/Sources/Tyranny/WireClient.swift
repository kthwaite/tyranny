import Darwin
import Foundation
import TyrannyCore

/// A connection owns its waiters. Losing it never authorizes replaying a command.
@MainActor
final class WireClient {
    var onSnapshot: ((ControlState, String?) -> Void)?
    var onDisconnect: ((String) -> Void)?
    private(set) var instanceID: String?

    nonisolated static let requestLimit = 16 * 1024
    nonisolated static let responseLimit = 8 * 1024 * 1024
    private static let pendingLimit = 32

    private struct PendingRequest: Sendable {
        let continuation: CheckedContinuation<Data, any Error>
        let expectedKind: String?
        let deadline: Task<Void, Never>
        let sender: Task<Void, Never>

        func cancelTasks() {
            deadline.cancel()
            sender.cancel()
        }
    }

    private var generation = UUID()
    private var session: URLSession?
    private var socket: URLSessionWebSocketTask?
    private var receiver: Task<Void, Never>?
    private var helloDeadline: Task<Void, Never>?
    private var helloWaiter: CheckedContinuation<Void, any Error>?
    private var pending: [UInt64: PendingRequest] = [:]
    private var nextRequestID: UInt64 = 1
    private var sequence: UInt64?
    private var ready = false
    private let decoder = JSONDecoder()
    private let encoder = JSONEncoder()
    private let requestDeadlineSeconds: UInt64

    init(requestDeadlineSeconds: UInt64 = 10) {
        self.requestDeadlineSeconds = requestDeadlineSeconds
    }

    deinit {
        receiver?.cancel()
        helloDeadline?.cancel()
        socket?.cancel(with: .goingAway, reason: nil)
        session?.invalidateAndCancel()
        helloWaiter?.resume(throwing: CancellationError())
        for request in pending.values {
            request.cancelTasks()
            request.continuation.resume(throwing: ServerFailure(
                code: "uncertain_outcome",
                message: "The client closed before a reply arrived. The request outcome is uncertain; inspect server state before issuing it again."
            ))
        }
    }

    func connect(endpoint: String, tokenPath: String) async throws {
        try Task.checkCancellation()
        endConnection(
            ServerFailure(code: "connection_replaced", message: "The connection was replaced."),
            notify: false
        )

        // Do not even open the credential until the complete destination is validated.
        let destination = try Self.validateEndpoint(endpoint)
        let token = try Self.readToken(at: tokenPath)
        let connectionID = generation
        let configuration = Self.sessionConfiguration(timeout: TimeInterval(requestDeadlineSeconds))
        let delegate = RedirectRejector { [weak self] in
            Task { @MainActor [weak self] in
                self?.endConnection(
                    ServerFailure(code: "redirect_rejected", message: "The server attempted a redirect. Credentials were not forwarded."),
                    generation: connectionID
                )
            }
        }
        let newSession = URLSession(configuration: configuration, delegate: delegate, delegateQueue: nil)
        var upgrade = URLRequest(url: destination)
        upgrade.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        let newSocket = newSession.webSocketTask(with: upgrade)
        newSocket.maximumMessageSize = Self.responseLimit
        session = newSession
        socket = newSocket

        try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, any Error>) in
                guard !Task.isCancelled else {
                    continuation.resume(throwing: CancellationError())
                    endConnection(
                        ServerFailure(code: "cancelled", message: "Connection cancelled."),
                        generation: connectionID
                    )
                    return
                }
                helloWaiter = continuation
                helloDeadline = Task { @MainActor [weak self] in
                    do { try await Task.sleep(for: .seconds(10)) } catch { return }
                    self?.endConnection(
                        ServerFailure(code: "hello_timeout", message: "The server did not send a protocol v11 hello within 10 seconds."),
                        generation: connectionID
                    )
                }
                newSocket.resume()
                receiver = Task { @MainActor [weak self] in
                    while !Task.isCancelled {
                        do {
                            let message = try await newSocket.receive()
                            guard self?.generation == connectionID else { return }
                            try self?.receive(message, generation: connectionID)
                        } catch {
                            guard self?.generation == connectionID else { return }
                            if let failure = error as? ServerFailure {
                                self?.endConnection(failure, generation: connectionID)
                            } else {
                                self?.connectionFailed(
                                    generation: connectionID,
                                    status: (newSocket.response as? HTTPURLResponse)?.statusCode
                                )
                            }
                            return
                        }
                    }
                }
            }
        } onCancel: {
            Task { @MainActor [weak self] in
                self?.endConnection(
                    ServerFailure(code: "cancelled", message: "Connection cancelled."),
                    generation: connectionID
                )
            }
        }
    }

    func disconnect() {
        endConnection(ServerFailure(code: "disconnected", message: "Disconnected."))
    }

    func request(_ command: String, fields: [String: JSONValue] = [:]) async throws -> Data {
        try Task.checkCancellation()
        guard ready, let socket else {
            throw ServerFailure(code: "not_connected", message: "Connect to the server before sending a command.")
        }
        guard pending.count < Self.pendingLimit else {
            throw ServerFailure(code: "too_many_requests", message: "There are already 32 requests awaiting replies. This command was not sent.")
        }
        guard fields["id"] == nil, fields["command"] == nil else {
            throw ServerFailure(code: "invalid_request", message: "Request fields cannot replace the request ID or command.")
        }
        guard nextRequestID < UInt64.max else {
            throw ServerFailure(code: "request_ids_exhausted", message: "Request IDs are exhausted. Reopen the client before sending another command.")
        }
        let id = nextRequestID
        var object = fields
        object["id"] = .uint(id)
        object["command"] = .string(command)
        let bytes: Data
        do {
            bytes = try encoder.encode(object)
        } catch {
            throw ServerFailure(code: "invalid_request", message: "The command could not be encoded and was not sent.")
        }
        guard bytes.count <= Self.requestLimit else {
            throw ServerFailure(code: "request_too_large", message: "The command exceeds the 16 KiB request limit and was not sent.")
        }
        let text = String(decoding: bytes, as: UTF8.self)
        nextRequestID += 1
        let connectionID = generation
        let deadlineSeconds = requestDeadlineSeconds
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Data, any Error>) in
                guard !Task.isCancelled else {
                    continuation.resume(throwing: CancellationError())
                    return
                }
                let deadline = Task { @MainActor [weak self] in
                    do { try await Task.sleep(for: .seconds(deadlineSeconds)) } catch { return }
                    guard self?.generation == connectionID, self?.pending[id] != nil else { return }
                    self?.endConnection(
                        ServerFailure(code: "request_timeout", message: "A request did not receive a reply within \(deadlineSeconds) seconds."),
                        generation: connectionID
                    )
                }
                let sender = Task { @MainActor [weak self] in
                    guard !Task.isCancelled,
                          self?.generation == connectionID,
                          self?.pending[id] != nil else { return }
                    do {
                        try await socket.send(.string(text))
                    } catch {
                        guard self?.generation == connectionID, self?.pending[id] != nil else { return }
                        self?.endConnection(
                            ServerFailure(code: "send_failed", message: "The connection failed while sending a request."),
                            generation: connectionID
                        )
                    }
                }
                pending[id] = PendingRequest(
                    continuation: continuation,
                    expectedKind: Self.resultKind(for: command),
                    deadline: deadline,
                    sender: sender
                )
            }
        } onCancel: {
            Task { @MainActor [weak self] in
                guard self?.generation == connectionID, self?.pending[id] != nil else { return }
                // Cancellation cannot retract a command already handed to URLSession.
                self?.endConnection(
                    ServerFailure(code: "request_cancelled", message: "A request was cancelled before its reply arrived."),
                    generation: connectionID
                )
            }
        }
    }

    private func receive(_ message: URLSessionWebSocketTask.Message, generation connectionID: UUID) throws {
        let data: Data
        switch message {
        case .string(let text):
            guard text.utf8.count <= Self.responseLimit else {
                throw protocolFailure("The server exceeded the 8 MiB message limit.")
            }
            data = Data(text.utf8)
        case .data:
            throw protocolFailure("The server sent a binary message; protocol v11 requires JSON text.")
        @unknown default:
            throw protocolFailure("The server sent an unsupported WebSocket message.")
        }
        let incoming: IncomingMessage
        do {
            incoming = try decoder.decode(IncomingMessage.self, from: data)
        } catch {
            throw protocolFailure("The server sent malformed or incompatible protocol v11 JSON. Upgrade the client and server together.")
        }

        if case .hello(let version, let instance, let snapshot) = incoming {
            guard !ready, let waiter = helloWaiter else {
                throw protocolFailure("The server sent more than one hello.")
            }
            guard version == 11 else {
                throw protocolFailure("The server protocol is incompatible; this client requires version 11.")
            }
            guard !instance.isEmpty else {
                throw protocolFailure("The server hello did not identify its instance.")
            }
            helloWaiter = nil
            helloDeadline?.cancel()
            helloDeadline = nil
            ready = true
            instanceID = instance
            sequence = snapshot.sequence
            onSnapshot?(snapshot, instance)
            if generation == connectionID {
                waiter.resume()
            } else {
                waiter.resume(throwing: ServerFailure(code: "disconnected", message: "The connection closed while accepting the server hello."))
            }
            return
        }
        guard ready else {
            throw protocolFailure("The server sent a message before its protocol v11 hello.")
        }
        switch incoming {
        case .hello:
            break // Handled above, including duplicate detection.
        case .state(let snapshot):
            apply(snapshot)
        case .failure(let id, let failure):
            guard let request = pending.removeValue(forKey: id) else {
                throw protocolFailure("The server replied with an unknown request ID.")
            }
            request.cancelTasks()
            request.continuation.resume(throwing: failure)
        case .success(let id, let header):
            guard let request = pending[id] else {
                throw protocolFailure("The server replied with an unknown request ID.")
            }
            if let expected = request.expectedKind, expected != header.kind {
                throw protocolFailure("The server reply kind does not match its request.")
            }
            guard let result = Self.result(ofReply: data) else {
                throw protocolFailure("The server reply result is not a valid JSON object.")
            }
            pending.removeValue(forKey: id)
            request.cancelTasks()
            if let snapshot = header.snapshot { apply(snapshot) }
            // The authoritative snapshot callback always precedes the command's return.
            request.continuation.resume(returning: result)
        }
    }

    private func apply(_ snapshot: ControlState) {
        guard sequence.map({ snapshot.sequence > $0 }) ?? true else { return }
        sequence = snapshot.sequence
        onSnapshot?(snapshot, nil)
    }

    private func connectionFailed(generation connectionID: UUID, status: Int?) {
        let failure: ServerFailure
        switch status {
        case 401, 403:
            failure = ServerFailure(code: "authentication_failed", message: "The server rejected authentication. Check the selected token file and server.")
        case .some(300..<400):
            failure = ServerFailure(code: "redirect_rejected", message: "The server attempted a redirect. Credentials were not forwarded.")
        default:
            failure = ServerFailure(code: "connection_lost", message: "The server connection closed or could not be established. Check that the local server is running.")
        }
        endConnection(failure, generation: connectionID)
    }

    private func endConnection(_ failure: ServerFailure, generation expected: UUID? = nil, notify: Bool = true) {
        if let expected, expected != generation { return }
        instanceID = nil
        let wasActive = session != nil || helloWaiter != nil
        generation = UUID()
        ready = false
        sequence = nil
        receiver?.cancel()
        receiver = nil
        helloDeadline?.cancel()
        helloDeadline = nil
        socket?.cancel(with: .goingAway, reason: nil)
        socket = nil
        session?.invalidateAndCancel()
        session = nil
        let waiter = helloWaiter
        helloWaiter = nil
        let abandoned = pending
        pending.removeAll()
        let outcome = abandoned.isEmpty ? failure : ServerFailure(
            code: "uncertain_outcome",
            message: "\(failure.message) The outcome of unanswered requests is uncertain: they may already have been applied. Reconnect and inspect server state before issuing them again."
        )
        waiter?.resume(throwing: failure)
        for request in abandoned.values {
            request.cancelTasks()
            request.continuation.resume(throwing: outcome)
        }
        if notify, wasActive { onDisconnect?(outcome.localizedDescription) }
    }

    private func protocolFailure(_ message: String) -> ServerFailure {
        ServerFailure(code: "protocol_error", message: message)
    }

    private static func resultKind(for command: String) -> String? {
        switch command {
        case "snapshot": return "snapshot"
        case "queue_page", "library_roots", "saved_playlists", "playlist_entries", "artwork", "lyrics": return command
        case "list_tracks", "list_albums", "list_artists", "list_genres", "album_summaries", "list_groups", "history": return command
        case "enqueue_tracks", "insert_tracks", "clear_upcoming", "shuffle_upcoming": return "applied"
        case "save_upcoming": return "playlist_applied"
        case "create_playlist", "create_smart_playlist", "update_smart_playlist", "snapshot_smart_playlist", "rename_playlist", "delete_playlist", "playlist_add", "playlist_add_selection", "playlist_remove", "playlist_move": return "playlist_applied"
        case "enqueue_playlist": return "applied"
        case "add_root": return "root_added"
        case "rescan": return "scan_accepted"
        case "append", "append_stream", "append_track", "remove", "move", "clear", "play", "pause", "toggle_play_pause", "next", "previous", "stop", "seek", "set_gain", "set_resume": return "applied"
        default: return nil
        }
    }

    /// No caches, cookies, stored credentials or proxies. `timeout` bounds each request.
    nonisolated static func sessionConfiguration(timeout: TimeInterval) -> URLSessionConfiguration {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.urlCache = nil
        configuration.urlCredentialStorage = nil
        configuration.httpCookieStorage = nil
        configuration.httpShouldSetCookies = false
        configuration.connectionProxyDictionary = [:]
        configuration.requestCachePolicy = .reloadIgnoringLocalCacheData
        configuration.timeoutIntervalForRequest = timeout
        return configuration
    }

    /// The `result` object of an ok reply frame, re-encoded on its own. It keeps the complete
    /// result, including metadata fields this UI does not decode. Nil when there is none.
    nonisolated static func result(ofReply data: Data) -> Data? {
        guard let envelope = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let object = envelope["result"] as? [String: Any] else { return nil }
        return try? JSONSerialization.data(withJSONObject: object)
    }

    nonisolated static func validateEndpoint(_ endpoint: String) throws -> URL {
        let invalid = ServerFailure(
            code: "invalid_endpoint",
            message: "Use ws://127.x.x.x:port/ws or ws://[::1]:port/ws, without user information, a query, or a fragment."
        )
        guard endpoint.hasPrefix("ws://"), endpoint.hasSuffix("/ws") else { throw invalid }
        let authority = endpoint.dropFirst(5).dropLast(3)
        let host: String
        let portText: Substring
        if authority.hasPrefix("[::1]:") {
            host = "[::1]"
            portText = authority.dropFirst(6)
        } else {
            let parts = authority.split(separator: ":", omittingEmptySubsequences: false)
            guard parts.count == 2 else { throw invalid }
            let octets = parts[0].split(separator: ".", omittingEmptySubsequences: false)
            guard octets.count == 4, octets[0] == "127",
                  octets.allSatisfy({ octet in
                      !octet.isEmpty && octet.utf8.count <= 3
                          && octet.utf8.allSatisfy({ $0 >= 48 && $0 <= 57 })
                          && (octet.count == 1 || octet.first != "0")
                          && UInt8(octet) != nil
                  }) else { throw invalid }
            host = String(parts[0])
            portText = parts[1]
        }
        guard !portText.isEmpty, portText.utf8.count <= 5,
              portText.utf8.allSatisfy({ $0 >= 48 && $0 <= 57 }),
              let port = UInt16(portText), port > 0,
              let url = URL(string: "ws://\(host):\(port)/ws") else { throw invalid }
        return url
    }

    nonisolated static func readToken(at path: String) throws -> String {
        let descriptor = open((path as NSString).expandingTildeInPath, O_RDONLY | O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK)
        guard descriptor >= 0 else {
            throw ServerFailure(code: "token_read_failed", message: "The private authentication token file could not be opened.")
        }
        defer { close(descriptor) }
        var metadata = stat()
        guard fstat(descriptor, &metadata) == 0,
              metadata.st_mode & mode_t(S_IFMT) == mode_t(S_IFREG),
              metadata.st_nlink == 1, metadata.st_uid == geteuid(),
              metadata.st_mode & 0o777 == 0o600 else {
            throw ServerFailure(code: "token_permissions", message: "The token must be a regular, single-link file owned by the current user with permissions 0600; symbolic links are not accepted.")
        }
        guard metadata.st_size == 64 else {
            throw ServerFailure(code: "token_format", message: "The authentication token must contain exactly 64 lowercase hexadecimal bytes.")
        }
        var bytes = [UInt8](repeating: 0, count: 64)
        var offset = 0
        while offset < bytes.count {
            let count = bytes.withUnsafeMutableBytes { buffer in
                Darwin.read(descriptor, buffer.baseAddress!.advanced(by: offset), buffer.count - offset)
            }
            if count < 0, errno == EINTR { continue }
            guard count > 0 else {
                throw ServerFailure(code: "token_read_failed", message: "The private authentication token could not be read completely.")
            }
            offset += count
        }
        guard bytes.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) }) else {
            throw ServerFailure(code: "token_format", message: "The authentication token must contain exactly 64 lowercase hexadecimal bytes.")
        }
        return String(decoding: bytes, as: UTF8.self)
    }
}

/// No delegate method exposes a request, credential, or underlying error to UI/logs.
final class RedirectRejector: NSObject, URLSessionTaskDelegate, Sendable {
    private let rejected: @Sendable () -> Void

    init(rejected: @escaping @Sendable () -> Void) {
        self.rejected = rejected
    }

    func urlSession(
        _ session: URLSession,
        task: URLSessionTask,
        willPerformHTTPRedirection response: HTTPURLResponse,
        newRequest request: URLRequest,
        completionHandler: @escaping @Sendable (URLRequest?) -> Void
    ) {
        completionHandler(nil)
        rejected()
    }

    func urlSession(
        _ session: URLSession,
        task: URLSessionTask,
        didReceive challenge: URLAuthenticationChallenge,
        completionHandler: @escaping @Sendable (URLSession.AuthChallengeDisposition, URLCredential?) -> Void
    ) {
        completionHandler(.cancelAuthenticationChallenge, nil)
    }
}

struct ResultHeader: Decodable {
    let kind: String
    let snapshot: ControlState?

    private enum CodingKeys: String, CodingKey { case kind, snapshot }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        kind = try container.decode(String.self, forKey: .kind)
        switch kind {
        case "snapshot", "applied", "playlist_applied":
            snapshot = try container.decode(ControlState.self, forKey: .snapshot)
        case "queue_page", "root_added", "scan_accepted", "library_roots", "saved_playlists", "playlist_entries", "artwork", "lyrics",
             "list_tracks", "list_albums", "list_artists", "list_genres", "album_summaries", "list_groups", "history":
            snapshot = nil
        default:
            throw DecodingError.dataCorruptedError(forKey: .kind, in: container, debugDescription: "Unknown protocol v11 result kind")
        }
    }
}

enum IncomingMessage: Decodable {
    case hello(UInt32, String, ControlState)
    case state(ControlState)
    case success(UInt64, ResultHeader)
    case failure(UInt64, ServerFailure)

    private enum CodingKeys: String, CodingKey {
        case type, protocol_version, instance_id, snapshot, id, status, result, code, message, queue_revision
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        switch try container.decode(String.self, forKey: .type) {
        case "hello":
            self = .hello(
                try container.decode(UInt32.self, forKey: .protocol_version),
                try container.decode(String.self, forKey: .instance_id),
                try container.decode(ControlState.self, forKey: .snapshot)
            )
        case "state":
            self = .state(try container.decode(ControlState.self, forKey: .snapshot))
        case "reply":
            let id = try container.decode(UInt64.self, forKey: .id)
            switch try container.decode(String.self, forKey: .status) {
            case "ok":
                self = .success(id, try container.decode(ResultHeader.self, forKey: .result))
            case "error":
                self = .failure(id, ServerFailure(
                    code: try container.decode(String.self, forKey: .code),
                    message: try container.decode(String.self, forKey: .message),
                    queue_revision: try container.decode(UInt64.self, forKey: .queue_revision)
                ))
            default:
                throw DecodingError.dataCorruptedError(forKey: .status, in: container, debugDescription: "Unknown reply status")
            }
        default:
            throw DecodingError.dataCorruptedError(forKey: .type, in: container, debugDescription: "Unknown protocol v11 message type")
        }
    }
}
