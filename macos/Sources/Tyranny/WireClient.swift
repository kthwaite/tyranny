import Darwin
import Foundation
import Network
import TyrannyCore

/// A connection owns its waiters. Losing it never authorizes replaying a command.
@MainActor
final class WireClient {
    var onSnapshot: ((ControlState, String?) -> Void)?
    var onDisconnect: ((String) -> Void)?
    private(set) var instanceID: String?

    nonisolated static let requestLimit = RequestFrame.limit
    nonisolated static let responseLimit = 8 * 1024 * 1024
    private static let pendingLimit = 32

    private struct PendingRequest: Sendable {
        let continuation: CheckedContinuation<Data, any Error>
        let expectedKind: String?
        let deadline: Task<Void, Never>

        func cancelTasks() {
            deadline.cancel()
        }
    }

    private var generation = UUID()
    private var socket: NWConnection?
    private var frameBuffer = Data()
    private var frameLength = 4
    private var readingHeader = true
    private var helloDeadline: Task<Void, Never>?
    private var helloWaiter: CheckedContinuation<Void, any Error>?
    private var pending: [UInt64: PendingRequest] = [:]
    private var nextRequestID: UInt64 = 1
    private var sequence: UInt64?
    private var ready = false
    private let decoder = JSONDecoder()
    private let requestDeadlineSeconds: UInt64

    init(requestDeadlineSeconds: UInt64 = 10) {
        self.requestDeadlineSeconds = requestDeadlineSeconds
    }

    deinit {
        socket?.cancel()
        helloDeadline?.cancel()
        helloWaiter?.resume(throwing: CancellationError())
        for request in pending.values {
            request.cancelTasks()
            request.continuation.resume(throwing: ServerFailure(
                code: "uncertain_outcome",
                message: "The client closed before a reply arrived. The request outcome is uncertain; inspect server state before issuing it again."
            ))
        }
    }

    func connect(socketPath: String) async throws {
        try Task.checkCancellation()
        endConnection(
            ServerFailure(code: "connection_replaced", message: "The connection was replaced."),
            notify: false
        )

        let path = try Self.validateSocketPath(socketPath)
        let connectionID = generation
        let newSocket = NWConnection(to: .unix(path: path), using: .tcp)
        socket = newSocket
        newSocket.stateUpdateHandler = { [weak self] state in
            Task { @MainActor [weak self] in
                guard let self, self.generation == connectionID else { return }
                switch state {
                case .ready:
                    self.receiveFramePart(generation: connectionID)
                case .failed:
                    self.connectionFailed(generation: connectionID)
                default:
                    break
                }
            }
        }

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
                        ServerFailure(code: "hello_timeout", message: "The server did not send a protocol v\(WireProtocol.version) hello within 10 seconds."),
                        generation: connectionID
                    )
                }
                newSocket.start(queue: .main)
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
        let bytes: Data
        do {
            bytes = try RequestFrame.encode(command, fields: fields, id: id)
        } catch {
            throw ServerFailure(code: "invalid_request", message: "The command could not be encoded and was not sent.")
        }
        guard bytes.count <= Self.requestLimit else {
            throw ServerFailure(code: "request_too_large", message: "The command exceeds the 16 KiB request limit and was not sent.")
        }
        let frame = try JSONFrame.encode(bytes, limit: Self.requestLimit)
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
                pending[id] = PendingRequest(
                    continuation: continuation,
                    expectedKind: Self.resultKind(for: command),
                    deadline: deadline
                )
                socket.send(content: frame, completion: .contentProcessed { [weak self] error in
                    guard error != nil else { return }
                    Task { @MainActor [weak self] in
                        guard self?.generation == connectionID, self?.pending[id] != nil else { return }
                        self?.endConnection(
                            ServerFailure(code: "send_failed", message: "The connection failed while sending a request."),
                            generation: connectionID
                        )
                    }
                })
            }
        } onCancel: {
            Task { @MainActor [weak self] in
                guard self?.generation == connectionID, self?.pending[id] != nil else { return }
                // Cancellation cannot retract a command already handed to Network.framework.
                self?.endConnection(
                    ServerFailure(code: "request_cancelled", message: "A request was cancelled before its reply arrived."),
                    generation: connectionID
                )
            }
        }
    }

    /// Exactly one receive is outstanding. Request cancellation closes the entire connection;
    /// it never cancels a partial frame and starts another read on the same byte stream.
    private func receiveFramePart(generation connectionID: UUID) {
        guard generation == connectionID, let socket else { return }
        let remaining = frameLength - frameBuffer.count
        socket.receive(minimumIncompleteLength: remaining, maximumLength: remaining) { [weak self] data, _, complete, error in
            Task { @MainActor [weak self] in
                guard let self, self.generation == connectionID else { return }
                if let data {
                    if self.frameBuffer.isEmpty {
                        self.frameBuffer = data
                    } else {
                        self.frameBuffer.append(data)
                    }
                }
                do {
                    if self.frameBuffer.count == self.frameLength {
                        if self.readingHeader {
                            // Validate the length before requesting or allocating its payload.
                            self.frameLength = try JSONFrame.payloadLength(self.frameBuffer, limit: Self.responseLimit)
                            self.frameBuffer = Data()
                            self.readingHeader = false
                            guard !complete, error == nil else {
                                throw self.protocolFailure("The server closed in the middle of a frame.")
                            }
                            self.receiveFramePart(generation: connectionID)
                        } else {
                            try self.receive(self.frameBuffer, generation: connectionID)
                            guard self.generation == connectionID else { return }
                            self.frameBuffer = Data()
                            self.frameLength = 4
                            self.readingHeader = true
                            if complete || error != nil {
                                self.connectionFailed(generation: connectionID)
                            } else {
                                self.receiveFramePart(generation: connectionID)
                            }
                        }
                    } else if complete || error != nil || data?.isEmpty != false {
                        self.connectionFailed(generation: connectionID)
                    } else {
                        self.receiveFramePart(generation: connectionID)
                    }
                } catch {
                    self.endConnection(
                        (error as? ServerFailure) ?? self.protocolFailure("The server sent an invalid frame length."),
                        generation: connectionID
                    )
                }
            }
        }
    }

    private func receive(_ data: Data, generation connectionID: UUID) throws {
        var bytes = data.makeIterator()
        var utf8 = Unicode.UTF8.ForwardParser()
        validation: while true {
            switch utf8.parseScalar(from: &bytes) {
            case .valid: continue
            case .emptyInput: break validation
            case .error: throw protocolFailure("The server sent non-UTF-8 JSON.")
            }

        }
        let incoming: IncomingMessage
        do {
            incoming = try decoder.decode(IncomingMessage.self, from: data)
        } catch {
            throw protocolFailure("The server sent malformed or incompatible protocol v\(WireProtocol.version) JSON. Upgrade the client and server together.")
        }

        if case .hello(let version, let instance, let snapshot) = incoming {
            guard !ready, let waiter = helloWaiter else {
                throw protocolFailure("The server sent more than one hello.")
            }
            guard version == WireProtocol.version else {
                throw protocolFailure("The server protocol is incompatible; this client requires version \(WireProtocol.version).")
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
            throw protocolFailure("The server sent a message before its protocol v\(WireProtocol.version) hello.")
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

    private func connectionFailed(generation connectionID: UUID) {
        endConnection(
            ServerFailure(code: "connection_lost", message: "The server connection closed or could not be established. Check that the local server is running."),
            generation: connectionID
        )
    }

    private func endConnection(_ failure: ServerFailure, generation expected: UUID? = nil, notify: Bool = true) {
        if let expected, expected != generation { return }
        instanceID = nil
        let wasActive = socket != nil || helloWaiter != nil
        generation = UUID()
        ready = false
        sequence = nil
        helloDeadline?.cancel()
        helloDeadline = nil
        socket?.cancel()
        socket = nil
        frameBuffer = Data()
        frameLength = 4
        readingHeader = true
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
        case "search_directory": return "directory_results"
        case "count_directory_click": return "directory_click_counted"
        case "list_tracks", "list_albums", "list_artists", "list_genres", "album_summaries", "list_groups", "history": return command
        case "enqueue_tracks", "insert_tracks", "clear_upcoming", "shuffle_upcoming": return "applied"
        case "save_upcoming": return "playlist_applied"
        case "create_playlist", "create_smart_playlist", "update_smart_playlist", "snapshot_smart_playlist", "rename_playlist", "delete_playlist", "playlist_add", "playlist_add_selection", "playlist_remove", "playlist_move": return "playlist_applied"
        case "enqueue_playlist": return "applied"
        case "stations": return "stations"
        case "stats", "wrapped", "stats_years": return command
        case "musicbrainz_lookup": return "tag_proposal"
        case "apply_tags", "revert_tag_edit": return "tag_apply"
        case "tag_edits": return "tag_edits"
        case "add_stations", "update_station", "remove_station": return "stations_applied"
        case "add_root": return "root_added"
        case "rescan": return "scan_accepted"
        case "set_track_liked": return "track_liked"
        case "append", "append_stream", "append_track", "remove", "move", "clear", "play", "pause", "toggle_play_pause", "next", "previous", "stop", "seek", "set_gain", "set_resume": return "applied"
        default: return nil
        }
    }

    /// The result object of an ok reply, retaining metadata the UI does not decode.
    nonisolated static func result(ofReply data: Data) -> Data? {
        guard let envelope = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let object = envelope["result"] as? [String: Any] else { return nil }
        return try? JSONSerialization.data(withJSONObject: object)
    }

    nonisolated static func validateSocketPath(_ value: String) throws -> String {
        let invalid = ServerFailure(
            code: "invalid_socket",
            message: "Use an absolute Unix socket path in a private directory owned by the current user; the directory must be 0700 and the socket 0600, without symbolic links."
        )
        let path = value
        guard path.hasPrefix("/"), !path.utf8.contains(0),
              !path.split(separator: "/").contains("..") else { throw invalid }
        let url = URL(fileURLWithPath: path)
        let parent = url.deletingLastPathComponent()
        var directory = stat()
        var file = stat()
        let unavailable = ServerFailure(code: "socket_unavailable", message: "The server socket does not exist.")
        guard lstat(parent.path, &directory) == 0 else {
            throw errno == ENOENT ? unavailable : invalid
        }
        guard directory.st_mode & mode_t(S_IFMT) == mode_t(S_IFDIR),
              directory.st_uid == geteuid(), directory.st_mode & 0o7777 == 0o700 else { throw invalid }
        // Foundation's resolvingSymlinksInPath rewrites /private/tmp back to the
        // /tmp symlink on macOS, which is not a canonical path for lstat checks.
        guard let resolved = realpath(parent.path, nil) else { throw invalid }
        defer { free(resolved) }
        let canonicalParent = URL(fileURLWithPath: String(cString: resolved))
        var ancestor = canonicalParent.deletingLastPathComponent()
        while true {
            var info = stat()
            guard lstat(ancestor.path, &info) == 0,
                  info.st_mode & mode_t(S_IFMT) == mode_t(S_IFDIR),
                  info.st_uid == 0 || info.st_uid == geteuid(),
                  info.st_mode & 0o022 == 0 || (info.st_uid == 0 && info.st_mode & 0o1000 != 0) else {
                throw invalid
            }
            if ancestor.path == "/" { break }
            ancestor.deleteLastPathComponent()
        }
        let canonical = canonicalParent.appendingPathComponent(url.lastPathComponent).path
        let address = sockaddr_un()
        guard canonical.utf8.count < MemoryLayout.size(ofValue: address.sun_path) else {
            throw ServerFailure(code: "invalid_socket", message: "The Unix socket path is too long for this platform.")
        }
        guard lstat(path, &file) == 0 else {
            throw errno == ENOENT ? unavailable : invalid
        }
        guard file.st_mode & mode_t(S_IFMT) == mode_t(S_IFSOCK),
              file.st_uid == geteuid(), file.st_mode & 0o7777 == 0o600 else { throw invalid }
        // Network.framework has no public peer-descriptor accessor. Unlike the Rust client,
        // this client relies on the owned private directory and socket, not a peer-UID query.
        return canonical
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
        case "snapshot", "applied", "playlist_applied", "stations_applied":
            snapshot = try container.decode(ControlState.self, forKey: .snapshot)
        case "queue_page", "root_added", "scan_accepted", "library_roots", "saved_playlists", "playlist_entries", "artwork", "lyrics",
             "directory_results", "directory_click_counted", "track_liked",
             "list_tracks", "list_albums", "list_artists", "list_genres", "album_summaries", "list_groups", "history",
             "stations", "stats", "wrapped", "stats_years", "tag_proposal", "tag_apply", "tag_edits":
            snapshot = nil
        default:
            throw DecodingError.dataCorruptedError(forKey: .kind, in: container, debugDescription: "Unknown protocol v\(WireProtocol.version) result kind")
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
            throw DecodingError.dataCorruptedError(forKey: .type, in: container, debugDescription: "Unknown protocol v\(WireProtocol.version) message type")
        }
    }
}
