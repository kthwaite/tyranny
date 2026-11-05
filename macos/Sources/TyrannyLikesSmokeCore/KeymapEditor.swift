import Foundation
import Observation

/// The state of the add and edit sheet in the Keys pane.
@MainActor @Observable
public final class KeymapEditor {
    public enum Verdict: Equatable {
        case incomplete
        case invalidCommand(String)
        case reserved(String)
        /// The chord already runs this command. Saving replaces it.
        case conflict(command: String)
        case ready
    }

    public var chord: KeyChord?
    public var command: String

    private let store: KeymapStore
    private let original: KeyBinding?

    public init(store: KeymapStore, editing: KeyBinding? = nil) {
        self.store = store
        original = editing
        chord = editing?.chord
        command = editing?.command ?? ""
    }

    public var verdict: Verdict {
        guard let chord else { return .incomplete }
        if let reason = Keymap.reservedReason(chord) { return .reserved(reason) }
        let text = CommandHistory.normalised(command)
        guard !text.isEmpty else { return .incomplete }
        do { _ = try CommandLanguage.parse(text) } catch { return .invalidCommand(error.localizedDescription) }
        if chord != original?.chord, let existing = store.command(for: chord) { return .conflict(command: existing) }
        return .ready
    }

    public var canSave: Bool {
        switch verdict {
        case .ready, .conflict: true
        case .incomplete, .invalidCommand, .reserved: false
        }
    }

    public var saveTitle: String {
        if case .conflict = verdict { "Replace" } else { "Save" }
    }

    /// Removes the edited row's old chord when the chord changed, then binds.
    public func save() {
        guard canSave, let chord else { return }
        if let original, original.chord != chord { store.remove(original.chord) }
        store.bind(chord, to: command)
    }
}
