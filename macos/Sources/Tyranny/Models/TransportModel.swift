import Foundation
import Observation
import TyrannyCore

/// Transport state for the toolbar, read from the server snapshot, and the
/// transport commands. The server decides `can_next` and `can_previous`
/// (Repeat All wraps Previous; Repeat One lets Next move on), so the client
/// adds no rules of its own.
@MainActor @Observable
final class TransportModel {
    @ObservationIgnored private weak var player: PlayerModel?
    @ObservationIgnored private var mediaControls: SystemMediaControls?
    /// The volume the user set locally, shown until the snapshot reports it.
    private(set) var pendingVolume: Int?
    @ObservationIgnored private var queuedVolume: Int?
    @ObservationIgnored private var volumeTask: Task<Void, Never>?

    func attach(_ player: PlayerModel) {
        self.player = player
        mediaControls = SystemMediaControls { [weak self] action in
            guard let self else { return }
            switch action {
            case .play: play()
            case .pause: pause()
            case .togglePlayPause: playPause()
            case .next: next()
            case .previous: previous()
            case .stop: stop()
            case .seek(let entryID, let seconds): seek(seconds: seconds, entryID: entryID)
            case .setRepeat(let mode): setRepeat(mode)
            case .setShuffle(let enabled): setShuffle(enabled)
            }
        }
    }

    /// Called by `PlayerModel` when the snapshot, connection or busy state changes.
    func stateChanged() {
        guard let player else { return }
        mediaControls?.update(snapshot: player.connected ? player.snapshot : nil, canMutate: player.canMutate)
        if volumeTask == nil, let pendingVolume,
           player.snapshot.map({ Int($0.settings.volume) == pendingVolume }) ?? true {
            self.pendingVolume = nil
        }
    }

    func shutdown() {
        mediaControls?.shutdown()
        mediaControls = nil
    }

    // MARK: - Read

    var snapshot: ControlState? { player?.connected == true ? player?.snapshot : nil }
    var nowPlaying: NowPlaying? { snapshot?.now_playing }
    var canMutate: Bool { player?.canMutate ?? false }
    var playbackSession: UInt64 { player?.playbackSession ?? 0 }
    var currentID: UInt64? { snapshot?.current_id }

    var title: String {
        guard player?.connected == true else { return "Not Connected" }
        return nowPlaying?.title ?? "Not Playing"
    }
    var artist: String { nowPlaying?.artist ?? "" }
    var album: String { nowPlaying?.album ?? "" }
    /// "artist — album", leaving out an empty side.
    var artistAlbum: String {
        [artist, album].filter { !$0.isEmpty }.joined(separator: " — ")
    }
    var elapsed: Double { player?.positionSeconds ?? 0 }
    var duration: Double? { player?.durationSeconds }
    var elapsedText: String { nowPlaying == nil ? "" : DisplayFormat.clock(elapsed) }
    var remainingText: String { nowPlaying == nil ? "" : DisplayFormat.remaining(elapsed: elapsed, duration: duration) }
    var format: String { nowPlaying?.format ?? "" }
    var gainMode: String { snapshot?.settings.gain_mode ?? "off" }
    var gainLabel: String? { DisplayFormat.gainLabel(mode: gainMode, gainDB: nowPlaying?.gain_db) }
    var formatLine: String {
        guard let nowPlaying else { return "" }
        return DisplayFormat.formatLine(format: nowPlaying.format, mode: gainMode, gainDB: nowPlaying.gain_db)
    }
    var error: String? { snapshot?.error }
    var metadataError: String? { nowPlaying?.metadata_error }

    var volume: Int { pendingVolume ?? Int(snapshot?.settings.volume ?? 100) }
    var `repeat`: RepeatMode { snapshot?.settings.repeat ?? .off }
    var shuffle: Bool { snapshot?.settings.shuffle ?? false }
    var resumeOnStart: Bool { snapshot?.settings.resume_on_start ?? false }

    var isPlaying: Bool { ["playing", "buffering"].contains(snapshot?.transport ?? "") }
    var canPlayPause: Bool { canMutate && (snapshot?.queue_length ?? 0) > 0 }
    var canNext: Bool { canMutate && snapshot?.can_next == true }
    var canPrevious: Bool { canMutate && snapshot?.can_previous == true }
    var canChangeModes: Bool { canMutate }
    var canSetVolume: Bool { player?.connected == true }

    // MARK: - Commands

    func play() { apply("play", ["entry_id": .null]) }
    func pause() { apply("pause") }
    func playPause() { apply("toggle_play_pause") }
    func stop() { apply("stop") }
    func next() {
        guard snapshot?.can_next == true else { return }
        apply("next")
    }
    func previous() {
        guard snapshot?.can_previous == true else { return }
        apply("previous")
    }

    /// Seeks within the current entry.
    func seek(to seconds: Double) {
        guard let entryID = currentID else { return }
        seek(seconds: seconds, entryID: entryID)
    }

    /// Seeks within `entryID`, only while it is still the current entry.
    func seek(seconds: Double, entryID: UInt64) {
        guard seconds.isFinite, seconds >= 0, let state = snapshot,
              state.current_id == entryID, state.now_playing?.entry_id == entryID,
              state.sample_rate > 0, let duration = state.now_playing?.duration_frames,
              duration > 0 else { return }
        let frames = seconds * Double(state.sample_rate)
        guard frames.isFinite, frames < Double(UInt64.max) else { return }
        let position = min(UInt64(frames), duration - 1)
        apply("seek", ["entry_id": .uint(entryID), "position_frames": .uint(position)])
    }

    /// Sends the latest value only: one `set_volume` in flight, newer values coalesced.
    /// It does not mark the model busy, so dragging the slider never blocks other controls.
    func setVolume(_ percent: Int) {
        guard let player, player.connected else { return }
        let value = min(max(percent, 0), 100)
        pendingVolume = value
        queuedVolume = value
        guard volumeTask == nil else { return }
        volumeTask = Task { [weak self] in
            while let self, let value = self.queuedVolume, let player = self.player, player.connected {
                self.queuedVolume = nil
                do {
                    let _: Applied = try await player.send("set_volume", ["percent": .int(value)])
                } catch {
                    player.message = error.localizedDescription
                    self.queuedVolume = nil
                    self.pendingVolume = nil
                    break
                }
            }
            guard let self else { return }
            volumeTask = nil
            stateChanged()
        }
    }

    func setRepeat(_ mode: RepeatMode) { apply("set_repeat", ["mode": .string(mode.rawValue)]) }
    /// The toolbar toggle: off → all → one → off.
    func cycleRepeat() { setRepeat(self.repeat.cycled) }
    func setShuffle(_ enabled: Bool) { apply("set_shuffle", ["enabled": .bool(enabled)]) }
    func setGain(_ mode: String) { apply("set_gain", ["mode": .string(mode)]) }
    func setResume(_ enabled: Bool) { apply("set_resume", ["enabled": .bool(enabled)]) }

    private func apply(_ command: String, _ fields: [String: JSONValue] = [:]) {
        guard let player else { return }
        player.mutate { let _: Applied = try await player.send(command, fields) }
    }
}
