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

/// What decides the sections: the current entry, its queue position, and the shuffle state.
public struct QueueContext: Sendable, Equatable {
    public var currentID: UInt64?
    public var currentIndex: Int?
    public var shuffle: Bool

    public init(currentID: UInt64?, currentIndex: Int?, shuffle: Bool) {
        self.currentID = currentID
        self.currentIndex = currentIndex
        self.shuffle = shuffle
    }

    /// From the snapshot; nothing current without one.
    public init(_ state: ControlState?) {
        self.init(currentID: state?.current_id, currentIndex: state?.current_index,
                  shuffle: state?.settings.shuffle ?? false)
    }
}

/// How queue entries fall into sections and what the "Added From" column shows.
///
/// The rule is the server's definition of upcoming (`ControlState.upcoming_count`):
/// - Shuffle off: entries before the current one are HISTORY (played or skipped), entries after
///   it are UP NEXT, in queue order.
/// - Shuffle on: played entries are HISTORY, unplayed entries other than the current one are
///   UP NEXT, wherever they sit.
/// - Nothing current: played entries are HISTORY, unplayed ones UP NEXT.
public enum UpNextSections {
    public static func section(of item: QueueItem, at position: Int, in context: QueueContext) -> QueueSection {
        guard let currentID = context.currentID else { return item.played ? .history : .upNext }
        if item.id == currentID { return .nowPlaying }
        if !context.shuffle, let index = context.currentIndex {
            return position < index ? .history : .upNext
        }
        return item.played ? .history : .upNext
    }

    /// Whether a section label shows above this entry: the first position, or a section
    /// different from the previous position's. An unloaded previous entry gives no label.
    public static func startsSection(item: QueueItem, previous: QueueItem?, position: Int,
                                     in context: QueueContext) -> Bool {
        if position == 0 { return true }
        guard let previous else { return false }
        return section(of: previous, at: position - 1, in: context) != section(of: item, at: position, in: context)
    }

    /// The UP NEXT entries among `rows` (loaded rows with their positions, ascending), at most
    /// `limit`, in queue order.
    public static func upcoming(_ rows: [(position: Int, row: QueueItem)], in context: QueueContext,
                                limit: Int) -> [QueueItem] {
        var result: [QueueItem] = []
        for (position, row) in rows where section(of: row, at: position, in: context) == .upNext {
            guard result.count < limit else { break }
            result.append(row)
        }
        return result
    }

    /// The queue positions the drawer reads to list `limit` upcoming entries: after the current
    /// entry with shuffle off, else from the start.
    public static func drawerRange(in context: QueueContext, limit: Int) -> Range<Int> {
        if context.currentID != nil, !context.shuffle, let index = context.currentIndex {
            return index + 1..<index + 1 + limit
        }
        return 0..<limit
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
        case .stream: return name.isEmpty ? "Stream" : "Stream: \(name)"
        }
    }

    private static func prefixed(_ kind: String, _ name: String) -> String {
        name.isEmpty ? kind : "\(kind) · \(name)"
    }
}
