import Foundation
import Observation
import TyrannyCore

/// Edits are optimistic only while a batch is in flight. The server owns persisted settings.
@MainActor @Observable
final class EqualizerModel {
    @ObservationIgnored private weak var player: PlayerModel?
    @ObservationIgnored private var queued: EqualizerSettings?
    @ObservationIgnored private var editTask: Task<Void, Never>?
    @ObservationIgnored private var generation: UInt64 = 0
    private(set) var pending: EqualizerSettings?
    private(set) var error: String?

    func attach(_ player: PlayerModel) { self.player = player }

    var canEdit: Bool { player?.connected == true && player?.snapshot != nil }
    var settings: EqualizerSettings { pending ?? player?.snapshot?.settings.equalizer ?? EqualizerSettings() }

    func stateChanged() {
        guard !canEdit, pending != nil || editTask != nil else { return }
        // A cancelled request may still complete. Its generation must not clear a new edit,
        // refresh a new connection, or replay queued values after reconnecting.
        generation &+= 1
        editTask?.cancel()
        editTask = nil
        queued = nil
        pending = nil
    }

    func setEnabled(_ enabled: Bool) {
        var value = settings
        value.enabled = enabled
        set(value)
    }

    func setPreamp(_ gain: Double) {
        var value = settings
        value.preamp_db = gain
        set(value)
    }

    func setBand(_ index: Int, gain: Double) {
        var value = settings
        guard value.bands_db.indices.contains(index) else { return }
        value.bands_db[index] = gain
        set(value)
    }

    func selectPreset(_ preset: EqualizerPreset) {
        set(preset.settings(enabled: settings.enabled))
    }

    /// One full configuration in flight; later slider edits replace the queued configuration.
    /// This deliberately does not take PlayerModel's busy lock.
    func set(_ value: EqualizerSettings) {
        guard canEdit, value.isValid else { return }
        error = nil
        pending = value
        queued = value
        guard editTask == nil else { return }
        let epoch = generation
        editTask = Task { [weak self] in
            guard let self, let player = self.player else { return }
            do {
                while epoch == generation, canEdit, !Task.isCancelled {
                    if let value = queued {
                        queued = nil
                        let _: Applied = try await player.send("set_equalizer", ["equalizer": try JSONValue.encoding(value)])
                        guard epoch == generation, !Task.isCancelled else { return }
                        if queued != nil { continue }
                    }
                    // An Applied reply can precede its snapshot event. Fetch the authoritative
                    // configuration before dropping optimistic state, including competing clients.
                    try await player.refreshSnapshot()
                    guard epoch == generation, !Task.isCancelled else { return }
                    if queued == nil { break }
                }
            } catch {
                guard epoch == generation, !Task.isCancelled else { return }
                self.error = error.localizedDescription
                player.message = error.localizedDescription
                queued = nil
                pending = nil
                // Rejection never leaves an optimistic curve behind. On a lost connection the
                // next hello supplies state instead; no failed mutation is retried.
                if canEdit { try? await player.refreshSnapshot() }
                guard epoch == generation, !Task.isCancelled else { return }
                // Any edits during failure recovery belong to a new batch, not the rejected one.
                let next = queued
                queued = nil
                pending = nil
                editTask = nil
                if let next { set(next) }
                return
            }
            guard epoch == generation else { return }
            pending = nil
            editTask = nil
        }
    }
}
