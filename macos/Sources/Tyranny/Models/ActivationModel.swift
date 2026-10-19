import Foundation
import Observation
import TyrannyCore

/// The rows to send, and how to send them again after the library moved on.
struct SelectionRequest {
    let selection: TrackSelection
    /// Called after a `stale_library_revision` reply, with the snapshot already refetched.
    /// Returns the selection under the current library revision, or throws `ListChangedError`
    /// when the rows are gone.
    let retarget: @MainActor () async throws -> TrackSelection

    /// A selection whose rows do not depend on positions: the same selection under the
    /// snapshot's current library revision.
    @MainActor static func source(_ selection: TrackSelection, player: PlayerModel) -> SelectionRequest {
        SelectionRequest(selection: selection) { [weak player] in
            guard let revision = player?.snapshot?.library.revision,
                  let fresh = SelectionRetarget.apply(selection, libraryRevision: revision, delta: 0) else {
                throw ListChangedError()
            }
            return fresh
        }
    }
}

/// One user request to play or add rows. Every list, grid and table builds one and hands it
/// to `ActivationModel.start`.
struct EnqueueJob {
    let action: ActivationAction
    let source: QueueSource
    let request: SelectionRequest
    /// Turn shuffle on before the play, so the server starts at a shuffle pick (album Shuffle).
    var shuffleFirst = false
    /// The status bar text after the server accepted it.
    var status: String?
}

/// The one path from a user request to `enqueue_tracks`: decide by the Up Next policy, ask
/// "Clear Up Next?" when the policy says so, send, and retry once per stale revision.
@MainActor @Observable
final class ActivationModel {
    struct Prompt: Identifiable {
        let id = UUID()
        let job: EnqueueJob
    }

    /// The pending "Clear Up Next?" sheet.
    var prompt: Prompt?

    @ObservationIgnored private weak var player: PlayerModel?

    func attach(_ player: PlayerModel) { self.player = player }

    static var policy: UpNextPolicy {
        UserDefaults.standard.string(forKey: "upNextOnPlay").flatMap(UpNextPolicy.init(rawValue:)) ?? .ask
    }

    func start(_ job: EnqueueJob) {
        guard let player else { return }
        player.mutate { [weak self] in
            try await self?.send(job, choice: nil)
        }
    }

    func cancelPrompt() { prompt = nil }

    /// The sheet's answer. `remember` stores it as the policy ("Don't ask again").
    func answer(_ choice: UpNextChoice, remember: Bool) {
        guard let job = prompt?.job, let player else { return }
        prompt = nil
        if remember { UserDefaults.standard.set(choice.remembered.rawValue, forKey: "upNextOnPlay") }
        player.mutate { [weak self] in
            try await self?.send(job, choice: choice)
        }
    }

    /// Decides, then sends. Without a `choice` the Up Next policy decides from the queue as the
    /// snapshot's revision saw it, so `expected_revision` matches what the decision read.
    /// `stale_revision`: the queue moved; refetch the snapshot and decide again (entries may
    /// have arrived: the new decision may ask). `stale_library_revision`: the library moved;
    /// refetch the snapshot, then relocate the rows. One retry for each.
    private func send(_ job: EnqueueJob, choice: UpNextChoice?) async throws {
        guard let player else { return }
        var selection = job.request.selection
        var retriedQueue = false, retriedLibrary = false
        var shuffled = false
        while true {
            guard player.connected, let state = player.snapshot else {
                throw ServerFailure(code: "connection_changed", message: "Disconnected. Nothing was queued.")
            }
            do {
                let placement: Placement, play: Bool
                if let choice {
                    (placement, play) = (choice.placement, true)
                } else {
                    let upcoming = job.action == .play
                        ? try await player.hasUpcoming(atQueueRevision: state.queue_revision) : false
                    switch ActivationPlan.make(job.action, policy: Self.policy, hasUpcoming: upcoming) {
                    case .ask:
                        prompt = Prompt(job: job)
                        return
                    case .enqueue(let decided, let decidedPlay):
                        (placement, play) = (decided, decidedPlay)
                    }
                }
                if job.shuffleFirst, !shuffled {
                    shuffled = true
                    let _: Applied = try await player.send("set_shuffle", ["enabled": .bool(true)])
                }
                let request = EnqueueRequest(selection: selection, placement: placement, play: play,
                                             source: job.source)
                let _: Applied = try await player.send("enqueue_tracks",
                                                       try request.fields(expectedRevision: state.queue_revision))
                if let status = job.status { player.message = status }
                return
            } catch let failure as ServerFailure where failure.code == "stale_revision" && !retriedQueue {
                retriedQueue = true
                try await player.refreshSnapshot()
            } catch let failure as ServerFailure where failure.code == "stale_library_revision" && !retriedLibrary {
                retriedLibrary = true
                try await player.refreshSnapshot()
                selection = try await job.request.retarget()
            }
        }
    }
}
