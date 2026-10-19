import SwiftUI
import TyrannyCore

/// A track-list destination: the view header, the optional column browser and the
/// continuous table. Songs uses it now; the other track lists follow.
struct TrackListView: View {
    let list: ListModel
    let player: PlayerModel
    @Bindable var navigation: NavigationModel
    private let columns = ColumnPreferences.shared

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            if navigation.columnBrowserVisible {
                ColumnBrowser(list: list)
                Divider()
            }
            VirtualTrackTable(
                list: list, player: player, generation: list.cache.generation, total: list.cache.total,
                epoch: list.epoch, hidden: columns.hidden, playingTrackID: player.snapshot?.now_playing?.track_id)
                .overlay { emptyState }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("main-\(list.destination.key)")
        .onChange(of: syncKey, initial: true) { _, _ in list.sync() }
        .onChange(of: list.filterError, initial: true) { _, error in navigation.filterError = error }
        .onDisappear { navigation.filterError = nil }
        #if DEBUG
        .onAppear { if list.destination == .songs { NativeSongsSmoke.songsList = list } }
        #endif
    }

    private var syncKey: SyncKey {
        SyncKey(connected: player.connected, session: player.playlistConnectionGeneration,
                library: player.snapshot?.library.revision, history: player.snapshot?.history_revision,
                filter: navigation.filter, browser: navigation.columnBrowserVisible,
                grouping: navigation.grouping(for: list.destination))
    }

    private struct SyncKey: Equatable {
        let connected: Bool
        let session: UInt64
        let library: UInt64?
        let history: UInt64?
        let filter: String
        let browser: Bool
        let grouping: Grouping
    }

    // MARK: Header

    private var header: some View {
        HStack(spacing: 10) {
            Text(list.destination.title)
                .font(.title2.weight(.semibold))
                .accessibilityAddTraits(.isHeader)
            Text(countText)
                .foregroundStyle(.secondary)
                .monospacedDigit()
                .accessibilityIdentifier("list-count")
            Spacer(minLength: 12)
            Picker("Group", selection: Binding(
                get: { navigation.grouping(for: list.destination) },
                set: { navigation.setGrouping($0, for: list.destination) }
            )) {
                Text("None").tag(Grouping.none)
                Text("Artist").tag(Grouping.artist)
                Text("Album").tag(Grouping.album)
                Text("Artist › Album").tag(Grouping.artistAlbum)
            }
            .pickerStyle(.menu)
            .fixedSize()
            .disabled(true)  // Grouping arrives in Task 7.
            .help("Group")
            .accessibilityIdentifier("group-control")
            Toggle(isOn: $navigation.columnBrowserVisible) {
                Label("Browser", systemImage: "rectangle.split.3x1")
            }
            .toggleStyle(.button)
            .help(navigation.columnBrowserVisible ? "Hide Column Browser (⌘B)" : "Show Column Browser (⌘B)")
            .accessibilityIdentifier("browser-toggle")
            Menu {
                ForEach(TrackColumns.all.filter(\.hideable)) { column in
                    Toggle(column.title, isOn: Binding(
                        get: { !columns.isHidden(column.id) },
                        set: { columns.setHidden(column.id, !$0) }))
                }
            } label: {
                Label("Columns", systemImage: "tablecells")
            }
            .menuStyle(.borderlessButton)
            .fixedSize()
            .help("Columns")
            .accessibilityIdentifier("columns-menu")
        }
        .controlSize(.small)
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
    }

    private var countText: String {
        guard list.cache.revision != nil else { return "" }
        let count = DisplayFormat.songs(list.cache.total)
        guard let total = list.filteredFrom else { return count }
        return "\(DisplayFormat.count(list.cache.total)) of \(DisplayFormat.songs(total))"
    }

    @ViewBuilder private var emptyState: some View {
        if list.cache.revision != nil, list.cache.total == 0 {
            Text(list.isNarrowed ? "No matching songs" : "No songs")
                .font(.title3)
                .foregroundStyle(.secondary)
                .allowsHitTesting(false)
        }
    }
}
