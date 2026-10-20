import Foundation

/// The album grid's geometry for one width and cover size (spec 4.4).
///
/// Cells are `coverSize` wide. The columns are as many as fit between the minimum insets,
/// and the grid is centred: a full row fills the width between `horizontalInset` and
/// `trailingInset` exactly, so a flow layout never spreads its cells apart. The expansion panel spans the width
/// between the minimum insets, less one point (a flow item must be narrower than its line).
public struct AlbumGridGeometry: Equatable, Sendable {
    public static let spacing: Double = 16
    public static let lineSpacing: Double = 20
    public static let minimumInset: Double = 20
    /// The title and the "artist · year" line under each cover.
    public static let textHeight: Double = 40
    public static let coverRange: ClosedRange<Double> = 96...200

    public let width: Double
    public let coverSize: Double
    public let columns: Int
    /// The leading inset. The trailing one takes any odd point left over.
    public let horizontalInset: Double
    public let trailingInset: Double

    public init(width: Double, coverSize: Double) {
        let cover = min(max(coverSize, Self.coverRange.lowerBound), Self.coverRange.upperBound)
        self.width = width
        self.coverSize = cover
        let available = width - 2 * Self.minimumInset
        let fit = Int(((available + Self.spacing) / (cover + Self.spacing)).rounded(.down))
        columns = max(1, fit)
        let used = Double(columns) * cover + Double(columns - 1) * Self.spacing
        horizontalInset = max(Self.minimumInset, ((width - used) / 2).rounded(.down))
        trailingInset = max(Self.minimumInset, width - used - horizontalInset)
    }

    public var itemWidth: Double { coverSize }
    public var itemHeight: Double { coverSize + Self.textHeight }
    public var panelWidth: Double { max(1, width - 2 * Self.minimumInset - 1) }

    /// The centre of the cover in `column`, measured from the panel's left edge.
    public func caretX(column: Int) -> Double {
        horizontalInset + Double(column) * (coverSize + Self.spacing) + coverSize / 2 - Self.minimumInset
    }
}

/// Where the grid's items sit: albums, and the expansion panel after the last cell of the
/// expanded album's row (spec 4.4).
///
/// Three sections, so every line before the panel is a section's full or last line: a
/// flow layout spreads a partial line that is followed by more items in its section.
/// Section 0 holds the albums up to the end of the expanded row (all albums when nothing
/// is expanded), section 1 the panel, section 2 the remaining albums.
public struct AlbumGridLayout: Equatable, Sendable {
    public static let sections = 3

    public enum Item: Equatable, Sendable {
        case album(Int)
        case expansion
    }

    public struct Location: Equatable, Hashable, Sendable {
        public let section: Int
        public let item: Int
        public init(section: Int, item: Int) {
            self.section = section
            self.item = item
        }
    }

    public let count: Int
    public let columns: Int
    /// The expanded album's position; nil, or past the end, shows no panel.
    public let expanded: Int?

    public init(count: Int, columns: Int, expanded: Int?) {
        self.count = max(0, count)
        self.columns = max(1, columns)
        self.expanded = expanded.flatMap { (0..<max(0, count)).contains($0) ? $0 : nil }
    }

    /// Albums before the panel.
    public var split: Int {
        guard let expanded else { return count }
        return min(count, (expanded / columns + 1) * columns)
    }

    public var expandedColumn: Int? { expanded.map { $0 % columns } }

    public func numberOfItems(inSection section: Int) -> Int {
        switch section {
        case 0: split
        case 1: expanded == nil ? 0 : 1
        case 2: count - split
        default: 0
        }
    }

    public func item(section: Int, item: Int) -> Item? {
        guard item >= 0, item < numberOfItems(inSection: section) else { return nil }
        switch section {
        case 0: return .album(item)
        case 1: return .expansion
        default: return .album(split + item)
        }
    }

    public func location(ofAlbum position: Int) -> Location? {
        guard position >= 0, position < count else { return nil }
        return position < split ? Location(section: 0, item: position)
            : Location(section: 2, item: position - split)
    }

    /// Every item keeps its index path: a re-layout suffices, without reloading items (which
    /// would release and re-request their covers).
    public func hasSameItems(as other: AlbumGridLayout) -> Bool {
        count == other.count && expanded == other.expanded
            && (0..<Self.sections).allSatisfy { numberOfItems(inSection: $0) == other.numberOfItems(inSection: $0) }
    }

    /// The album positions spanned by `locations`; nil when they hold no album.
    public func albumRange(of locations: some Sequence<Location>) -> Range<Int>? {
        var lower = Int.max, upper = Int.min
        for location in locations {
            guard case .album(let position)? = item(section: location.section, item: location.item) else { continue }
            lower = min(lower, position)
            upper = max(upper, position)
        }
        return lower <= upper ? lower..<upper + 1 : nil
    }
}

public enum AlbumPlayback {
    /// The album's tracks in album order.
    public static func list(albumID: UInt64) -> ListDescription {
        ListDescription(source: .album(id: albumID))
    }

    /// The whole album as a source selection, so the server picks the start (the first track,
    /// or a shuffle pick under shuffle).
    public static func selection(albumID: UInt64, libraryRevision: UInt64) -> TrackSelection {
        SelectionEncoder.source(list: list(albumID: albumID), revision: libraryRevision)
    }

    public static func source(title: String) -> QueueSource {
        QueueSource(kind: .album, name: title)
    }
}

/// The album panel's tracklist: at most `trackLimit` tracks in one request, and when to
/// fetch them again.
public enum AlbumPanel {
    /// Longer albums show their first tracks and "and N more…".
    public static let trackLimit = 200
    /// The table scrolls past this many rows.
    public static let maximumRows = 12

    /// What the shown tracks were fetched for.
    public struct Loaded: Equatable, Sendable {
        public let album: AlbumSummary
        public let revision: UInt64
        public init(album: AlbumSummary, revision: UInt64) {
            self.album = album
            self.revision = revision
        }
    }

    /// Fetch when nothing is loaded or the album's summary changed. A bare library revision
    /// change refetches only once no scan is running, so scan batches do not refetch.
    public static func needsReload(loaded: Loaded?, current: AlbumSummary, revision: UInt64, scanning: Bool) -> Bool {
        guard let loaded else { return true }
        if loaded.album != current { return true }
        return !scanning && loaded.revision != revision
    }

    public static func moreText(total: Int, shown: Int) -> String? {
        total > shown ? "and \(DisplayFormat.count(total - shown)) more…" : nil
    }

    /// The rows the panel sends. No rows chosen sends the whole album, and the server picks
    /// the start. Otherwise it is an activation over the album's tracks: one row as a run
    /// plays from it to the album's end, and anything else sends exactly the rows chosen.
    public static func selection(
        albumID: UInt64, revision: UInt64, positions: IndexSet, asRun: Bool,
        rowID: (Int) -> UInt64?, trackID: (Int) -> UInt64?
    ) throws -> TrackSelection {
        guard !positions.isEmpty else { return AlbumPlayback.selection(albumID: albumID, libraryRevision: revision) }
        return try SelectionEncoder.activation(list: AlbumPlayback.list(albumID: albumID), revision: revision,
                                               positions: positions, asRun: asRun, rowID: rowID, trackID: trackID)
    }

    /// The rows the table shows without scrolling. An empty album still shows one.
    public static func visibleRows(trackCount: Int) -> Int {
        max(1, min(trackCount, trackLimit, maximumRows))
    }

    /// "and N more…" shows when the album has more tracks than one request fetches.
    public static func showsMore(trackCount: Int) -> Bool {
        trackCount > trackLimit
    }
}
