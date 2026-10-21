import Foundation
import Observation
import TyrannyCore

/// The MusicBrainz tag lookup: one target album at a time. Lookups run on their own lane
/// (`lookupWire`), so a new lookup or closing the sheet can cut one off without touching
/// writes. Apply, revert and the edit list share `editWire`.
@MainActor @Observable
final class TaggingModel {
    static let acoustIDKeyDefault = "acoustidAPIKey"

    struct Target: Identifiable, Equatable {
        let albumID: UInt64
        let title: String
        var id: UInt64 { albumID }
    }

    enum Phase: Equatable {
        case lookingUp, reviewing, applying, reverting, applied(TagApplyResult), failed(String)
    }

    /// The sheet's item. Setting nil closes the sheet and cuts off any lookup.
    var target: Target? {
        didSet {
            guard target != oldValue else { return }
            if target == nil { reset() }
        }
    }
    private(set) var phase: Phase = .lookingUp
    private(set) var proposal: TagProposal?
    /// The picker's list: the first lookup's candidates for this target.
    private(set) var candidates: [TagCandidate] = []
    var selection: TagSelection?
    private(set) var edits: [TagEditSummary] = []
    private(set) var editsError: String?
    /// True once the applied edit was reverted.
    private(set) var isUndone = false

    /// A write or revert is in flight. It is never cancelled: a half-finished write would leave files mixed.
    var isEditing: Bool { phase == .applying || phase == .reverting }

    var hasAcoustIDKey: Bool { acoustIDKey != nil }

    private var acoustIDKey: String? {
        AcoustIDKey.normalized(UserDefaults.standard.string(forKey: Self.acoustIDKeyDefault) ?? "")
    }

    @ObservationIgnored private weak var player: PlayerModel?
    @ObservationIgnored private let lookupWire = WireClient(requestDeadlineSeconds: TagRequests.lookupDeadlineSeconds)
    @ObservationIgnored private let editWire = WireClient(requestDeadlineSeconds: TagRequests.editDeadlineSeconds)
    @ObservationIgnored private var lookupTask: Task<Void, Never>?
    @ObservationIgnored private var editTail: Task<Void, Never> = Task {}
    @ObservationIgnored private var ticket = UUID()
    @ObservationIgnored private var lastReleaseID: String?
    @ObservationIgnored private var lastFingerprint = false

    func attach(_ player: PlayerModel) {
        self.player = player
    }

    func begin(albumID: UInt64, title: String) {
        reset()
        target = Target(albumID: albumID, title: title)
        lookUp(releaseID: nil, fingerprint: false)
    }

    func lookUp(releaseID: String?, fingerprint: Bool) {
        guard let target else { return }
        lastReleaseID = releaseID
        lastFingerprint = fingerprint
        cancelLookup()
        phase = .lookingUp
        let ticket = ticket
        // The key goes out only for a fingerprint lookup: the request fingerprints whenever it has one.
        let key = fingerprint ? acoustIDKey : nil
        lookupTask = Task { [weak self] in
            guard let self else { return }
            do {
                let reply = try await fetchProposal(albumID: target.albumID, releaseID: releaseID, key: key, retryStale: true)
                guard self.ticket == ticket, self.target == target else { return }
                proposal = reply.proposal
                if releaseID == nil { candidates = reply.proposal.candidates }
                selection = TagSelection(reply.proposal)
                phase = .reviewing
            } catch {
                guard !Task.isCancelled, self.ticket == ticket, self.target == target else { return }
                phase = .failed(Self.message(for: error))
            }
        }
    }

    /// Re-runs the last lookup. The Try Again button.
    func retry() {
        lookUp(releaseID: lastReleaseID, fingerprint: lastFingerprint)
    }

    func apply() {
        guard case .reviewing = phase, let request = selection?.applyRequest() else { return }
        phase = .applying
        isUndone = false
        let ticket = ticket
        // Unreferenced on purpose: closing the sheet must not cancel a write. The edit lane queues it.
        Task { [weak self] in
            guard let self else { return }
            do {
                let data = try await editRequest(request.name, request.fields)
                let result = try JSONDecoder().decode(TagApplyResult.self, from: data)
                if self.ticket == ticket { phase = .applied(result) }
            } catch {
                if self.ticket == ticket { phase = .failed(Self.editMessage(for: error)) }
            }
            await refreshEdits()
        }
    }

    func undo() {
        guard case .applied(let result) = phase, let editID = result.edit_id, !isUndone else { return }
        let ticket = ticket
        phase = .reverting
        let request = TagRequests.revert(editID: editID)
        // Unreferenced on purpose: closing the sheet must not cancel a write. The edit lane queues it.
        Task { [weak self] in
            guard let self else { return }
            do {
                let data = try await editRequest(request.name, request.fields)
                let reverted = try JSONDecoder().decode(TagApplyResult.self, from: data)
                if self.ticket == ticket {
                    isUndone = true
                    phase = .applied(reverted)
                }
            } catch {
                if self.ticket == ticket { phase = .failed(Self.editMessage(for: error)) }
            }
            await refreshEdits()
        }
    }

    func refreshEdits() async {
        do {
            let data = try await editRequest(TagRequests.edits.name, TagRequests.edits.fields)
            edits = try JSONDecoder().decode(TagEditsResponse.self, from: data).edits
            editsError = nil
        } catch {
            if let failure = error as? ServerFailure {
                editsError = TagEditMessage.listText(for: failure)
            } else {
                editsError = error.localizedDescription
            }
        }
    }

    func revert(_ editID: UInt64) async {
        let request = TagRequests.revert(editID: editID)
        do {
            _ = try await editRequest(request.name, request.fields)
        } catch {
            editsError = Self.editMessage(for: error)
            return
        }
        await refreshEdits()
    }

    // MARK: - Wire

    private func fetchProposal(albumID: UInt64, releaseID: String?, key: String?, retryStale: Bool) async throws -> TagProposalResponse {
        guard let player else { throw Self.unavailable }
        let session = try await open(lookupWire, player)
        let revision = player.snapshot?.library.revision ?? 0
        let request = TagRequests.lookup(albumID: albumID, libraryRevision: revision, releaseID: releaseID, acoustIDKey: key)
        try Task.checkCancellation()
        do {
            let data = try await lookupWire.request(request.name, fields: request.fields)
            try Task.checkCancellation()
            guard lookupWire.instanceID == session else { throw Self.sessionChanged }
            return try JSONDecoder().decode(TagProposalResponse.self, from: data)
        } catch let failure as ServerFailure where failure.code == "stale_library_revision" && retryStale {
            try Task.checkCancellation()
            return try await fetchProposal(albumID: albumID, releaseID: releaseID, key: key, retryStale: false)
        }
    }

    private func editRequest(_ name: String, _ fields: [String: JSONValue]) async throws -> Data {
        guard let player else { throw Self.unavailable }
        // One request at a time: each connect replaces the connection, which would kill a write in flight.
        return try await exclusive { [self] in
            let session = try await open(editWire, player)
            let data = try await editWire.request(name, fields: fields)
            guard editWire.instanceID == session else { throw Self.sessionChanged }
            return data
        }
    }

    /// Runs edit-lane operations in order. The operation itself is not cancelled with its caller.
    private func exclusive<T: Sendable>(_ operation: @escaping @MainActor () async throws -> T) async throws -> T {
        let previous = editTail
        let work = Task { @MainActor in
            await previous.value
            return try await operation()
        }
        editTail = Task { _ = try? await work.value }
        return try await work.value
    }

    /// Connects on demand and refuses a server other than the one the window shows.
    private func open(_ wire: WireClient, _ player: PlayerModel) async throws -> String {
        guard let expected = player.serverInstance else { throw Self.unavailable }
        try await wire.connect(endpoint: player.endpoint, tokenPath: player.tokenPath)
        guard let instance = wire.instanceID, instance == expected else { throw Self.sessionChanged }
        return instance
    }

    private static let unavailable = ServerFailure(code: "not_connected", message: "Connect to the server first.")
    private static let sessionChanged = ServerFailure(code: "session_changed", message: "The server session changed. Look the album up again.")

    private static func message(for error: Error) -> String {
        if let failure = error as? ServerFailure { return TagLookupMessage.text(for: failure) }
        return error.localizedDescription
    }

    /// For an apply or a revert.
    private static func editMessage(for error: Error) -> String {
        if let failure = error as? ServerFailure { return TagEditMessage.text(for: failure) }
        return error.localizedDescription
    }

    private func cancelLookup() {
        ticket = UUID()
        lookupTask?.cancel()
        lookupTask = nil
        lookupWire.disconnect()
    }

    private func reset() {
        cancelLookup()
        // The edit lane is never touched here: writes, reverts and Settings requests in flight finish on
        // their own, and `open()` reconnects on demand.
        phase = .lookingUp
        proposal = nil
        candidates = []
        selection = nil
        isUndone = false
        lastReleaseID = nil
        lastFingerprint = false
    }
}
