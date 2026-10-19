import SwiftUI
import TyrannyCore

/// The library window: source list, toolbar transport and filter, main view,
/// and the status bar.
struct MainWindowView: View {
    let model: PlayerModel
    @Bindable var navigation: NavigationModel
    @Environment(\.openSettings) private var openSettings
    @FocusState private var filterFocused: Bool
    @State private var dismissedFilterError: String?
    @State private var lists = TrackLists()

    private var transport: TransportModel { model.transport }
    /// Set by the shown track list from `list_tracks` `invalid_filter` replies.
    private var filterError: String? { navigation.filterError }

    /// The shown destination's track list, if it is one.
    private var currentList: ListModel? {
        switch navigation.destination {
        case .songs:
            lists.list(for: .songs, source: .library, player: model, navigation: navigation)
        default:
            nil
        }
    }

    var body: some View {
        VStack(spacing: 0) {
            NavigationSplitView {
                SourceList(model: model, navigation: navigation)
                    .navigationSplitViewColumnWidth(min: 170, ideal: 200, max: 300)
            } detail: {
                mainArea
            }
            Divider()
            StatusBar(model: model, totals: currentList?.totals, filteredFrom: currentList?.filteredFrom) {
                showSettings(.connection)
            }
        }
        .frame(minWidth: 940, minHeight: 560)
        .toolbar { toolbar }
        .onChange(of: navigation.filterFocusRequest) { _, _ in filterFocused = true }
        .onChange(of: filterError) { _, error in if error == nil { dismissedFilterError = nil } }
    }

    // MARK: - Toolbar

    @ToolbarContentBuilder private var toolbar: some ToolbarContent {
        ToolbarItemGroup(placement: .navigation) {
            Button(action: transport.previous) {
                Label("Previous", systemImage: "backward.fill")
            }
            .disabled(!transport.canPrevious)
            .help("Previous")
            .accessibilityIdentifier("transport-previous")
            Button(action: transport.playPause) {
                Label(transport.isPlaying ? "Pause" : "Play",
                      systemImage: transport.isPlaying ? "pause.fill" : "play.fill")
            }
            .disabled(!transport.canPlayPause)
            .help(transport.isPlaying ? "Pause" : "Play")
            .accessibilityIdentifier("transport-play-pause")
            Button(action: transport.next) {
                Label("Next", systemImage: "forward.fill")
            }
            .disabled(!transport.canNext)
            .help("Next")
            .accessibilityIdentifier("transport-next")
            HStack(spacing: 4) {
                Image(systemName: "speaker.fill").font(.system(size: 9)).foregroundStyle(.secondary)
                VolumeSlider(transport: transport).frame(width: 84)
                Image(systemName: "speaker.wave.3.fill").font(.system(size: 9)).foregroundStyle(.secondary)
            }
            .padding(.leading, 6)
        }
        ToolbarItem(placement: .principal) {
            TransportDisplay(transport: transport, artwork: model.artwork)
        }
        ToolbarItemGroup(placement: .primaryAction) {
            Toggle(isOn: Binding(get: { transport.shuffle }, set: { transport.setShuffle($0) })) {
                Label("Shuffle", systemImage: "shuffle")
            }
            .disabled(!transport.canChangeModes)
            .help(transport.shuffle ? "Shuffle on" : "Shuffle off")
            .accessibilityValue(transport.shuffle ? "on" : "off")
            .accessibilityIdentifier("transport-shuffle")
            Button(action: transport.cycleRepeat) {
                Label(repeatTitle, systemImage: transport.repeat == .one ? "repeat.1" : "repeat")
                    .foregroundStyle(transport.repeat == .off ? Color.secondary : Color.accentColor)
            }
            .disabled(!transport.canChangeModes)
            .help(repeatTitle)
            .accessibilityValue(transport.repeat.rawValue)
            .accessibilityIdentifier("transport-repeat")
            filterField
        }
    }

    private var repeatTitle: String {
        switch transport.repeat {
        case .off: "Repeat Off"
        case .all: "Repeat All"
        case .one: "Repeat One"
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
        } else if let list = currentList {
            TrackListView(list: list, player: model, navigation: navigation)
        } else {
            DestinationPlaceholder(destination: navigation.destination)
        }
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
