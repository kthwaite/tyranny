import Foundation
import MediaPlayer
import TyrannyLikesSmokeCore

@MainActor
final class SystemMediaControls {
    enum Action: Sendable {
        case play, pause, togglePlayPause, next, previous, stop
        case seek(entryID: UInt64, seconds: Double)
        case setRepeat(RepeatMode)
        case setShuffle(Bool)
    }

    private static weak var owner: SystemMediaControls?
    private static var ownerID: UUID?

    private let id = UUID()
    private let handle: @MainActor (Action) -> Void
    private let gate = MediaCommandGate()
    private let commands = MPRemoteCommandCenter.shared()
    private let information = MPNowPlayingInfoCenter.default()
    private var registrations: [(command: MPRemoteCommand, target: Any)] = []
    private var publication: Publication?
    private var isShutdown = false

    init(handle: @escaping @MainActor (Action) -> Void) {
        self.handle = handle
        // Standalone model gates must not participate in the user's system media session.
        guard Bundle.main.bundleURL.pathExtension == "app", Bundle.main.bundleIdentifier != nil else {
            isShutdown = true
            return
        }
        Self.owner?.shutdown()
        Self.owner = self
        Self.ownerID = id
        setEnabled(MediaCommandAvailability())
        disableUnsupportedCommands()
        register(commands.playCommand, action: .play)
        register(commands.pauseCommand, action: .pause)
        register(commands.togglePlayPauseCommand, action: .togglePlayPause)
        register(commands.nextTrackCommand, action: .next)
        register(commands.previousTrackCommand, action: .previous)
        register(commands.stopCommand, action: .stop)
        registerSeek()
        registerModes()
    }

    func update(snapshot: ControlState?, canMutate: Bool) {
        guard !isShutdown, Self.ownerID == id else { return }
        let availability = MediaCommandAvailability(snapshot: snapshot, canMutate: canMutate)
        gate.update(availability)
        setEnabled(availability)
        if let settings = snapshot?.settings {
            commands.changeRepeatModeCommand.currentRepeatType = switch settings.repeat {
            case .off: .off
            case .all: .all
            case .one: .one
            }
            commands.changeShuffleModeCommand.currentShuffleType = settings.shuffle ? .items : .off
        }
        guard let snapshot, let item = snapshot.now_playing, item.entry_id == snapshot.current_id else {
            clearPublication()
            return
        }

        let metadata = Metadata(
            entryID: item.entry_id,
            title: item.title,
            artist: distinctStation(title: item.title, station: item.station) ?? item.artist,
            album: item.album,
            duration: availability.duration
        )
        let elapsed = snapshot.sample_rate > 0
            ? Double(snapshot.position_frames) / Double(snapshot.sample_rate) : 0
        let position = metadata.duration.map { min(elapsed, $0) } ?? elapsed
        let rate = snapshot.transport == "playing" ? 1.0 : 0.0
        let playbackState: MPNowPlayingPlaybackState
        switch snapshot.transport {
        case "playing", "buffering": playbackState = .playing
        case "paused": playbackState = .paused
        case "stopped", "ended", "error": playbackState = .stopped
        default: playbackState = .unknown
        }

        let now = ProcessInfo.processInfo.systemUptime
        if let publication, publication.metadata == metadata,
           publication.rate == rate, publication.playbackState == playbackState {
            // MediaPlayer extrapolates while playing; tolerate minor clock drift rather
            // than republishing each 250ms snapshot. At rest, publish every position
            // change so even a subsecond paused seek reaches the system surface.
            if rate == 0 {
                if position == publication.position { return }
            } else {
                let extrapolated = publication.position + (now - publication.uptime) * rate
                if abs(position - extrapolated) < 0.5 { return }
            }
        }
        var info: [String: Any] = [
            MPMediaItemPropertyTitle: metadata.title,
            MPMediaItemPropertyArtist: metadata.artist,
            MPMediaItemPropertyAlbumTitle: metadata.album,
            MPNowPlayingInfoPropertyElapsedPlaybackTime: position,
            MPNowPlayingInfoPropertyPlaybackRate: rate,
            MPNowPlayingInfoPropertyDefaultPlaybackRate: 1.0,
            MPNowPlayingInfoPropertyMediaType: MPNowPlayingInfoMediaType.audio.rawValue,
            MPNowPlayingInfoPropertyIsLiveStream: false,
        ]
        if let duration = metadata.duration { info[MPMediaItemPropertyPlaybackDuration] = duration }
        information.nowPlayingInfo = info
        // macOS requires explicit state: there is deliberately no local audio session/player.
        information.playbackState = playbackState
        publication = Publication(metadata: metadata, position: position, rate: rate,
                                  playbackState: playbackState, uptime: now)
    }

    func shutdown() {
        guard !isShutdown else { return }
        isShutdown = true
        gate.update(MediaCommandAvailability())
        for registration in registrations { registration.command.removeTarget(registration.target) }
        registrations.removeAll()
        // A late teardown must never disable or clear a replacement bridge's publication.
        guard Self.ownerID == id else { return }
        setEnabled(MediaCommandAvailability())
        clearPublication()
        Self.owner = nil
        Self.ownerID = nil
    }

    isolated deinit { shutdown() }

    private func register(_ command: MPRemoteCommand, action: Action) {
        let gate = gate
        // The SDK callback is neither @MainActor nor @Sendable. An explicitly Sendable
        // closure prevents inheriting MainActor isolation at the registration site.
        let callback: @Sendable (MPRemoteCommandEvent) -> MPRemoteCommandHandlerStatus = { [weak self, gate] _ in
            guard let self, let receipt = gate.receive(action) else { return .commandFailed }
            Task { @MainActor [weak self] in self?.dispatch(receipt) }
            // Accepted for dispatch only; the daemon's result/error is reported by PlayerModel.
            return .success
        }
        registrations.append((command, command.addTarget(handler: callback)))
    }

    private func registerSeek() {
        let gate = gate
        let callback: @Sendable (MPRemoteCommandEvent) -> MPRemoteCommandHandlerStatus = { [weak self, gate] event in
            guard let self, let event = event as? MPChangePlaybackPositionCommandEvent,
                  let receipt = gate.receiveSeek(seconds: event.positionTime) else { return .commandFailed }
            // Only Sendable values cross executors. Capture the occurrence at receipt,
            // not after the main actor becomes available or the current item changes.
            Task { @MainActor [weak self] in self?.dispatch(receipt) }
            return .success
        }
        let command = commands.changePlaybackPositionCommand
        registrations.append((command, command.addTarget(handler: callback)))
    }

    /// Shuffle and repeat go to the server's play-order modes, as the player bar toggles do.
    private func registerModes() {
        let gate = gate
        let repeatCallback: @Sendable (MPRemoteCommandEvent) -> MPRemoteCommandHandlerStatus = { [weak self, gate] event in
            guard let self, let event = event as? MPChangeRepeatModeCommandEvent else { return .commandFailed }
            let mode: RepeatMode
            switch event.repeatType {
            case .off: mode = .off
            case .one: mode = .one
            case .all: mode = .all
            @unknown default: return .commandFailed
            }
            guard let receipt = gate.receive(.setRepeat(mode)) else { return .commandFailed }
            Task { @MainActor [weak self] in self?.dispatch(receipt) }
            return .success
        }
        let shuffleCallback: @Sendable (MPRemoteCommandEvent) -> MPRemoteCommandHandlerStatus = { [weak self, gate] event in
            guard let self, let event = event as? MPChangeShuffleModeCommandEvent,
                  let receipt = gate.receive(.setShuffle(event.shuffleType != .off)) else { return .commandFailed }
            Task { @MainActor [weak self] in self?.dispatch(receipt) }
            return .success
        }
        let repeatCommand = commands.changeRepeatModeCommand
        registrations.append((repeatCommand, repeatCommand.addTarget(handler: repeatCallback)))
        let shuffleCommand = commands.changeShuffleModeCommand
        registrations.append((shuffleCommand, shuffleCommand.addTarget(handler: shuffleCallback)))
    }

    private func dispatch(_ receipt: MediaCommandReceipt) {
        guard !isShutdown, Self.ownerID == id, gate.isCurrent(receipt) else { return }
        handle(receipt.action)
    }

    private func setEnabled(_ availability: MediaCommandAvailability) {
        commands.playCommand.isEnabled = availability.allows(.play)
        commands.pauseCommand.isEnabled = availability.allows(.pause)
        commands.togglePlayPauseCommand.isEnabled = availability.allows(.togglePlayPause)
        commands.nextTrackCommand.isEnabled = availability.allows(.next)
        commands.previousTrackCommand.isEnabled = availability.allows(.previous)
        commands.stopCommand.isEnabled = availability.allows(.stop)
        commands.changePlaybackPositionCommand.isEnabled = availability.canMutate && availability.seekLimit != nil
        commands.changeRepeatModeCommand.isEnabled = availability.allows(.setRepeat(.off))
        commands.changeShuffleModeCommand.isEnabled = availability.allows(.setShuffle(false))
    }

    private func disableUnsupportedCommands() {
        commands.seekForwardCommand.isEnabled = false
        commands.seekBackwardCommand.isEnabled = false
        commands.skipForwardCommand.isEnabled = false
        commands.skipBackwardCommand.isEnabled = false
        commands.changePlaybackRateCommand.isEnabled = false
        commands.enableLanguageOptionCommand.isEnabled = false
        commands.disableLanguageOptionCommand.isEnabled = false
        commands.ratingCommand.isEnabled = false
        commands.likeCommand.isEnabled = false
        commands.dislikeCommand.isEnabled = false
        commands.bookmarkCommand.isEnabled = false
    }

    private func clearPublication() {
        // Merely creating a disconnected window must not claim Now Playing ownership.
        guard publication != nil else { return }
        information.playbackState = .stopped
        information.nowPlayingInfo = nil
        publication = nil
    }

    private struct Metadata: Equatable {
        let entryID: UInt64
        let title: String
        let artist: String
        let album: String
        let duration: Double?
    }

    private struct Publication {
        let metadata: Metadata
        let position: Double
        let rate: Double
        let playbackState: MPNowPlayingPlaybackState
        let uptime: TimeInterval
    }
}

private struct MediaCommandAvailability: Sendable {
    var connected = false
    var canMutate = false
    var hasContent = false
    var entryID: UInt64?
    var canNext = false
    var canPrevious = false
    var duration: Double?
    var seekLimit: Double?

    init(snapshot: ControlState? = nil, canMutate: Bool = false) {
        guard let snapshot else { return }
        connected = true
        self.canMutate = canMutate
        hasContent = snapshot.current_id != nil || snapshot.queue_length > 0
        entryID = snapshot.current_id
        canNext = snapshot.can_next
        canPrevious = snapshot.can_previous
        if let item = snapshot.now_playing, item.entry_id == entryID,
           let frames = item.duration_frames, frames > 0, snapshot.sample_rate > 0 {
            let sampleRate = Double(snapshot.sample_rate)
            duration = Double(frames) / sampleRate
            // The daemon accepts only positions strictly before the end. The model
            // also clamps the final frame conversion, including Double rounding.
            seekLimit = Double(frames - 1) / sampleRate
        }
    }

    func allows(_ action: SystemMediaControls.Action) -> Bool {
        guard connected, canMutate else { return false }
        switch action {
        case .play, .togglePlayPause: return hasContent
        case .pause, .stop: return entryID != nil
        case .next: return entryID != nil && canNext
        case .previous: return entryID != nil && canPrevious
        case .seek(let requestedID, let seconds):
            return requestedID == entryID && seekLimit != nil && seconds.isFinite && seconds >= 0
        case .setRepeat, .setShuffle: return true
        }
    }
}

private struct MediaCommandReceipt: Sendable {
    let generation: UInt64
    let action: SystemMediaControls.Action
}

// All mutable state is protected by lock. No MediaPlayer objects or actor-isolated
// closures live here; callbacks never synchronously wait for the main actor.
private final class MediaCommandGate: @unchecked Sendable {
    private let lock = NSLock()
    private var availability = MediaCommandAvailability()
    private var generation: UInt64 = 0

    func update(_ value: MediaCommandAvailability) {
        lock.lock()
        defer { lock.unlock() }
        if availability.connected && !value.connected { generation &+= 1 }
        availability = value
    }

    func receive(_ action: SystemMediaControls.Action) -> MediaCommandReceipt? {
        lock.lock()
        defer { lock.unlock() }
        guard availability.allows(action) else { return nil }
        return MediaCommandReceipt(generation: generation, action: action)
    }

    func receiveSeek(seconds: Double) -> MediaCommandReceipt? {
        lock.lock()
        defer { lock.unlock() }
        guard seconds.isFinite, seconds >= 0, let entryID = availability.entryID,
              let limit = availability.seekLimit else { return nil }
        let action = SystemMediaControls.Action.seek(entryID: entryID, seconds: min(seconds, limit))
        guard availability.allows(action) else { return nil }
        return MediaCommandReceipt(generation: generation, action: action)
    }

    func isCurrent(_ receipt: MediaCommandReceipt) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return receipt.generation == generation && availability.allows(receipt.action)
    }
}
