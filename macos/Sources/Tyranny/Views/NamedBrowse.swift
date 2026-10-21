import Observation
import SwiftUI
import TyrannyCore

/// The Artists and Genres destinations (spec 4.5): a narrow list of album artists or genres,
/// continuous through `ListWindowCache<NamedCount>`, and the selected entry's tracks on the
/// right as a `TrackListView` grouped by Album.
///
/// The left list follows the filter field with the right list. Choosing an artist also
/// opens an Appears On list: the compilation tracks by that track artist.
@MainActor @Observable
final class NamedBrowseModel {
    enum Kind {
        case artists, genres

        var destination: Destination { self == .artists ? .artists : .genres }
        var command: String { self == .artists ? "list_artists" : "list_genres" }
        var listIdentifier: String { self == .artists ? "artists-list" : "genres-list" }
        var singular: String { self == .artists ? "Artist" : "Genre" }
        var plural: String { self == .artists ? "Artists" : "Genres" }
        var unknown: String { "Unknown \(singular)" }
    }

    let kind: Kind
    /// The left list's rows and, for Artists, its Compilations row.
    let column = BrowserColumnModel<NamedCount>()
    /// The chosen entry: one ID, or `BrowserSelection.compilationsID`. Empty before a choice.
    private(set) var selected: Set<UInt64> = []
    /// The chosen entry's tracks.
    private(set) var detail: ListModel?
    /// Compilation tracks by the chosen artist. Its own windowed cache, shown only when it has rows.
    private(set) var appearsOn: ListModel?
    private(set) var detailTitle: String?

    @ObservationIgnored private weak var player: PlayerModel?
    @ObservationIgnored private let navigation: NavigationModel
    @ObservationIgnored private var session: UInt64?
    @ObservationIgnored private var revision: UInt64?
    /// The query the left list was requested with. nil before the first load.
    @ObservationIgnored private var loadedQuery: String?
    @ObservationIgnored private var pendingFilter: String?
    @ObservationIgnored private var filterTask: Task<Void, Never>?

    init(kind: Kind, player: PlayerModel, navigation: NavigationModel) {
        self.kind = kind
        self.player = player
        self.navigation = navigation
    }

    /// Follows the connection, the library revision and the filter. The view calls it when
    /// any of them changes. It also syncs the right-hand lists, so Appears On knows its
    /// total while hidden.
    func sync() {
        guard let player else { return }
        defer {
            detail?.sync()
            appearsOn?.sync()
        }
        let generation = player.playlistConnectionGeneration
        guard player.connected, let snapshot = player.snapshot else {
            if session != nil {
                session = nil
                column.cache.discard(session: generation)
            }
            return
        }
        let filter = navigation.filter.trimmingCharacters(in: .whitespacesAndNewlines)
        let library = snapshot.library.revision
        if session != generation || loadedQuery == nil {
            session = generation
            cancelFilter()
            load(query: filter, revision: library)
        } else if filter != loadedQuery {
            if filter.isEmpty {
                cancelFilter()
                load(query: filter, revision: library)
            } else if filter != pendingFilter {
                schedule(filter)
            }
        } else {
            if pendingFilter != nil { cancelFilter() }
            if library != revision {
                revision = library
                column.cache.serverRevisionChanged(library, anchor: nil)
            }
        }
    }

    /// Chooses an entry. The left list is single-selection; an empty set clears the choice.
    func select(_ ids: Set<UInt64>) {
        guard ids != selected, let player else { return }
        selected = ids
        guard let id = ids.first else {
            detail = nil
            appearsOn = nil
            detailTitle = nil
            return
        }
        let compilations = kind == .artists && id == BrowserSelection.compilationsID
        let source: ListSource = switch kind {
        case .artists: compilations ? .compilations : .artist(id: id)
        case .genres: .genre(id: id)
        }
        let key = kind.destination.key
        detail = ListModel(destination: kind.destination, source: source, player: player,
                           navigation: navigation, key: "\(key)-tracks", usesBrowser: false)
        appearsOn = kind == .artists && !compilations
            ? ListModel(destination: .artists, source: .appearsOn(artistID: id), player: player,
                        navigation: navigation, key: "artists-appears-on", usesBrowser: false)
            : nil
        detailTitle = title(of: id)
        detail?.displayName = detailTitle
        appearsOn?.displayName = detailTitle
        detail?.sync()
        appearsOn?.sync()
    }

    private func title(of id: UInt64) -> String {
        if id == BrowserSelection.compilationsID { return "Compilations" }
        let name = column.cache.loadedRows().first { $0.row.id == id }?.row.name ?? ""
        return name.isEmpty ? kind.unknown : name
    }

    private func load(query: String, revision: UInt64) {
        loadedQuery = query
        self.revision = revision
        let kind = self.kind
        let column = self.column
        let player: @MainActor () -> PlayerModel? = { [weak self] in self?.player }
        column.reset { epoch in
            namedCountFetch(player: player, command: kind.command, column: column, query: query,
                            narrowing: nil, epoch: epoch)
        }
    }

    /// Typing waits 250 ms before the left list refetches, as the track lists do.
    private func schedule(_ filter: String) {
        filterTask?.cancel()
        pendingFilter = filter
        filterTask = Task { [weak self] in
            do { try await Task.sleep(for: .milliseconds(250)) } catch { return }
            guard let self, self.pendingFilter == filter else { return }
            self.pendingFilter = nil
            self.loadedQuery = nil
            self.sync()
        }
    }

    private func cancelFilter() {
        filterTask?.cancel()
        filterTask = nil
        pendingFilter = nil
    }
}

struct NamedBrowseView: View {
    let model: NamedBrowseModel
    let player: PlayerModel
    @Bindable var navigation: NavigationModel

    private static let listWidth: CGFloat = 220
    private static let appearsOnHeight: CGFloat = 220

    var body: some View {
        let kind = model.kind
        HStack(spacing: 0) {
            BrowserColumnTable(
                column: model.column, identifier: kind.listIdentifier, title: kind.plural,
                singular: kind.singular, plural: kind.plural, selected: model.selected,
                generation: model.column.cache.generation, total: model.column.cache.total,
                compilations: kind == .artists ? model.column.compilations : nil,
                label: { $0.name.isEmpty ? kind.unknown : $0.name },
                help: { "\($0.name) · \(DisplayFormat.count($0.album_count)) albums · \(DisplayFormat.songs($0.track_count))" },
                select: model.select, colours: player.theme.tableColours, showsAll: false, allowsMultiple: false)
                .frame(width: Self.listWidth)
            Divider()
            detail
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("main-\(kind.destination.key)")
        .onChange(of: syncKey, initial: true) { _, _ in model.sync() }
        #if DEBUG
        .onAppear { NativeArtistsSmoke.browse = model }
        #endif
    }

    private var syncKey: SyncKey {
        SyncKey(connected: player.connected, session: player.playlistConnectionGeneration,
                library: player.snapshot?.library.revision, filter: navigation.filter)
    }

    private struct SyncKey: Equatable {
        let connected: Bool
        let session: UInt64
        let library: UInt64?
        let filter: String
    }

    @ViewBuilder private var detail: some View {
        if let list = model.detail {
            VStack(spacing: 0) {
                TrackListView(list: list, player: player, navigation: navigation,
                              chrome: .page(title: model.detailTitle))
                    .id(ObjectIdentifier(list))
                if let appearsOn = model.appearsOn, appearsOn.cache.total > 0 {
                    Divider()
                    TrackListView(list: appearsOn, player: player, navigation: navigation,
                                  chrome: .section(title: "Appears On", identifier: "artist-appears-on"))
                        .id(ObjectIdentifier(appearsOn))
                        .frame(height: Self.appearsOnHeight)
                }
            }
        } else {
            Text("Select \(model.kind == .artists ? "an" : "a") \(model.kind.singular)")
                .font(.title3)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .accessibilityIdentifier("\(model.kind.destination.key)-placeholder")
        }
    }
}
