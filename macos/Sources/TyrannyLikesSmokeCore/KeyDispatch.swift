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
        return chord.modifiers.contains(.command) && !editingKeys.contains(chord.key)
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

    public func enqueue(_ command: String) {
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
            } catch {
                report(error.localizedDescription)
            }
        }
        isRunning = false
    }
}
