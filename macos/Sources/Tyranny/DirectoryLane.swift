import Foundation
import TyrannyCore

/// A dedicated connection for `search_directory`, never the transport lane. The server answers
/// one request per connection at a time and sends no state while one is pending, and a search
/// can take several seconds. It follows the socket path and server instance like the lyrics lane.
@MainActor
final class DirectoryLane {
    private struct Context: Equatable {
        let socketPath: String
        let instance: String
    }

    private let wire = WireClient(requestDeadlineSeconds: 30)
    private var context: Context?
    /// Bumps when the context changes and when a newer request replaces an older one.
    private var ticket = UUID()

    /// `instance` is nil while disconnected. A change drops the connection and any request on it.
    func synchronize(socketPath: String, instance: String?) {
        let next = instance.map { Context(socketPath: socketPath, instance: $0) }
        guard next != context else { return }
        context = next
        drop()
    }

    /// Connects fresh, so a newer search never waits behind an older one.
    func request(_ request: CommandRequest) async throws -> Data {
        guard let context else {
            throw ServerFailure(code: "disconnected", message: "Disconnected. Connect before searching.")
        }
        drop()
        let mine = ticket
        do {
            try await wire.connect(socketPath: context.socketPath)
            guard wire.instanceID == context.instance else {
                throw ServerFailure(code: "directory_session_changed", message: "The server session changed. Search again.")
            }
            return try await wire.request(request.name, fields: request.fields)
        } catch {
            // A newer request owns the connection now; leave it alone.
            if ticket == mine { wire.disconnect() }
            throw error
        }
    }

    private func drop() {
        ticket = UUID()
        wire.disconnect()
    }
}
