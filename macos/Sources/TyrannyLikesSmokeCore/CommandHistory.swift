import Foundation

/// The commands the user ran before, newest first.
/// Entries have no leading `:`. The `history` command is never kept.
public struct CommandHistory: Equatable, Sendable {
    public static let limit = 100
    public static let defaultsKey = "commandHistory"

    public private(set) var entries: [String]

    public init(entries: [String] = []) {
        self.entries = entries
    }

    /// Adds a command at the front. A repeat moves to the front. The oldest entries fall off at the limit.
    public mutating func record(_ text: String) {
        let entry = Self.normalised(text)
        guard !entry.isEmpty, !Self.isHistoryCommand(entry) else { return }
        entries.removeAll { $0 == entry }
        entries.insert(entry, at: 0)
        if entries.count > Self.limit { entries.removeLast(entries.count - Self.limit) }
    }

    /// Trims whitespace, drops one leading `:`, and trims again.
    public static func normalised(_ text: String) -> String {
        var result = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if result.hasPrefix(":") { result.removeFirst() }
        return result.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// True when the text parses to the `history` command.
    public static func isHistoryCommand(_ text: String) -> Bool {
        (try? CommandLanguage.parse(text))?.id == .history
    }

    /// Reads the saved entries. Empty strings are dropped and the limit is applied.
    public static func load(from defaults: UserDefaults) -> CommandHistory {
        let stored = defaults.stringArray(forKey: defaultsKey) ?? []
        return CommandHistory(entries: Array(stored.filter { !$0.isEmpty }.prefix(limit)))
    }

    public func save(to defaults: UserDefaults) {
        defaults.set(entries, forKey: Self.defaultsKey)
    }
}
