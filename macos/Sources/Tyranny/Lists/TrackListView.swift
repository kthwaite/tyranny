import SwiftUI
import TyrannyCore

/// A track-list destination: the view header, the optional column browser and the
/// continuous table, grouped by the destination's group setting (spec 4.3). Songs uses
/// it now; the other track lists follow.
struct TrackListView: View {
    let list: ListModel
    let player: PlayerModel
    @Bindable var navigation: NavigationModel
    var chrome: Chrome = .page(title: nil)
    private let columns = ColumnPreferences.shared

    /// A page fills the main area. A section sits under another list with a small heading
    /// (Appears On) and the identifier the section is known by.
    enum Chrome {
        case page(title: String?)
        case section(title: String, identifier: String)
    }

    var body: some View {
        VStack(spacing: 0) {
            switch chrome {
            case .page: header
            case .section(let title, _): sectionHeader(title)
            }
            Divider()
            if navigation.columnBrowserVisible, list.usesBrowser, case .page = chrome {
                ColumnBrowser(list: list)
                Divider()
            }
            VirtualTrackTable(
                list: list, player: player, generation: list.cache.generation, epoch: list.epoch,
                layoutToken: list.layoutToken, factsGeneration: list.facts.generation, hidden: columns.hidden, explicit: columns.explicit,
                playingTrackID: player.snapshot?.now_playing?.track_id)
                .overlay { emptyState }
                .overlay(alignment: .bottom) { reorderHint }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(containerIdentifier)
        .onChange(of: syncKey, initial: true) { _, _ in list.sync() }
        .onChange(of: layoutKey, initial: true) { _, _ in list.syncLayout() }
        .onChange(of: list.filterError, initial: true) { _, error in navigation.filterError = error }
        .onDisappear { navigation.filterError = nil }
        #if DEBUG
        .onAppear {
            if list.destination == .songs { NativeSongsSmoke.songsList = list }
            if list.destination == .recent { NativeHistorySmoke.recent = list }
        }
        #endif
    }

    private var containerIdentifier: String {
        if case .section(_, let identifier) = chrome { return identifier }
        return "main-\(list.key)"
    }

    private var syncKey: SyncKey {
        SyncKey(connected: player.connected, session: player.playlistConnectionGeneration,
                library: player.snapshot?.library.revision, history: player.snapshot?.history_revision,
                filter: navigation.filter, browser: list.usesBrowser && navigation.columnBrowserVisible,
                grouping: navigation.grouping(for: list.destination))
    }

    /// The cache's rows moved on: grouped lists fetch spans for them.
    private var layoutKey: LayoutKey {
        LayoutKey(epoch: list.epoch, revision: list.cache.revision, refreshes: list.cache.refreshes,
                  connected: player.connected)
    }

    private struct LayoutKey: Equatable {
        let epoch: UInt64
        let revision: UInt64?
        let refreshes: UInt64
        let connected: Bool
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
            Text(pageTitle)
                .font(.title2.weight(.semibold))
                .lineLimit(1)
                .truncationMode(.tail)
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
            .help("Group")
            .accessibilityIdentifier("group-control")
            if list.usesBrowser {
                Toggle(isOn: $navigation.columnBrowserVisible) {
                    Label("Browser", systemImage: "rectangle.split.3x1")
                }
                .toggleStyle(.button)
                .help(navigation.columnBrowserVisible ? "Hide Column Browser (⌘B)" : "Show Column Browser (⌘B)")
                .accessibilityIdentifier("browser-toggle")
            }
            Menu {
                ForEach(TrackColumns.all.filter(\.hideable)) { column in
                    Toggle(column.title, isOn: Binding(
                        get: { columns.isShown(column.id, sortFields: list.sortKeys.map(\.field)) },
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

    private var pageTitle: String {
        if case .page(let title?) = chrome { return title }
        return list.destination.title
    }

    private func sectionHeader(_ title: String) -> some View {
        HStack(spacing: 10) {
            Text(title)
                .font(.headline)
                .accessibilityAddTraits(.isHeader)
            Text(countText)
                .foregroundStyle(.secondary)
                .monospacedDigit()
                .accessibilityIdentifier("\(containerIdentifier)-count")
            Spacer(minLength: 0)
        }
        .controlSize(.small)
        .padding(.horizontal, 16)
        .padding(.vertical, 6)
    }

    private var countText: String {
        guard list.cache.revision != nil else { return "" }
        let count = DisplayFormat.songs(list.cache.total)
        guard let total = list.filteredFrom else { return count }
        return "\(DisplayFormat.count(list.cache.total)) of \(DisplayFormat.songs(total))"
    }

    /// A refused reorder drop in a grouped or sorted regular playlist says how to reorder.
    @ViewBuilder private var reorderHint: some View {
        if list.reorderHintShown {
            Text(PlaylistReorder.hint)
                .font(.callout)
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
                .background(.regularMaterial, in: Capsule())
                .padding(.bottom, 12)
                .allowsHitTesting(false)
                .accessibilityIdentifier("playlist-reorder-hint")
        }
    }

    @ViewBuilder private var emptyState: some View {
        if list.cache.revision != nil, list.cache.total == 0 {
            if list.destination == .recent, !list.isNarrowed {
                Text("Nothing added in the last 30 days")
                    .font(.title3)
                    .foregroundStyle(.secondary)
                    .allowsHitTesting(false)
                    .accessibilityIdentifier("recent-empty")
            } else {
                Text(list.isNarrowed ? "No matching songs" : "No songs")
                    .font(.title3)
                    .foregroundStyle(.secondary)
                    .allowsHitTesting(false)
            }
        }
    }
}
