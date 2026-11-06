import Foundation

/// The request and messages of vi-style search (`/`, `n`, `N`). The server finds the match.
public enum ListSearch {
    /// The server refused the term (`invalid_filter` or `invalid`). It is not kept as the last search.
    public struct InvalidTerm: LocalizedError {
        public let message: String
        public init(message: String) { self.message = message }
        public var errorDescription: String? { message }
    }

    /// The fields of a `find_in_list` request: everything but `command` and `id`.
    public static func fields(list: ListDescription, revision: UInt64?, term: String, from: Int?,
                              backward: Bool) throws -> [String: JSONValue] {
        ["list": try JSONValue.encoding(list), "revision": revision.map(JSONValue.uint) ?? .null,
         "term": .string(term), "from": from.map(JSONValue.int) ?? .null, "backward": .bool(backward)]
    }

    /// Forward starts after the last selected row, backward before the first; nil with no selection.
    public static func from(selection: IndexSet, backward: Bool) -> Int? {
        backward ? selection.first : selection.last
    }

    public static func wrappedMessage(backward: Bool) -> String {
        backward ? "Search wrapped to the bottom." : "Search wrapped to the top."
    }

    /// The prompt's hint. It names the chords bound to `search-next` and `search-previous`.
    public static func hint(next: KeyChord?, previous: KeyChord?) -> String {
        let base = "Jump to a match. Uses filter syntax."
        switch (next, previous) {
        case let (next?, previous?): return "\(base) \(next.display) and \(previous.display) repeat."
        case let (chord?, nil), let (nil, chord?): return "\(base) \(chord.display) repeats."
        case (nil, nil): return base
        }
    }

    public static func noMatch(_ term: String) -> String {
        "No match for “\(term)”."
    }
}
