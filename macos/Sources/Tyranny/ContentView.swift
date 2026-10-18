import SwiftUI
import AppKit

struct ContentView: View {
    @Bindable var model: PlayerModel
    @State private var showingConnection = false
    @State private var showingRoots = false
    @State private var showingPlaylistPicker = false
    @State private var showingSmartEditor = false
    @State private var seekPreview: Double?
    @State private var serverAction: String?

    var body: some View {
        VStack(spacing: 0) {
            connectionStatus
            if let message = model.message {
                notice(message, identifier: "operation-message") { model.message = nil }
            }
            if let error = model.snapshot?.error {
                notice("Playback: \(error)", identifier: "playback-error")
            }
            if let warning = model.snapshot?.now_playing?.metadata_error {
                notice("Track metadata: \(warning)", identifier: "metadata-warning")
            }
            if model.connected {
                scanStatus
                libraryHeader
                if !model.showQueue && !model.showPlaylists, let error = model.libraryFilterError {
                    Text("Filter: \(error) Edit or clear the query; see Filter syntax for examples.")
                        .foregroundStyle(.orange).textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading).padding(.horizontal, 10).padding(.bottom, 8)
                        .accessibilityIdentifier("library-filter-error")
                }
                if model.showQueue {
                    queuePane
                } else if model.showPlaylists {
                    PlaylistView(model: model)
                } else if model.snapshot?.library.roots == 0 {
                    onboarding
                } else {
                    libraryPanes
                }
            } else {
                ContentUnavailableView {
                    Label(model.connection == "connecting" ? "Connecting to Tyranny" : "Server Disconnected",
                          systemImage: "network")
                } description: {
                    Text("The server owns playback and your library. Connect to browse and control it.")
                } actions: {
                    Button("Connection Settings…") { showingConnection = true }
                    Button("Retry", action: model.reconnect)
                        .disabled(model.connection == "connecting" || model.serverManagementBusy)
                        .accessibilityIdentifier("disconnected-retry")
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            Divider()
            transport
        }
        .frame(minWidth: 900, minHeight: 600)
        .toolbar {
            ToolbarItemGroup {
                Button(action: model.chooseFolder) {
                    Label("Choose Music Folder", systemImage: "folder.badge.plus")
                }
                .disabled(!model.canMutate || model.scanActive)
                .accessibilityIdentifier("choose-folder")
                Button { showingRoots = true } label: {
                    Label("Music Folders", systemImage: "folder")
                }
                .disabled(!model.connected)
                .accessibilityIdentifier("show-roots")
                Toggle(isOn: $model.showQueue) {
                    Label("Queue", systemImage: "list.bullet")
                }
                .accessibilityIdentifier("toggle-queue")
                .trackDragHover(model: model, destination: .queue) { model.showQueue = true; model.showPlaylists = false }
                .help("While dragging, hover to open the queue without releasing.")
                Toggle(isOn: $model.showPlaylists) {
                    Label("Playlists", systemImage: "music.note.list")
                }
                .accessibilityIdentifier("toggle-playlists")
                .trackDragHover(model: model) { model.showPlaylists = true; model.showQueue = false }
                .help("While dragging, hover to choose a saved playlist without releasing.")
                Button { showingConnection = true } label: {
                    Label("Connection Settings", systemImage: "network")
                }
                .accessibilityIdentifier("connection-settings")
            }
        }
        .sheet(isPresented: $showingConnection) { connectionSettings }
        .sheet(isPresented: $showingRoots) { rootsSheet }
        .sheet(isPresented: $showingPlaylistPicker) { AddToPlaylistSheet(model: model) }
        .sheet(isPresented: $showingSmartEditor) { SmartPlaylistEditor(model: model, playlist: nil, initialQuery: model.filter) }
        .onChange(of: model.showQueue) { _, showing in if showing { model.showPlaylists = false } }
        .onChange(of: model.showPlaylists) { _, showing in if showing { model.showQueue = false } }
        .onChange(of: model.playlistFilter) { _, _ in model.playlistFilterChanged() }
        .onChange(of: model.selectedPlaylist) { _, _ in model.playlistSelectionChanged() }
        .onChange(of: model.selectedArtist) { _, _ in model.selectionChanged() }
        .onChange(of: model.selectedAlbum) { _, _ in model.selectionChanged() }
        .onChange(of: model.snapshot?.current_id) { _, _ in seekPreview = nil }
        .onChange(of: model.playbackSession) { _, _ in seekPreview = nil }
        .onChange(of: model.canMutate) { _, enabled in
            if !enabled { seekPreview = nil }
        }
    }

    private var connectionStatus: some View {
        HStack {
            if model.connection == "connecting" {
                ProgressView().controlSize(.small)
            } else {
                Image(systemName: model.connected ? "checkmark.circle.fill" : "exclamationmark.circle")
                    .foregroundStyle(model.connected ? Color.green : Color.orange)
            }
            Text(model.connection.capitalized).fontWeight(.medium)
                .accessibilityIdentifier("connection-state")
            Text(model.connectionDetail).foregroundStyle(.secondary).lineLimit(2)
                .textSelection(.enabled)
            Spacer()
            if model.loading { ProgressView().controlSize(.small).help("Loading server data") }
            if model.busy { Text("Applying…").foregroundStyle(.secondary) }
            Button("Retry", action: model.reconnect)
                .disabled(model.connection == "connecting" || model.busy || model.serverManagementBusy)
                .accessibilityIdentifier("reconnect")
        }
        .font(.callout)
        .padding(.horizontal).padding(.vertical, 8)
        .background(.bar)
    }

    private func notice(_ text: String, identifier: String, dismiss: (() -> Void)? = nil) -> some View {
        HStack(alignment: .top) {
            Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange)
            Text(text).textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading)
            if let dismiss {
                Button("Dismiss", action: dismiss)
            }
        }
        .font(.callout).padding(10)
        .background(Color.orange.opacity(0.10))
        .accessibilityIdentifier(identifier)
    }

    @ViewBuilder private var scanStatus: some View {
        if let scan = model.snapshot?.library.scan, scan.phase != "idle" {
            VStack(alignment: .leading, spacing: 5) {
                HStack {
                    Text("Scan: \(scan.phase.capitalized)").fontWeight(.medium)
                    Spacer()
                    if !model.scanActive {
                        Button("Review Music Folders…") { showingRoots = true }
                    }
                }
                if scan.phase == "discovering" {
                    HStack {
                        ProgressView().controlSize(.small)
                        Text("Discovering files — \(scan.discovered) found")
                    }
                } else if scan.phase == "indexing" {
                    if let total = scan.total, total > 0 {
                        ProgressView(value: Double(scan.processed), total: Double(total))
                        Text("Indexed \(scan.processed) of \(total)")
                    } else {
                        HStack {
                            ProgressView().controlSize(.small)
                            Text("Indexing — \(scan.processed) processed")
                        }
                    }
                }
                Text("\(scan.updated) updated · \(scan.unchanged) unchanged · \(scan.failures) failed · \(scan.skipped) skipped")
                    .foregroundStyle(.secondary)
                if let error = scan.last_error {
                    Text(error).foregroundStyle(.orange).textSelection(.enabled)
                }
            }
            .font(.callout).padding(10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(.quaternary)
            .accessibilityIdentifier("scan-status")
        }
    }

    private var libraryHeader: some View {
        HStack {
            Text(model.showQueue ? "Playback Queue" : model.showPlaylists ? "Saved Playlists" : "Music Library").font(.headline)
            Spacer()
            if !model.showQueue && !model.showPlaylists {
                TextField("Filter library", text: $model.filter)
                    .textFieldStyle(.roundedBorder).frame(maxWidth: 300)
                    .accessibilityIdentifier("library-filter")
                    .help("Filter title, artist or album with phrases, AND (&), OR (|), NOT (!), parentheses and field lists.")
                TrackFilterHelp(identifier: "library-filter-help")
                Button("Save as Smart Playlist…") { showingSmartEditor = true }
                    .disabled(!model.canMutate)
                    .accessibilityIdentifier("library-save-smart")
            }
            Button("Refresh", action: model.refresh)
                .disabled(model.loading || !model.connected)
                .accessibilityIdentifier("refresh")
        }.padding(10)
    }

    private var onboarding: some View {
        ContentUnavailableView {
            Label("Add Your Music", systemImage: "music.note.house")
        } description: {
            Text("Choose a music folder to index on the server. Files stay in place, and playback continues when this app closes.")
        } actions: {
            Button("Choose Music Folder…", action: model.chooseFolder)
                .buttonStyle(.borderedProminent)
                .disabled(!model.canMutate || model.scanActive)
                .accessibilityIdentifier("onboarding-choose-folder")
        }.frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private var libraryPanes: some View {
        HSplitView {
            VStack(spacing: 0) {
                paneTitle("Album Artists")
                List(selection: $model.selectedArtist) {
                    ForEach(model.artists?.entries ?? []) { artist in
                        VStack(alignment: .leading) {
                            Text(artist.name)
                            Text("\(artist.album_count) albums").font(.caption).foregroundStyle(.secondary)
                        }.tag(artist.id)
                    }
                }
                .accessibilityIdentifier("artist-list")
                .overlay { if model.artists?.entries.isEmpty == true { emptyLabel("No matching artists") } }
                pageControls(model.artists, prefix: "artists", action: model.pageArtists)
            }.frame(minWidth: 170, idealWidth: 210, maxWidth: 280)
            VStack(spacing: 0) {
                paneTitle("Albums")
                List(selection: $model.selectedAlbum) {
                    ForEach(model.albums?.entries ?? []) { album in
                        HStack(spacing: 8) {
                            ArtworkView(
                                store: model.artwork,
                                request: model.albums?.revision == model.snapshot?.library.revision
                                    ? model.artwork.albumRequest(album.id) : nil,
                                identifier: "album-art-\(album.id)", dimension: 48, tracksVisibility: true
                            )
                            VStack(alignment: .leading) {
                                Text(album.title)
                                Text(album.album_artist).font(.caption).foregroundStyle(.secondary)
                                Text("\(album.track_count) tracks · \(album.disc_count) discs\(album.compilation ? " · Compilation" : "")")
                                    .font(.caption2).foregroundStyle(.secondary)
                            }
                        }.tag(album.id)
                    }
                }
                .accessibilityIdentifier("album-list")
                .overlay { if model.albums?.entries.isEmpty == true { emptyLabel("No matching albums") } }
                pageControls(model.albums, prefix: "albums", action: model.pageAlbums)
            }.frame(minWidth: 190, idealWidth: 250, maxWidth: 320)
            tracksPane.frame(minWidth: 420, maxWidth: .infinity)
        }
        .disabled(!model.libraryReady)
    }

    private var tracksPane: some View {
        VStack(spacing: 0) {
            paneTitle("Tracks")
            DragTrackTable(model: model, source: .library, identifier: "track-table",
                columns: [.init(title: "Disc", width: 40), .init(title: "#", width: 40),
                    .init(title: "Title", width: 230), .init(title: "Artist", width: 150), .init(title: "Time", width: 65)],
                rows: (model.tracks?.entries ?? []).map { track in
                    .init(id: track.id, cells: [track.metadata.disc_number.map(String.init) ?? "—",
                        track.metadata.track_number.map(String.init) ?? "—",
                        track.metadata.title + (track.error.map { " — \($0)" } ?? ""),
                        track.metadata.artist, track.metadata.duration_ms.map { time(Double($0) / 1000) } ?? "—"],
                        help: track.path)
                }, selection: $model.selectedTracks, activate: model.playLibraryTrack)
            .allowsHitTesting(model.libraryReady)
            .overlay {
                if model.selectedArtist != nil && model.selectedAlbum == nil {
                    emptyLabel("Choose an album")
                } else if model.tracks?.entries.isEmpty == true {
                    emptyLabel("No matching tracks")
                }
            }
            HStack {
                Text("\(model.selectedTracks.count) selected").font(.caption).foregroundStyle(.secondary)
                Spacer()
                Button("Add to Playlist…") { showingPlaylistPicker = true }
                    .accessibilityIdentifier("add-to-playlist")
                Button("Enqueue") { model.enqueueSelection(play: false) }
                    .accessibilityIdentifier("enqueue")
                Button("Play Selection") { model.enqueueSelection(play: true) }
                    .accessibilityIdentifier("play-selection")
            }
            .disabled(!model.canMutate || !model.libraryReady || model.selectedTracks.isEmpty)
            .padding(8)
            pageControls(model.tracks, prefix: "tracks", action: model.pageTracks)
        }
    }

    private var queuePane: some View {
        VStack(spacing: 0) {
            DragTrackTable(model: model, source: .queue, identifier: "queue-table",
                columns: [.init(title: "Playing", width: 95), .init(title: "Track", width: 240),
                    .init(title: "Path", width: 350), .init(title: "Entry", width: 65)],
                rows: (model.queue?.entries ?? []).map { item in
                    .init(id: item.id, cells: [
                        item.id == model.snapshot?.current_id ? model.snapshot?.transport.capitalized ?? "Current" : "",
                        URL(fileURLWithPath: item.path).lastPathComponent, item.path, String(item.id)], help: item.path)
                }, selection: $model.selectedQueue, activate: model.playQueueEntry)
            .overlay { if model.queue?.entries.isEmpty == true { emptyLabel("Queue is empty — select library tracks to enqueue") } }
            HStack {
                Text("\(model.selectedQueue.count) selected").font(.caption).foregroundStyle(.secondary)
                Button("Play", action: model.playQueueSelection)
                    .disabled(!model.canMutate || model.selectedQueue.count != 1)
                    .accessibilityIdentifier("queue-play")
                Button("Remove", action: model.removeQueueSelection)
                    .disabled(!model.canMutate || model.selectedQueue.isEmpty)
                    .accessibilityIdentifier("queue-remove")
                Button("Move Up") { model.moveQueueSelection(-1) }
                    .disabled(!model.canMutate || !model.canMoveQueue(-1))
                    .accessibilityIdentifier("queue-up")
                Button("Move Down") { model.moveQueueSelection(1) }
                    .disabled(!model.canMutate || !model.canMoveQueue(1))
                    .accessibilityIdentifier("queue-down")
                Spacer()
                Button("Clear Queue", action: model.clearQueue)
                    .disabled(!model.canMutate || (model.snapshot?.queue_length ?? 0) == 0)
                    .accessibilityIdentifier("queue-clear")
            }.padding(10)
            pageControls(model.queue, prefix: "queue", action: model.pageQueue)
        }
    }

    private var transport: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                ArtworkView(
                    store: model.artwork, request: model.artwork.nowPlayingRequest,
                    identifier: "now-playing-art", dimension: 128
                )
                Button("Previous", systemImage: "backward.end.fill", action: model.previous)
                    .disabled(!model.canMutate || model.snapshot?.can_previous != true)
                    .accessibilityIdentifier("transport-previous")
                Button(action: model.playPause) {
                    Label(["playing", "buffering"].contains(model.snapshot?.transport ?? "") ? "Pause" : "Play",
                          systemImage: ["playing", "buffering"].contains(model.snapshot?.transport ?? "") ? "pause.fill" : "play.fill")
                }
                .disabled(!model.canMutate || (model.snapshot?.queue_length ?? 0) == 0)
                .accessibilityIdentifier("transport-play")
                Button("Next", systemImage: "forward.end.fill", action: model.next)
                    .disabled(!model.canMutate || model.snapshot?.can_next != true)
                    .accessibilityIdentifier("transport-next")
                Button("Stop", systemImage: "stop.fill", action: model.stop)
                    .disabled(!model.canMutate)
                    .accessibilityIdentifier("transport-stop")
                VStack(alignment: .leading) {
                    Text(model.currentName).lineLimit(1).help(model.currentName)
                        .accessibilityIdentifier("now-playing-title")
                    Text(model.connected ? (model.snapshot?.transport.capitalized ?? "No state") : "Not connected")
                        .font(.caption).foregroundStyle(.secondary)
                }.frame(maxWidth: .infinity, alignment: .leading)
                Text(time(seekPreview ?? model.positionSeconds) + (model.durationSeconds.map { " / " + time($0) } ?? " / —"))
                    .monospacedDigit().accessibilityIdentifier("transport-position")
            }
            NativeSeekSlider(model: model, preview: $seekPreview)
                .frame(height: 20)
            HStack {
                Picker("ReplayGain", selection: Binding(
                    get: { model.snapshot?.settings.gain_mode ?? "off" },
                    set: { model.setGain($0) }
                )) {
                    Text("Off").tag("off")
                    Text("Track").tag("track")
                    Text("Album").tag("album")
                }
                .frame(width: 210).accessibilityIdentifier("gain-mode")
                Toggle("Resume when server starts", isOn: Binding(
                    get: { model.snapshot?.settings.resume_on_start ?? false },
                    set: { model.setResume($0) }
                )).accessibilityIdentifier("resume-on-start")
                Spacer()
                if let underruns = model.snapshot?.underruns, underruns > 0 {
                    Text("Underruns: \(underruns)").font(.caption).foregroundStyle(.secondary)
                }
            }.disabled(!model.canMutate)
        }
        .padding(12).background(.bar)
    }


    private var connectionSettings: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Server Connection").font(.title2)
            Text("Connect to a local Tyranny server. The token file is read when connecting; its contents are never displayed.")
                .foregroundStyle(.secondary)
            Form {
                TextField("WebSocket URL", text: $model.endpoint)
                    .accessibilityIdentifier("connection-url")
                TextField("Token file", text: $model.tokenPath)
                    .accessibilityIdentifier("connection-token-path")
            }.disabled(model.serverManagementBusy)
            Text("Use a literal loopback address, for example ws://127.0.0.1:9865/ws.")
                .font(.caption).foregroundStyle(.secondary)
            Text(model.connectionDetail).textSelection(.enabled)
            if model.managesServer {
                Divider()
                Text("Local Background Server").font(.headline)
                Text("Runs independently of this window and at login. Stop removes its login job; opening the app again starts it.")
                    .font(.caption).foregroundStyle(.secondary)
                if let path = model.managedLogPath {
                    Text("Log: \(path)").font(.caption).textSelection(.enabled)
                }
                HStack {
                    Button("Start", action: model.startManagedServer)
                        .accessibilityIdentifier("managed-start")
                    Button("Restart…") { serverAction = "restart" }
                        .accessibilityIdentifier("managed-restart")
                    Button("Stop…") { serverAction = "stop" }
                        .accessibilityIdentifier("managed-stop")
                    if model.serverManagementBusy { ProgressView().controlSize(.small) }
                }.disabled(model.serverManagementBusy || model.busy || model.connection == "connecting")
            }
            HStack {
                Button("Disconnect", action: model.disconnect)
                    .disabled(model.connection == "disconnected" || model.serverManagementBusy)
                    .accessibilityIdentifier("connection-disconnect")
                Spacer()
                Button("Done") { showingConnection = false }
                    .keyboardShortcut(.cancelAction)
                Button("Connect / Retry", action: model.reconnect)
                    .disabled(model.connection == "connecting" || model.busy || model.serverManagementBusy)
                    .keyboardShortcut(.defaultAction)
                    .accessibilityIdentifier("connection-connect")
            }
        }.padding(24).frame(width: 550)
        .confirmationDialog(
            serverAction == "restart" ? "Restart the background server?" : "Stop the background server?",
            isPresented: Binding(get: { serverAction != nil }, set: { if !$0 { serverAction = nil } })
        ) {
            if serverAction == "restart" {
                Button("Restart Server", role: .destructive, action: model.restartManagedServer)
            } else {
                Button("Stop Server", role: .destructive, action: model.stopManagedServer)
            }
        } message: {
            Text("Playback will stop. Your library, queue, and settings are retained.")
        }
    }

    private var rootsSheet: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Music Folders").font(.title2)
            Text("Folders must be readable by the server. Rescan to import changes or retry failed and interrupted scans.")
                .foregroundStyle(.secondary)
            List(model.roots?.entries ?? []) { root in
                HStack(alignment: .top) {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(root.path).textSelection(.enabled)
                        if let scan = root.last_scan {
                            Text("\(scan.phase.capitalized) · \(scan.processed) processed · \(scan.failures) failed")
                                .font(.caption).foregroundStyle(.secondary)
                            if let error = scan.last_error {
                                Text(error).font(.caption).foregroundStyle(.orange).textSelection(.enabled)
                            }
                        }
                    }
                    Spacer()
                    Button("Rescan") { model.rescan(root.id) }
                        .disabled(!model.canMutate || model.scanActive)
                        .accessibilityIdentifier("rescan-root-\(root.id)")
                }.padding(.vertical, 4)
            }.accessibilityIdentifier("roots-list")
            pageControls(model.roots, prefix: "roots", action: model.pageRoots)
            HStack {
                Button("Choose Music Folder…", action: model.chooseFolder)
                    .disabled(!model.canMutate || model.scanActive)
                    .accessibilityIdentifier("roots-choose-folder")
                Spacer()
                Button("Done") { showingRoots = false }.keyboardShortcut(.cancelAction)
            }
        }.padding(20).frame(width: 650, height: 430)
    }

    private func paneTitle(_ title: String) -> some View {
        Text(title).font(.headline).frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 10).padding(.vertical, 8)
    }

    private func emptyLabel(_ text: String) -> some View {
        Text(text).font(.callout).foregroundStyle(.secondary)
            .multilineTextAlignment(.center).padding().allowsHitTesting(false)
    }

    private func pageControls<T: Codable & Sendable & Equatable>(_ page: Page<T>?, prefix: String, action: @escaping (Int) -> Void) -> some View {
        HStack(spacing: 8) {
            Button { action(-1) } label: { Image(systemName: "chevron.left") }
                .help("Previous page").accessibilityLabel("Previous \(prefix) page")
                .accessibilityIdentifier("\(prefix)-previous")
                .disabled(!model.connected || model.loading || (page?.offset ?? 0) == 0)
                .trackDragHover(model: model, enabled: prefix == "queue" && !model.loading && (page?.offset ?? 0) > 0,
                    destination: .queue) { action(-1) }
            Spacer(minLength: 0)
            if let page {
                Text(page.entries.isEmpty ? "0 of \(page.total)" : "\(page.offset + 1)–\(page.offset + page.entries.count) of \(page.total)")
                    .font(.caption).monospacedDigit()
            } else {
                Text("Not loaded").font(.caption).foregroundStyle(.secondary)
            }
            Spacer(minLength: 0)
            Button { action(1) } label: { Image(systemName: "chevron.right") }
                .help("Next page").accessibilityLabel("Next \(prefix) page")
                .accessibilityIdentifier("\(prefix)-next")
                .disabled(!model.connected || model.loading || page == nil || (page.map { $0.offset + $0.entries.count >= $0.total } ?? true))
                .trackDragHover(model: model, enabled: prefix == "queue" && !model.loading
                    && (page.map { $0.offset + $0.entries.count < $0.total } ?? false),
                    destination: .queue) { action(1) }
        }.padding(8).background(.bar)
    }

    private func time(_ seconds: Double) -> String {
        guard seconds.isFinite, seconds >= 0, seconds < Double(Int.max) else { return "—" }
        let value = Int(seconds)
        if value >= 3600 {
            return String(format: "%d:%02d:%02d", value / 3600, (value / 60) % 60, value % 60)
        }
        return String(format: "%d:%02d", value / 60, value % 60)
    }
}

struct TrackFilterHelp: View {
    let identifier: String
    @State private var showing = false

    var body: some View {
        Button("Filter syntax", systemImage: "line.3.horizontal.decrease.circle") { showing = true }
            .labelStyle(.iconOnly)
            .accessibilityIdentifier(identifier)
            .help("Filter syntax and examples")
            .popover(isPresented: $showing) {
                VStack(alignment: .leading, spacing: 10) {
                    Text("Live Track Filters").font(.headline)
                    Text("Use title:, artist: (track artist), or album:. Text matches literal substrings, ignoring case. Bare words search all three fields; quote a phrase.")
                    Text(#"artist:"Miles Davis" & !album:"Kind of Blue""#).font(.system(.callout, design: .monospaced))
                    Text(#"(artist:"Miles Davis" | artist:"Ornette Coleman") & !album:"Kind of Blue""#)
                        .font(.system(.callout, design: .monospaced))
                    Text(#"artist:["Miles Davis", "Ornette Coleman"]"#).font(.system(.callout, design: .monospaced))
                    Text("Use ! or NOT, then & or AND, then | or OR in order of precedence. Parentheses group expressions. Adjacent terms mean AND. Word operators must be uppercase. Lists mean OR; !artist:[…] excludes every listed artist.")
                    Text(#"Values cannot be empty. Inside quotes, use \" for a quote and \\ for a backslash. Filters are limited to 1,024 UTF-8 bytes. Clear the filter to show all tracks."#)
                        .foregroundStyle(.secondary)
                }
                .textSelection(.enabled).padding(18).frame(width: 480)
            }
    }
}

/// AppKit owns native mouse tracking, keyboard focus and accessibility. Continuous
/// mouse actions only preview; the tracking loop's return commits one occurrence-bound seek.
private struct NativeSeekSlider: NSViewRepresentable {
    var model: PlayerModel
    @Binding var preview: Double?

    func makeCoordinator() -> Coordinator { Coordinator(model: model, preview: $preview) }

    func makeNSView(context: Context) -> SeekSlider {
        let slider = SeekSlider()
        slider.minValue = 0
        slider.isContinuous = true
        slider.target = context.coordinator
        slider.action = #selector(Coordinator.changed(_:))
        slider.setAccessibilityIdentifier("seek-slider")
        slider.setAccessibilityLabel("Playback position in seconds")
        slider.beginTracking = { [weak coordinator = context.coordinator] in coordinator?.begin() }
        slider.endTracking = { [weak coordinator = context.coordinator] in coordinator?.end(slider: $0) }
        return slider
    }

    func updateNSView(_ slider: SeekSlider, context: Context) {
        context.coordinator.preview = $preview
        let duration = model.durationSeconds ?? 0
        slider.maxValue = max(duration, 1 / 48_000)
        slider.isEnabled = model.canMutate && model.snapshot?.current_id != nil && duration > 0
        context.coordinator.invalidateIfNeeded()
        if !context.coordinator.tracking || context.coordinator.occurrence == nil {
            slider.doubleValue = min(max(model.positionSeconds, 0), slider.maxValue)
        }
    }

    @MainActor final class Coordinator: NSObject {
        weak var model: PlayerModel?
        var preview: Binding<Double?>
        var tracking = false
        var occurrence: (entry: UInt64, session: UInt64)?

        init(model: PlayerModel, preview: Binding<Double?>) {
            self.model = model
            self.preview = preview
        }

        func begin() {
            tracking = true
            guard let model, model.canMutate, let entry = model.snapshot?.current_id,
                  (model.durationSeconds ?? 0) > 0 else { occurrence = nil; return }
            occurrence = (entry, model.playbackSession)
        }

        func invalidateIfNeeded() {
            guard let occurrence, let model else { return }
            if occurrence.entry != model.snapshot?.current_id
                || occurrence.session != model.playbackSession || !model.canMutate
                || (model.durationSeconds ?? 0) <= 0 {
                self.occurrence = nil
            }
        }

        @objc func changed(_ slider: NSSlider) {
            if tracking {
                invalidateIfNeeded()
                if occurrence != nil { preview.wrappedValue = slider.doubleValue }
            } else if let model, let entry = model.snapshot?.current_id {
                model.seek(seconds: slider.doubleValue, entryID: entry)
            }
        }

        func end(slider: NSSlider) {
            invalidateIfNeeded()
            let destination = occurrence
            occurrence = nil
            tracking = false
            preview.wrappedValue = nil
            if let destination {
                model?.seek(seconds: slider.doubleValue, entryID: destination.entry)
            }
        }
    }
}

private final class SeekSlider: NSSlider {
    var beginTracking: (() -> Void)?
    var endTracking: ((NSSlider) -> Void)?

    override func mouseDown(with event: NSEvent) {
        guard isEnabled else { return }
        beginTracking?()
        super.mouseDown(with: event)
        endTracking?(self)
    }

    override func setAccessibilityValue(_ value: Any?) {
        guard isEnabled, let number = value as? NSNumber, number.doubleValue.isFinite else { return }
        doubleValue = min(max(number.doubleValue, minValue), maxValue)
        sendAction(action, to: target)
    }

    override func accessibilityPerformIncrement() -> Bool { adjust(by: 5) }
    override func accessibilityPerformDecrement() -> Bool { adjust(by: -5) }

    private func adjust(by seconds: Double) -> Bool {
        guard isEnabled else { return false }
        doubleValue = min(max(doubleValue + seconds, minValue), maxValue)
        sendAction(action, to: target)
        return true
    }
}
