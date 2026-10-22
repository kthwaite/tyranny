import Foundation
import Observation

/// The state of the `:` command palette. The view renders it and forwards keys.
///
/// Each `open` starts a session, and `close` ends it. A command keeps running after its session
/// ends. When it ends, success is still recorded in history, and failure goes to `report`
/// instead of the palette. A stale run never touches the newer session's state.
@MainActor @Observable
public final class CommandPaletteModel {
    public enum Mode: Equatable, Sendable {
        /// Typing a command. The dropdown shows completions.
        case commands
        /// Picking a previous command. The dropdown shows history entries.
        case history
    }

    /// One dropdown row. Tab puts `line` in the input; the view shows `value` and `summary`.
    public struct Row: Equatable, Sendable {
        public let line: String
        public let value: String
        public let summary: String?
        /// True when the command takes another argument after this one.
        public let continues: Bool
    }

    public enum Dropdown: Equatable, Sendable {
        case rows([Row])
        /// The command's usage line, shown when its argument has no rows.
        case hint(usage: String, summary: String)
        case empty
    }

    public private(set) var isOpen = false
    public private(set) var mode: Mode = .commands
    /// The input line. Setting it clears the error and refreshes the dropdown.
    public var text = "" {
        didSet { refresh() }
    }
    public private(set) var dropdown: Dropdown = .empty
    /// The highlighted row. Nil when the dropdown has no rows.
    public private(set) var highlighted: Int?
    /// The last failure in this session, shown under the input line.
    public private(set) var error: String?
    /// True while a command from this session is in flight.
    public private(set) var isRunning = false
    public private(set) var history: CommandHistory

    /// Completion rows for the input line.
    @ObservationIgnored public var suggest: @MainActor (String) -> [CommandSuggestion] = { CommandLanguage.suggestions($0) }
    /// Runs the input line. A thrown error's description is shown.
    @ObservationIgnored public var submit: @MainActor (String) async throws -> Void = { _ in
        throw CommandLanguageError("Open the library window before submitting commands.")
    }
    /// Shows a failure from a run whose session has ended.
    @ObservationIgnored public var report: @MainActor (String) -> Void = { _ in }

    @ObservationIgnored private let defaults: UserDefaults
    /// Bumps on `open` and `close`. A run that sees a different value belongs to an ended session.
    @ObservationIgnored private var session: UInt64 = 0

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        history = CommandHistory.load(from: defaults)
    }

    /// Starts a fresh session. The input is `:` for commands and empty for history.
    public func open(_ mode: Mode = .commands) {
        session &+= 1
        self.mode = mode
        text = mode == .commands ? ":" : ""
        isOpen = true
        isRunning = false
        error = nil
    }

    /// Hides the palette and ends the session. A command in flight keeps running.
    public func close() {
        session &+= 1
        isOpen = false
        isRunning = false
    }

    /// Moves the highlight. It stops at the first and last rows.
    public func moveHighlight(_ delta: Int) {
        guard let highlighted, case .rows(let rows) = dropdown else { return }
        self.highlighted = min(max(highlighted + delta, 0), rows.count - 1)
    }

    /// A row click. Highlights the row at `index`, clamped to the rows.
    public func highlight(_ index: Int) {
        guard case .rows(let rows) = dropdown else { return }
        highlighted = min(max(index, 0), rows.count - 1)
    }

    /// Tab. Puts the highlighted row in the input line. In history mode, also returns to commands.
    public func acceptHighlighted() {
        guard !isRunning, let row = highlightedRow else { return }
        switch mode {
        case .commands:
            text = row.line + (row.continues ? " " : "")
        case .history:
            mode = .commands
            text = row.line
        }
    }

    /// Return. Runs the input line as typed. `:history` opens the history picker instead.
    /// In history mode, picks the highlighted entry, unrun.
    public func submitText() async {
        guard !isRunning else { return }
        switch mode {
        case .history:
            acceptHighlighted()
        case .commands where CommandHistory.isHistoryCommand(text):
            open(.history)
        case .commands:
            await run(text)
        }
    }

    private func run(_ text: String) async {
        let started = session
        isRunning = true
        do {
            try await submit(text)
            history.record(text)
            history.save(to: defaults)
            if started == session { close() }
        } catch {
            let message = error.localizedDescription
            if started == session {
                isRunning = false
                self.error = message
            } else {
                report(message)
            }
        }
    }

    private var highlightedRow: Row? {
        guard let highlighted, case .rows(let rows) = dropdown else { return nil }
        return rows[highlighted]
    }

    private func refresh() {
        error = nil
        let rows: [Row]
        switch mode {
        case .commands:
            rows = suggest(text).map { Row(line: $0.line, value: $0.value, summary: $0.summary, continues: $0.continues) }
            if rows.isEmpty, let hint = CommandLanguage.usageHint(text) {
                dropdown = .hint(usage: hint.usage, summary: hint.summary)
                highlighted = nil
                return
            }
        case .history:
            let prefix = CommandHistory.normalised(text)
            rows = history.entries.filter { $0.hasPrefix(prefix) }.map { Row(line: ":" + $0, value: $0, summary: nil, continues: false) }
        }
        dropdown = rows.isEmpty ? .empty : .rows(rows)
        highlighted = rows.isEmpty ? nil : 0
    }
}
