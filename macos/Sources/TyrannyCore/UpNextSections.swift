/// The three sections of the Up Next view.
public enum QueueSection: Sendable, Equatable {
    case history, nowPlaying, upNext

    public var title: String {
        switch self {
        case .history: "HISTORY"
        case .nowPlaying: "NOW PLAYING"
        case .upNext: "UP NEXT"
        }
    }

    /// Identifier suffix: `up-next-section-<key>`.
    public var key: String {
        switch self {
        case .history: "history"
        case .nowPlaying: "now-playing"
        case .upNext: "up-next"
        }
    }
}

/// How queue entries fall into sections and what the "Added From" column shows.
///
/// A section comes from `QueueItem.played` and `current_id`, never from the queue position.
/// Shuffle and Previous leave played entries after the current one, and unplayed entries
/// before it.
public enum UpNextSections {
    public static func section(of item: QueueItem, currentID: UInt64?) -> QueueSection {
        if item.id == currentID { return .nowPlaying }
        return item.played ? .history : .upNext
    }

    /// Whether a section label shows above this entry: the first position, or a section
    /// different from the previous position's. An unloaded previous entry gives no label.
    public static func startsSection(item: QueueItem, previous: QueueItem?, position: Int,
                                     currentID: UInt64?) -> Bool {
        if position == 0 { return true }
        guard let previous else { return false }
        return section(of: previous, currentID: currentID) != section(of: item, currentID: currentID)
    }

    /// "Album · Spirit of Eden", "Playlist · Late Night", "Smart · Unplayed Jazz", "Songs",
    /// "Dragged from Songs", or blank.
    public static func addedFrom(_ source: QueueSource?) -> String {
        guard let source else { return "" }
        let name = source.name
        switch source.kind {
        case .album: return prefixed("Album", name)
        case .playlist: return prefixed("Playlist", name)
        case .smart: return prefixed("Smart", name)
        case .genre: return prefixed("Genre", name)
        case .artist: return prefixed("Artist", name)
        case .songs: return "Songs"
        case .recent: return "Recently Added"
        case .compilations: return "Compilations"
        case .drag: return name.isEmpty ? "Dragged" : "Dragged from \(name)"
        case .direct: return ""
        }
    }

    private static func prefixed(_ kind: String, _ name: String) -> String {
        name.isEmpty ? kind : "\(kind) · \(name)"
    }

    /// Entries after the current one that are still to play.
    public static func upcomingCount(total: Int, playedNotCurrent: Int, hasCurrent: Bool) -> Int {
        max(0, total - playedNotCurrent - (hasCurrent ? 1 : 0))
    }
}
