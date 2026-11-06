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

    public static func noMatch(_ term: String) -> String {
        "No match for “\(term)”."
    }
}
