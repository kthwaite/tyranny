import Foundation

/// The selection of a virtualized list, held by row ID (spec 5.4).
///
/// `positions` are the selected rows the table shows. `ids` are the selected rows'
/// IDs, as far as they are known. A selected placeholder has a position but no ID yet.
///
/// The `Basis` says what the positions refer to: the list description (`epoch`) and
/// the page revision. While the basis holds, positions are stable: a row that loads at
/// a selected position joins `ids`, and a loaded row whose ID is selected is shown
/// selected. When the revision changes or a refresh commits, rows may have moved, so loaded rows are
/// selected exactly when their ID is. When the description changes, every old position
/// is dropped and rows are reselected by ID as they load.
public struct RowSelection: Equatable, Sendable {
    public struct Basis: Equatable, Sendable {
        public var epoch: UInt64
        public var revision: UInt64?
        /// Counts committed refreshes. A refresh may move rows without a revision change,
        /// for example a play-count change under a Plays sort.
        public var refresh: UInt64

        public init(epoch: UInt64, revision: UInt64?, refresh: UInt64 = 0) {
            self.epoch = epoch
            self.revision = revision
            self.refresh = refresh
        }
    }

    public private(set) var ids: Set<UInt64> = []
    public private(set) var positions = IndexSet()
    private var basis: Basis?

    public init() {}

    /// The user selected `positions`. IDs not loaded at those positions are forgotten.
    public mutating func select(_ positions: IndexSet, rowID: (Int) -> UInt64?, basis: Basis) {
        self.positions = positions
        ids = Set(positions.compactMap(rowID))
        self.basis = basis
    }

    /// The loaded rows or the total changed. `loaded` lists every loaded row.
    public mutating func rowsChanged(loaded: [(position: Int, id: UInt64)], total: Int, basis next: Basis) {
        var selected = positions
        if total < Int.max { selected.remove(integersIn: max(0, total)..<Int.max) }
        if next.epoch != basis?.epoch {
            selected = IndexSet()
            for row in loaded where ids.contains(row.id) { selected.insert(row.position) }
        } else if next.revision != basis?.revision || next.refresh != basis?.refresh {
            for row in loaded {
                if ids.contains(row.id) { selected.insert(row.position) } else { selected.remove(row.position) }
            }
        } else {
            for row in loaded {
                if selected.contains(row.position) { ids.insert(row.id) }
                if ids.contains(row.id) { selected.insert(row.position) }
            }
        }
        positions = selected
        basis = next
    }

    public mutating func clear() {
        ids = []
        positions = IndexSet()
    }
}

/// The column browser's selection: Genre | Artist | Album. An empty dimension is "All".
/// Changing a column resets the columns to its right, as their contents change.
public struct BrowserSelection: Equatable, Sendable {
    /// The Artist column's Compilations row. Album artist IDs never reach it.
    public static let compilationsID = UInt64.max

    public private(set) var genres: Set<UInt64> = []
    public private(set) var artists: Set<UInt64> = []
    public private(set) var albums: Set<UInt64> = []

    public init() {}

    public mutating func setGenres(_ ids: Set<UInt64>) {
        guard ids != genres else { return }
        genres = ids
        artists = []
        albums = []
    }

    public mutating func setArtists(_ ids: Set<UInt64>) {
        guard ids != artists else { return }
        artists = ids
        albums = []
    }

    public mutating func setAlbums(_ ids: Set<UInt64>) {
        albums = ids
    }

    /// The narrowing for the track list.
    public var browser: Browser {
        var browser = albumColumnBrowser
        browser.album_ids = albums.sorted()
        return browser
    }

    /// The narrowing for the Album column: genres and artists only.
    public var albumColumnBrowser: Browser {
        Browser(genre_ids: genres.sorted(),
                artist_ids: artists.subtracting([Self.compilationsID]).sorted(),
                compilations: artists.contains(Self.compilationsID),
                album_ids: [])
    }
}
