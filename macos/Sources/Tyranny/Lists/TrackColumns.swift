import AppKit
import Observation
import TyrannyCore

/// One track-table column. `id` names it in identifiers and in the `hiddenColumns`
/// preference. `sort` is the server sort field a header click selects.
struct TrackColumn: Identifiable {
    let id: String
    let title: String
    let width: CGFloat
    var minWidth: CGFloat = 30
    var sort: SortField?
    var alignment: NSTextAlignment = .left
    var monospacedDigits = false
    /// The status and Title columns are always shown.
    var hideable = true
    var hiddenByDefault = false
    let value: (TrackSummary) -> String
}

/// The Songs columns (spec 4.2): status, #, Title, Artist, Album, Time, Year, Genre,
/// Format, Plays; hidden by default: Album Artist, Disc, Date Added, Last Played,
/// Bitrate, Sample Rate.
@MainActor
enum TrackColumns {
    static let status = "status"

    static let all: [TrackColumn] = [
        TrackColumn(id: status, title: "", width: 22, minWidth: 22, hideable: false) { _ in "" },
        TrackColumn(id: "number", title: "#", width: 34, sort: .albumOrder, alignment: .right,
                    monospacedDigits: true) { $0.track_number == 0 ? "" : String($0.track_number) },
        TrackColumn(id: "title", title: "Title", width: 240, minWidth: 80, sort: .title, hideable: false) { $0.title },
        TrackColumn(id: "artist", title: "Artist", width: 160, sort: .artist) { $0.artist },
        TrackColumn(id: "album", title: "Album", width: 180, sort: .album) { $0.album },
        TrackColumn(id: "time", title: "Time", width: 52, sort: .duration, alignment: .right, monospacedDigits: true) {
            $0.duration_ms.map { DisplayFormat.clock(Double($0) / 1000) } ?? ""
        },
        TrackColumn(id: "year", title: "Year", width: 54, sort: .year, alignment: .right, monospacedDigits: true) {
            $0.year.map(String.init) ?? ""
        },
        TrackColumn(id: "genre", title: "Genre", width: 100, sort: .genre) { $0.genre },
        TrackColumn(id: "format", title: "Format", width: 92, sort: .format) { $0.format },
        TrackColumn(id: "plays", title: "Plays", width: 44, sort: .plays, alignment: .right, monospacedDigits: true) {
            $0.play_count == 0 ? "" : DisplayFormat.count($0.play_count)
        },
        TrackColumn(id: "album-artist", title: "Album Artist", width: 160, sort: .albumArtist, hiddenByDefault: true) {
            $0.album_artist
        },
        TrackColumn(id: "disc", title: "Disc", width: 36, sort: .albumOrder, alignment: .right, monospacedDigits: true,
                    hiddenByDefault: true) { $0.disc_number == 0 ? "" : String($0.disc_number) },
        TrackColumn(id: "date-added", title: "Date Added", width: 128, sort: .dateAdded, hiddenByDefault: true) {
            $0.date_added > 0 ? date($0.date_added) : ""
        },
        TrackColumn(id: "last-played", title: "Last Played", width: 128, sort: .lastPlayed, hiddenByDefault: true) {
            $0.last_played.map(date) ?? ""
        },
        TrackColumn(id: "bitrate", title: "Bitrate", width: 76, alignment: .right, monospacedDigits: true,
                    hiddenByDefault: true) { DisplayFormat.kilobits($0.bitrate_kbps) },
        TrackColumn(id: "sample-rate", title: "Sample Rate", width: 80, alignment: .right, monospacedDigits: true,
                    hiddenByDefault: true) { DisplayFormat.sampleRate($0.sample_rate) },
    ]

    static func column(_ id: String) -> TrackColumn? { all.first { $0.id == id } }

    static let defaultHidden = Set(all.filter(\.hiddenByDefault).map(\.id))

    nonisolated private static func date(_ seconds: Int64) -> String {
        Date(timeIntervalSince1970: TimeInterval(seconds)).formatted(date: .numeric, time: .shortened)
    }
}

/// Which track-table columns are hidden, in the `hiddenColumns` preference.
/// Shared by every track table, the header context menu and the Columns menu.
@MainActor @Observable
final class ColumnPreferences {
    static let shared = ColumnPreferences()

    private(set) var hidden: Set<String>
    @ObservationIgnored private let defaults: UserDefaults

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        hidden = (defaults.stringArray(forKey: "hiddenColumns")).map(Set.init) ?? TrackColumns.defaultHidden
    }

    func isHidden(_ id: String) -> Bool { hidden.contains(id) }

    func setHidden(_ id: String, _ isHidden: Bool) {
        guard TrackColumns.column(id)?.hideable == true, hidden.contains(id) != isHidden else { return }
        if isHidden { hidden.insert(id) } else { hidden.remove(id) }
        defaults.set(hidden.sorted(), forKey: "hiddenColumns")
    }

    func toggle(_ id: String) { setHidden(id, !isHidden(id)) }
}
