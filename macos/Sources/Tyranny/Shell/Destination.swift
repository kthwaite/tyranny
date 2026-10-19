import TyrannyCore

/// A source-list destination. `key` names it in identifiers (`source-<key>`)
/// and preferences (`group.<key>`).
enum Destination: Hashable {
    case songs, albums, artists, genres, recent, upNext, history, playlist(UInt64)

    static let library: [Destination] = [.songs, .albums, .artists, .genres, .recent]
    static let playing: [Destination] = [.upNext, .history]

    var key: String {
        switch self {
        case .songs: "songs"
        case .albums: "albums"
        case .artists: "artists"
        case .genres: "genres"
        case .recent: "recent"
        case .upNext: "up-next"
        case .history: "history"
        case .playlist(let id): "playlist-\(id)"
        }
    }

    var identifier: String { "source-\(key)" }

    var title: String {
        switch self {
        case .songs: "Songs"
        case .albums: "Albums"
        case .artists: "Artists"
        case .genres: "Genres"
        case .recent: "Recently Added"
        case .upNext: "Up Next"
        case .history: "History"
        case .playlist: "Playlist"
        }
    }

    var systemImage: String {
        switch self {
        case .songs: "music.note"
        case .albums: "square.stack"
        case .artists: "music.mic"
        case .genres: "guitars"
        case .recent: "clock"
        case .upNext: "list.bullet"
        case .history: "clock.arrow.circlepath"
        case .playlist: "music.note.list"
        }
    }

    /// Grouping used until the user picks one for this destination (spec 4.3, 4.5, 4.6):
    /// Recently Added and the Artists/Genres detail group by Album; Songs and playlists none.
    var defaultGrouping: Grouping {
        switch self {
        case .recent, .artists, .genres: .album
        case .songs, .albums, .upNext, .history, .playlist: .none
        }
    }
}
