import SwiftUI
import TyrannyCore

/// The library window: toolbar filter, source list, main view, the full-width player bar,
/// and the status bar.
struct MainWindowView: View {
    let model: PlayerModel
    @Bindable var navigation: NavigationModel
    @Environment(\.openSettings) private var openSettings
    @FocusState private var filterFocused: Bool
    @State private var dismissedFilterError: String?
    @State private var lists = TrackLists()
    /// Whether the sidebar is on screen, from the split view's own sidebar item. The split view can
    /// collapse the sidebar without a visibility binding hearing of it (restored state, a drag,
    /// a narrow window), so expanded art follows this instead.
    @State private var sidebarOnScreen = true

    /// Set by the shown track list from `list_tracks` `invalid_filter` replies.
    private var filterError: String? { navigation.filterError }

    /// The shown destination's track list, if it is one.
    private var currentList: ListModel? {
        switch navigation.destination {
        case .songs:
            lists.list(for: .songs, source: .library, player: model, navigation: navigation)
        case .recent:
            lists.list(for: .recent, source: .recent, player: model, navigation: navigation, usesBrowser: false)
        case .artists, .genres:
            lists.browse(navigation.destination, player: model, navigation: navigation).detail
        case .playlist:
            currentPlaylist.map { lists.playlist($0, player: model, navigation: navigation) }
        default:
            nil
        }
    }

    /// The shown destination's playlist, once the catalog has it.
    private var currentPlaylist: Playlist? {
        guard case .playlist(let id) = navigation.destination else { return nil }
        return model.savedPlaylists.playlist(id: id)
    }

    /// The shown playlist was deleted (here or by another client).
    private var playlistGone: Bool {
        guard case .playlist = navigation.destination else { return false }
        return model.savedPlaylists.isLoaded && currentPlaylist == nil
    }

    /// The drawer follows the toggle while connected. Up Next itself already lists the queue.
    private var showsDrawer: Bool {
        navigation.upNextDrawerVisible && model.connected && navigation.destination != .upNext
    }

    var body: some View {
        VStack(spacing: 0) {
            NavigationSplitView {
                SourceList(model: model, navigation: navigation, sidebarVisible: sidebarOnScreen)
                    .navigationSplitViewColumnWidth(min: 170, ideal: 200, max: 300)
            } detail: {
                HStack(spacing: 0) {
                    mainArea
                    if showsDrawer {
                        Divider()
                        UpNextDrawer(model: model.upNext, player: model)
                    }
                }
                // In the detail column: the split view always loads it, even with the sidebar collapsed.
                .background(SidebarVisibilityProbe { sidebarOnScreen = $0 })
            }
            Divider()
            PlayerBar(model: model, navigation: navigation, sidebarVisible: sidebarOnScreen)
            Divider()
            StatusBar(model: model, totals: currentList?.totals, filteredFrom: currentList?.filteredFrom) {
                showSettings(.connection)
            }
        }
        .frame(minWidth: 940, minHeight: 560)
        .toolbar { toolbar }
        .onChange(of: navigation.filterFocusRequest) { _, _ in filterFocused = true }
        .sheet(item: Binding(get: { model.activation.prompt }, set: { model.activation.prompt = $0 })) { _ in
            UpNextPrompt(activation: model.activation)
        }
        .onChange(of: navigation.destination) { _, _ in model.endImport() }
        .onChange(of: navigation.reveal) { _, target in reveal(target) }
        .onChange(of: navigation.revealPlayingRequest) { _, _ in revealPlaying() }
        .onChange(of: filterError) { _, error in if error == nil { dismissedFilterError = nil } }
        .onChange(of: playlistGone) { _, gone in if gone { navigation.destination = .songs } }
        .onChange(of: model.savedPlaylists.cache.generation) { _, _ in
            guard model.savedPlaylists.isLoaded else { return }
            lists.prunePlaylists(keeping: Set(model.savedPlaylists.rows.map(\.id)))
        }
    }

    // MARK: - Toolbar

    /// The split view supplies the sidebar toggle; the toolbar holds only the filter. macOS puts
    /// primary actions at the leading edge, so a flexible space pushes the filter to the trailing edge.
    @ToolbarContentBuilder private var toolbar: some ToolbarContent {
        ToolbarItemGroup(placement: .primaryAction) {
            Spacer()
            filterField
        }
    }

    private var filterField: some View {
        TextField("Filter", text: $navigation.filter, prompt: Text("Filter"))
            .textFieldStyle(.roundedBorder)
            .frame(width: 190)
            .focused($filterFocused)
            .overlay {
                RoundedRectangle(cornerRadius: 5)
                    .strokeBorder(Color.red, lineWidth: filterError == nil ? 0 : 1.5)
                    .allowsHitTesting(false)
            }
            .help(filterError ?? "Filter title, artist or album with phrases, AND (&), OR (|), NOT (!), parentheses and field lists. ⌘F")
            .accessibilityIdentifier("library-filter")
            .popover(isPresented: Binding(
                get: { filterError != nil && dismissedFilterError != filterError },
                set: { if !$0 { dismissedFilterError = filterError } }
            ), arrowEdge: .bottom) {
                Text(filterError ?? "")
                    .font(.callout).fixedSize(horizontal: false, vertical: true)
                    .padding(10).frame(width: 300, alignment: .leading)
                    .accessibilityIdentifier("library-filter-error")
            }
    }

    // MARK: - Main area

    private var importScreen: ImportProgress? {
        #if DEBUG
        model.importOverride ?? model.importProgress
        #else
        model.importProgress
        #endif
    }

    @ViewBuilder private var mainArea: some View {
        if !model.connected {
            ContentUnavailableView {
                Label(model.connection == "connecting" ? "Connecting to Tyranny" : "Server Disconnected",
                      systemImage: "network")
            } description: {
                Text("The server owns playback and your library. Connect to browse and control it.")
            } actions: {
                Button("Connection Settings…") { showSettings(.connection) }
                    .accessibilityIdentifier("disconnected-settings")
                Button("Retry", action: model.reconnect)
                    .disabled(model.connection == "connecting" || model.serverManagementBusy)
                    .accessibilityIdentifier("disconnected-retry")
            }
        } else if let progress = importScreen {
            ImportProgressView(progress: progress, folder: model.importRootPath, model: model, navigation: navigation)
        } else if model.snapshot?.library.roots == 0 {
            ContentUnavailableView {
                Label("Add Your Music", systemImage: "music.note.house")
            } description: {
                Text("Choose a music folder to index on the server. Files stay in place, and playback continues when this app closes.")
            } actions: {
                Button("Choose Music Folder…", action: model.chooseFolder)
                    .buttonStyle(.borderedProminent)
                    .disabled(!model.canMutate || model.scanActive)
                    .accessibilityIdentifier("choose-folder")
            }
        } else if navigation.destination == .albums {
            AlbumGridView(grid: lists.albumGrid(player: model, navigation: navigation), player: model,
                          navigation: navigation)
        } else if navigation.destination == .artists {
            ArtistsView(model: lists.browse(.artists, player: model, navigation: navigation), player: model,
                        navigation: navigation)
        } else if navigation.destination == .genres {
            GenresView(model: lists.browse(.genres, player: model, navigation: navigation), player: model,
                       navigation: navigation)
        } else if navigation.destination == .upNext {
            UpNextView(model: model.upNext, player: model)
        } else if navigation.destination == .history {
            HistoryView(model: lists.history(player: model), player: model)
        } else if let list = currentList {
            TrackListView(list: list, player: model, navigation: navigation, playlist: currentPlaylist)
        } else {
            DestinationPlaceholder(destination: navigation.destination)
        }
    }

    /// Show in Album and Show in Artist: move to the destination and select the entry.
    private func reveal(_ target: NavigationModel.RevealTarget?) {
        guard let target else { return }
        navigation.reveal = nil
        switch target {
        case .album(let id):
            navigation.destination = .albums
            lists.albumGrid(player: model, navigation: navigation).reveal(albumID: id)
        case .artist(let id):
            navigation.destination = .artists
            lists.browse(.artists, player: model, navigation: navigation).select([id])
        }
    }

    /// ⌘L: the shown track list selects the playing row.
    private func revealPlaying() {
        currentList?.revealPlaying()
    }

    private func showSettings(_ tab: SettingsTab) {
        navigation.settingsTab = tab
        openSettings()
    }
}

/// Stands in for each destination's view until its task builds it.
private struct DestinationPlaceholder: View {
    let destination: Destination

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text(destination.title).font(.title2.weight(.semibold))
                .padding(.horizontal, 16).padding(.vertical, 10)
            Divider()
            Text(destination.title).font(.largeTitle).foregroundStyle(.tertiary)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("main-\(destination.key)")
    }
}
