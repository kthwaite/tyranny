import Foundation

public enum CommandID: String, CaseIterable, Sendable {
    case play, pause, toggle, stop, next, previous, seek, volume
    case repeatMode = "repeat"
    case shuffle, gain, resume, go, filter
    case focusFilter = "focus-filter"
    case revealPlaying = "reveal-playing"
    case browser, drawer, artwork, settings, connect, disconnect, server
    case chooseFolder = "choose-folder"
    case rescan
    case retryImport = "retry-import"
    case browseLibrary = "browse-library"
    case quit
    case playlistOpen = "playlist-open"
    case playlistCreate = "playlist-create"
    case playlistRename = "playlist-rename"
    case playlistDelete = "playlist-delete"
    case playlistPlay = "playlist-play"
    case playlistEnqueue = "playlist-enqueue"
    case playlistAdd = "playlist-add"
    case smartCreate = "smart-create"
    case smartUpdate = "smart-update"
    case smartSnapshot = "smart-snapshot"
    case queueClear = "queue-clear"
    case queueShuffle = "queue-shuffle"
    case queueSave = "queue-save"
    case selectionPlay = "selection-play"
    case selectionRun = "selection-run"
    case selectionNext = "selection-next"
    case selectionEnqueue = "selection-enqueue"
    case selectionRemove = "selection-remove"
    case selectionAlbum = "selection-album"
    case selectionArtist = "selection-artist"
    case selectionAll = "selection-all"
    case selectionClear = "selection-clear"
    case selectionMove = "selection-move"
    case group, sort, column
    case albumSort = "album-sort"
    case coverSize = "cover-size"
    case albumExpand = "album-expand"
    case albumCollapse = "album-collapse"
    case browserGenre = "browser-genre"
    case browserArtist = "browser-artist"
    case browserAlbum = "browser-album"
    case upNextPolicy = "up-next-policy"
    case prompt, help
    case colour, colours, tint
}

public struct ParsedCommand: Equatable, Sendable {
    public let id: CommandID
    public let arguments: [String]
}

public struct CommandLanguageError: Error, LocalizedError, Equatable, Sendable {
    public let message: String
    public init(_ message: String) { self.message = message }
    public var errorDescription: String? { message }
}

public enum CommandArgumentKind: Equatable, Sendable {
    case text(allowEmpty: Bool)
    case playlist
    case choices([String])
    case unsigned(or: String?)
    case seek
    case volume
    case coverSize
    case command
    case colour
}

public struct CommandArgument: Equatable, Sendable {
    public let name: String
    public let kind: CommandArgumentKind
    public let optional: Bool

    init(_ name: String, _ kind: CommandArgumentKind, optional: Bool = false) {
        self.name = name
        self.kind = kind
        self.optional = optional
    }
}

public struct CommandDefinition: Equatable, Sendable {
    public let id: CommandID
    public let usage: String
    public let summary: String
    public let arguments: [CommandArgument]

    init(_ id: CommandID, _ summary: String, _ arguments: [CommandArgument] = []) {
        self.id = id
        self.summary = summary
        self.arguments = arguments
        usage = ([id.rawValue] + arguments.map { $0.optional ? "[\($0.name)]" : "<\($0.name)>" }).joined(separator: " ")
    }
}

/// A client-only language. Tokens are literal strings, never shell expressions or wire commands.
public enum CommandLanguage {
    private static let toggleValues = ["on", "off", "toggle"]
    private static let name = CommandArgument("name", .text(allowEmpty: false))
    private static let playlist = CommandArgument("playlist", .playlist)
    private static let rule = CommandArgument("rule", .text(allowEmpty: false))

    public static let registry: [CommandDefinition] = [
        .init(.play, "Start or resume playback."),
        .init(.pause, "Pause playback."),
        .init(.toggle, "Toggle playback."),
        .init(.stop, "Stop playback."),
        .init(.next, "Play the next entry."),
        .init(.previous, "Play the previous entry."),
        .init(.seek, "Seek absolutely, or relatively with a sign; default unit is seconds.", [.init("[+-]number[s|m|h|%]", .seek)]),
        .init(.volume, "Set volume, or adjust it with a sign; values are percentages.", [.init("[+-]number[%]", .volume)]),
        .init(.repeatMode, "Set or cycle repeat mode.", [.init("off|all|one|cycle", .choices(["off", "all", "one", "cycle"]))]),
        .init(.shuffle, "Set or toggle shuffle.", [.init("on|off|toggle", .choices(toggleValues))]),
        .init(.gain, "Set ReplayGain mode.", [.init("off|track|album", .choices(["off", "track", "album"]))]),
        .init(.resume, "Set or toggle resume on launch.", [.init("on|off|toggle", .choices(toggleValues))]),
        .init(.go, "Open a library destination.", [.init("destination", .choices(["songs", "albums", "artists", "genres", "recent", "history", "up-next", "radio"]))]),
        .init(.filter, "Set the current filter; an empty quoted string clears it.", [.init("text", .text(allowEmpty: true))]),
        .init(.focusFilter, "Focus the filter field."),
        .init(.revealPlaying, "Reveal the current entry."),
        .init(.browser, "Show, hide or toggle the browser.", [.init("on|off|toggle", .choices(toggleValues))]),
        .init(.drawer, "Show, hide or toggle Up Next.", [.init("on|off|toggle", .choices(toggleValues))]),
        .init(.artwork, "Expand, collapse or toggle Now Playing artwork.", [.init("on|off|toggle", .choices(toggleValues))]),
        .init(.settings, "Open a settings pane.", [.init("pane", .choices(["connection", "library", "playback", "appearance"]))]),
        .init(.connect, "Connect to the server."),
        .init(.disconnect, "Disconnect from the server."),
        .init(.server, "Control the local server.", [.init("start|restart|stop", .choices(["start", "restart", "stop"]))]),
        .init(.chooseFolder, "Choose a library folder."),
        .init(.rescan, "Rescan a library root.", [.init("root-id", .unsigned(or: nil))]),
        .init(.retryImport, "Retry the failed import."),
        .init(.browseLibrary, "Browse the imported library."),
        .init(.quit, "Quit the application."),
        .init(.playlistOpen, "Open a saved playlist.", [playlist]),
        .init(.playlistCreate, "Create a playlist.", [name]),
        .init(.playlistRename, "Rename a saved playlist.", [playlist, name]),
        .init(.playlistDelete, "Delete a saved playlist.", [playlist]),
        .init(.playlistPlay, "Play a saved playlist.", [playlist]),
        .init(.playlistEnqueue, "Append a saved playlist to Up Next.", [playlist]),
        .init(.playlistAdd, "Add selected tracks to a playlist.", [playlist]),
        .init(.smartCreate, "Create a smart playlist using a rule.", [name, rule]),
        .init(.smartUpdate, "Update a smart playlist rule.", [playlist, rule]),
        .init(.smartSnapshot, "Save a smart playlist as a static playlist.", [playlist, name]),
        .init(.queueClear, "Clear Up Next."),
        .init(.queueShuffle, "Shuffle upcoming entries."),
        .init(.queueSave, "Save Up Next as a playlist.", [name]),
        .init(.selectionPlay, "Play selected tracks."),
        .init(.selectionRun, "Play from the selected row in its list."),
        .init(.selectionNext, "Place selected tracks next."),
        .init(.selectionEnqueue, "Append selected tracks to Up Next."),
        .init(.selectionRemove, "Remove selected entries."),
        .init(.selectionAlbum, "Show the selected track's album."),
        .init(.selectionArtist, "Show the selected track's artist."),
        .init(.selectionAll, "Select all rows."),
        .init(.selectionClear, "Clear the selection."),
        .init(.selectionMove, "Move selected entries before an entry, or to the end.", [.init("entry-id|end", .unsigned(or: "end"))]),
        .init(.group, "Set list grouping.", [.init("none|album|artist_album", .choices(["none", "album", "artist_album"]))]),
        .init(.sort, "Sort the list; direction defaults to ascending; none clears sorting.", [.init("field", .choices(["none", "title", "artist", "album_artist", "album", "duration", "year", "genre", "format", "plays", "date_added", "last_played", "album_order", "playlist_order"])), .init("asc|desc", .choices(["asc", "desc"]), optional: true)]),
        .init(.column, "Show, hide or toggle a column.", [.init("column-id", .choices(["number", "artist", "album", "time", "year", "genre", "format", "plays", "album-artist", "disc", "date-added", "last-played", "bitrate", "sample-rate"])), .init("on|off|toggle", .choices(toggleValues))]),
        .init(.albumSort, "Sort the album grid.", [.init("album_artist|year|date_added", .choices(["album_artist", "year", "date_added"]))]),
        .init(.coverSize, "Set the album grid cover size.", [.init("96...200", .coverSize)]),
        .init(.albumExpand, "Expand the selected album."),
        .init(.albumCollapse, "Collapse the expanded album."),
        .init(.browserGenre, "Select a genre in the browser.", [.init("genre-id|all", .unsigned(or: "all"))]),
        .init(.browserArtist, "Select an artist in the browser.", [.init("artist-id|all", .unsigned(or: "all"))]),
        .init(.browserAlbum, "Select an album in the browser.", [.init("album-id|all", .unsigned(or: "all"))]),
        .init(.upNextPolicy, "Choose how new playback treats upcoming entries.", [.init("ask|keep|replace", .choices(["ask", "keep", "replace"]))]),
        .init(.prompt, "Resolve the pending Up Next prompt.", [.init("keep|clear|cancel", .choices(["keep", "clear", "cancel"]))]),
        .init(.help, "Show command help.", [.init("command", .command, optional: true)]),
        .init(.colour, "Set a colour token, or return it to System.", [.init("token", .choices(ColourToken.allCases.map(\.rawValue))), .init("#rrggbb|system", .colour)]),
        .init(.colours, "Return every colour to System and turn the cover tint on.", [.init("reset", .choices(["reset"]))]),
        .init(.tint, "Turn the accent tint from cover art on or off.", [.init("on|off", .choices(["on", "off"]))]),
    ]

    public static func parse(_ text: String) throws -> ParsedCommand {
        let tokens = try tokenize(body(text), incomplete: false).tokens
        guard let first = tokens.first, let definition = registry.first(where: { $0.id.rawValue == first }) else {
            throw CommandLanguageError(tokens.isEmpty ? "Enter a command." : "Unknown command: \(tokens[0]).")
        }
        let arguments = Array(tokens.dropFirst())
        let required = definition.arguments.filter { !$0.optional }.count
        guard arguments.count >= required, arguments.count <= definition.arguments.count else {
            throw CommandLanguageError("Usage: \(definition.usage)")
        }
        for (value, schema) in zip(arguments, definition.arguments) {
            try validate(value, as: schema)
        }
        if definition.id == .sort && arguments.first == "none" && arguments.count > 1 {
            throw CommandLanguageError("sort none does not take a direction.")
        }
        return ParsedCommand(id: definition.id, arguments: arguments)
    }

    /// Returns complete replacement lines, suitable for parsing after accepting a suggestion.
    public static func completions(_ text: String, playlistNames: [String] = []) -> [String] {
        guard let scan = try? tokenize(body(text), incomplete: true) else { return [] }
        let colon = text.drop(while: { $0.isWhitespace }).hasPrefix(":") ? ":" : ""
        var tokens = scan.tokens
        if !scan.active { tokens.append("") }
        if tokens.count == 1 {
            return registry.map { $0.id.rawValue }.filter { $0.hasPrefix(tokens[0]) }.map { colon + $0 }
        }
        guard let definition = registry.first(where: { $0.id.rawValue == tokens[0] }) else { return [] }
        let index = tokens.count - 2
        guard index < definition.arguments.count else { return [] }
        if definition.id == .sort && index > 0 && tokens[1] == "none" { return [] }
        for argumentIndex in 0..<index {
            guard (try? validate(tokens[argumentIndex + 1], as: definition.arguments[argumentIndex])) != nil else { return [] }
        }
        let candidates: [String]
        switch definition.arguments[index].kind {
        case .choices(let values): candidates = values
        case .playlist: candidates = playlistNames.filter { !$0.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
        case .unsigned(let alternative): candidates = alternative.map { [$0] } ?? []
        case .command: candidates = registry.map { $0.id.rawValue }
        case .colour: candidates = ["system"]
        default: return []
        }
        let prefix = tokens.removeLast()
        let stem = colon + tokens.map(quote).joined(separator: " ") + " "
        return Set(candidates).sorted().filter { $0.hasPrefix(prefix) }.map { stem + quote($0) }
    }

    private static func body(_ text: String) -> String {
        let trimmed = text.drop(while: { $0.isWhitespace })
        return String(trimmed.hasPrefix(":") ? trimmed.dropFirst() : trimmed)
    }

    private static func quote(_ token: String) -> String {
        if !token.isEmpty && !token.contains(where: { $0.isWhitespace || $0 == "\"" || $0 == "'" || $0 == "\\" }) { return token }
        return "\"" + token.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "\"", with: "\\\"") + "\""
    }

    private static func tokenize(_ text: String, incomplete: Bool) throws -> (tokens: [String], active: Bool) {
        var tokens: [String] = []
        var token = ""
        var quote: Character?
        var escaped = false
        var active = false
        for character in text {
            if escaped {
                token.append(character)
                escaped = false
            } else if character == "\\" {
                escaped = true
                active = true
            } else if let delimiter = quote {
                if character == delimiter { quote = nil } else { token.append(character) }
            } else if character == "\"" || character == "'" {
                quote = character
                active = true
            } else if character.isWhitespace {
                if active { tokens.append(token); token = ""; active = false }
            } else {
                token.append(character)
                active = true
            }
        }
        if !incomplete {
            if escaped { throw CommandLanguageError("Trailing escape: add the escaped character.") }
            if quote != nil { throw CommandLanguageError("Unterminated quoted argument.") }
        }
        if active { tokens.append(token) }
        return (tokens, active)
    }

    private static func validate(_ value: String, as argument: CommandArgument) throws {
        let valid: Bool
        switch argument.kind {
        case .text(let allowEmpty): valid = allowEmpty || !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        case .playlist: valid = !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        case .choices(let choices): valid = choices.contains(value)
        case .unsigned(let alternative):
            valid = value == alternative || (!value.isEmpty && value.utf8.allSatisfy { (48...57).contains($0) } && UInt64(value) != nil)
        case .seek: _ = try quantity(value, units: ["s", "m", "h", "%"]); return
        case .volume: _ = try quantity(value, units: ["%"]); return
        case .coverSize:
            let number = try quantity(value, units: [])
            valid = !number.relative && AlbumGridGeometry.coverRange.contains(NSDecimalNumber(decimal: number.magnitude).doubleValue)
        case .command: valid = CommandID(rawValue: value) != nil
        case .colour: valid = value == "system" || RGB(hex: value) != nil
        }
        guard valid else { throw CommandLanguageError("Invalid \(argument.name): \(value).") }
    }

    struct Quantity {
        let magnitude: Decimal
        let negative: Bool
        let relative: Bool
        let unit: Character?
    }

    static func quantity(_ value: String, units: Set<Character>) throws -> Quantity {
        var digits = value[...]
        let relative = digits.first == "+" || digits.first == "-"
        let negative = digits.first == "-"
        if relative { digits = digits.dropFirst() }
        var unit: Character?
        if let last = digits.last, units.contains(last) { unit = last; digits = digits.dropLast() }
        var dot = false
        var count = 0
        var significantDigits = 0
        var trailingZeros = 0
        for byte in digits.utf8 {
            if byte == 46 && !dot { dot = true }
            else if (48...57).contains(byte) {
                count += 1
                if byte != 48 {
                    significantDigits += trailingZeros + 1
                    trailingZeros = 0
                } else if significantDigits > 0 { trailingZeros += 1 }
            } else { throw CommandLanguageError("Invalid numeric value: \(value).") }
        }
        guard count > 0, significantDigits <= 38,
              let magnitude = Decimal(string: String(digits), locale: Locale(identifier: "en_US_POSIX")),
              !magnitude.isNaN, magnitude != 0 || significantDigits == 0 else {
            throw CommandLanguageError("Numeric value is invalid, overflows, or exceeds decimal precision: \(value).")
        }
        return Quantity(magnitude: magnitude, negative: negative, relative: relative, unit: unit)
    }
}
