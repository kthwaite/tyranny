import Foundation
import Observation
import TyrannyCore

/// A dedicated connection, never the transport lane. Only the mounted, visible pane asks
/// for the authoritative current queue occurrence; browsing creates no lyrics demand.
@MainActor @Observable
final class LyricsStore {
    enum State: String { case loading, ready, missing, unavailable, instrumental }
    private struct Context: Equatable {
        let endpoint: String
        let tokenPath: String
        let instance: String
        let revision: UInt64
        let entryID: UInt64
    }

    private(set) var isVisible = false
    private(set) var state: State = .missing
    private(set) var payload: LyricsPayload?
    private(set) var timeline = LyricsTimeline("")
    private(set) var generation: UInt64 = 0
    @ObservationIgnored private var context: Context?
    @ObservationIgnored private var mounted = false
    @ObservationIgnored private var connected = false
    @ObservationIgnored private var task: Task<Void, Never>?
    @ObservationIgnored private var ticket = UUID()
    @ObservationIgnored private let wire = WireClient(requestDeadlineSeconds: 30)

    func setVisible(_ visible: Bool) {
        guard visible != isVisible else { return }
        isVisible = visible
        refresh()
    }

    func setMounted(_ mounted: Bool) {
        guard self.mounted != mounted else { return }
        self.mounted = mounted
        refresh()
    }

    func synchronize(endpoint: String, tokenPath: String, instance: String?, revision: UInt64?, entryID: UInt64?, invalidate: Bool = false) {
        let next: Context?
        if let instance, let revision, let entryID {
            next = Context(endpoint: endpoint, tokenPath: tokenPath, instance: instance, revision: revision, entryID: entryID)
        } else { next = nil }
        let isConnected = instance != nil
        guard next != context || connected != isConnected || invalidate else { return }
        connected = isConnected
        context = next
        refresh()
    }

    private func refresh() {
        ticket = UUID()
        task?.cancel()
        task = nil
        wire.disconnect()
        payload = nil
        timeline = LyricsTimeline("")
        generation &+= 1
        state = connected ? .missing : .unavailable
        guard isVisible, mounted, let context else { return }
        state = .loading
        let ticket = ticket
        task = Task { [weak self] in
            guard let self else { return }
            do {
                try await wire.connect(endpoint: context.endpoint, tokenPath: context.tokenPath)
                guard wire.instanceID == context.instance else {
                    throw ServerFailure(code: "lyrics_session_changed", message: "Lyrics belong to a different server session.")
                }
                let data = try await wire.request("lyrics", fields: [
                    "entry_id": .uint(context.entryID), "library_revision": .uint(context.revision),
                ])
                try Task.checkCancellation()
                let reply = try JSONDecoder().decode(LyricsResponse.self, from: data)
                guard self.ticket == ticket, self.context == context, isVisible, mounted else { return }
                guard wire.instanceID == context.instance else {
                    throw ServerFailure(code: "lyrics_session_changed", message: "Lyrics belong to a different server session.")
                }
                payload = reply.lyrics
                timeline = LyricsTimeline(reply.lyrics.synced_lrc ?? "")
                state = State(rawValue: reply.lyrics.status.rawValue) ?? .unavailable
                // A malformed/empty synchronized response must not render a blank pane.
                if state == .ready, timeline.lines.isEmpty,
                   reply.lyrics.plain_text?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty != false {
                    state = .missing
                }
            } catch {
                guard !Task.isCancelled, self.ticket == ticket, self.context == context, isVisible, mounted else { return }
                state = .unavailable
            }
            guard self.ticket == ticket else { return }
            task = nil
        }
    }
}
