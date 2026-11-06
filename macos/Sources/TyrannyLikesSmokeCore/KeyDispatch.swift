import Foundation

/// Decides whether a key press may run its bound command.
///
/// A key monitor runs before menu key equivalents and before the focused text field, so it asks
/// this first. A field that is being edited keeps the keys it types and its ⌘ cursor and delete keys.
public enum KeyDispatch {
    private static let editingKeys: Set<String> = ["left", "right", "up", "down", "delete", "forwarddelete"]

    /// Whether `chord` may run a bound command.
    public static func shouldRun(
        _ chord: KeyChord, isMainWindow: Bool, hasSheet: Bool, overlayOpen: Bool, isEditingText: Bool
    ) -> Bool {
        guard isMainWindow, !hasSheet, !overlayOpen else { return false }
        guard isEditingText else { return true }
        return chord.modifiers.contains(.command) && !fieldKeepsKey(chord)
    }

    /// Whether a text field being edited keeps `chord`: a ⌘ chord on a cursor or delete key.
    public static func fieldKeepsKey(_ chord: KeyChord) -> Bool {
        chord.modifiers.contains(.command) && editingKeys.contains(chord.key)
    }
}

/// Decides what an armed key recorder does with a key press.
public enum KeyRecording {
    public enum Action: Equatable {
        /// Take the key as the chord.
        case record
        /// Stop recording and swallow Escape.
        case cancel
        /// Stop recording and pass the key on to its target.
        case abandon
    }

    /// A key in another window, or in text being edited, is not meant for the recorder.
    public static func action(isRecorderWindow: Bool, isEditingText: Bool, isEscape: Bool) -> Action {
        if !isRecorderWindow || isEditingText { return .abandon }
        return isEscape ? .cancel : .record
    }
}

/// Runs key commands one at a time, in order. A failure goes to `report`.
@MainActor public final class KeyCommandQueue {
    private let run: @MainActor (String) async throws -> Void
    private let report: @MainActor (String) -> Void
    private var pending: [String] = []
    private var isRunning = false

    public init(run: @escaping @MainActor (String) async throws -> Void, report: @escaping @MainActor (String) -> Void) {
        self.run = run
        self.report = report
    }

    private static let repeatingSearches: Set<String> = [CommandID.searchNext.rawValue, CommandID.searchPrevious.rawValue]

    public func enqueue(_ command: String) {
        // A held `n` would queue a search for every key repeat. One running and one waiting is enough.
        if Self.repeatingSearches.contains(CommandHistory.normalised(command)), let last = pending.last,
           CommandHistory.normalised(last) == CommandHistory.normalised(command) {
            return
        }
        pending.append(command)
        guard !isRunning else { return }
        isRunning = true
        Task { await drain() }
    }

    private func drain() async {
        while !pending.isEmpty {
            let command = pending.removeFirst()
            do {
                try await run(command)
            } catch is CancellationError {
                // The user cancelled, for example a folder chooser. Nothing to report.
            } catch {
                report(error.localizedDescription)
            }
        }
        isRunning = false
    }
}
