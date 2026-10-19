import AppKit
import Foundation
import Observation
import TyrannyCore

@MainActor @Observable
final class PlayerModel {
    var endpoint = "ws://127.0.0.1:9865/ws"
    var tokenPath = FileManager.default.homeDirectoryForCurrentUser
        .appendingPathComponent("Library/Application Support/tyranny/token").path
    private(set) var connection = "disconnected" {
        didSet { updateMediaControls(); updateArtwork() }
    }
    private(set) var connectionDetail = "Connecting to the local server."
    private(set) var snapshot: ControlState? {
        didSet { updateMediaControls(); updateArtwork() }
    }
    private(set) var playbackSession: UInt64 = 0
    let artwork = ArtworkStore()
    /// Transport commands and the toolbar display. Owns the system media controls.
    let transport = TransportModel()
    /// The Up Next view's and drawer's queue windows.
    let upNext = UpNextModel()
    /// The saved playlists in the source list, and the edits made to them.
    let savedPlaylists = PlaylistsModel()
    private(set) var artists: Page<AlbumArtist>?
    private(set) var albums: Page<Album>?
    private(set) var tracks: Page<Track>?
    private(set) var roots: Page<Root>?
    var selectedArtist: UInt64?
    var selectedAlbum: UInt64?
    var selectedTracks: Set<UInt64> = []
    var filter = "" {
        didSet { if oldValue != filter { filterChanged() } }
    }
    private(set) var libraryFilterError: String?
    private var loadedLibraryQuery: String?
    private var loadedLibraryEpoch: UInt64?
    private var loadedLibraryArtist: UInt64?
    private var loadedLibraryAlbum: UInt64?
    var message: String?
    var showPlaylists = false
    var playlistFilter = ""
    var playlistEntryFilter = "" {
        didSet { if oldValue != playlistEntryFilter { playlistEntryFilterChanged() } }
    }
    private(set) var playlistEntryFilterError: String?
    private var loadedEntryQuery: String?
    var selectedPlaylist: UInt64?
    private var retainedPlaylist: Playlist?
    var selectedPlaylistEntries: Set<UInt64> = []
    private(set) var playlists: Page<Playlist>?
    private(set) var playlistEntries: PlaylistEntries?
    private(set) var playlistsLoading = false
    private(set) var playlistEntriesLoading = false
    private var playlistCatalogError: String?
    private var playlistEntryError: String?
    var playlistError: String? { playlistCatalogError ?? playlistEntryError }
    @ObservationIgnored private var playlistsTask: Task<Void, Never>?
    @ObservationIgnored private var entriesTask: Task<Void, Never>?
    @ObservationIgnored private var entriesTimer: Task<Void, Never>?
    @ObservationIgnored private var entryFilterDeadline = Date.distantPast
    @ObservationIgnored private var entryFilterEpoch: UInt64 = 0
    @ObservationIgnored private var playlistsEpoch: UInt64 = 0
    @ObservationIgnored private var entriesEpoch: UInt64 = 0
    @ObservationIgnored private var playlistOffset = 0
    @ObservationIgnored private var entryOffset = 0
    @ObservationIgnored private var needsPlaylists = false
    @ObservationIgnored private var needsEntries = false
    @ObservationIgnored private var playlistSession: UInt64 = 0
    private(set) var busy = false {
        didSet { updateMediaControls() }
    }
    private(set) var loading = false
    private(set) var startupFailed = false
    private(set) var serverManagementBusy = false {
        didSet { updateMediaControls() }
    }
    private(set) var managedEnabled = true
    private var managedServer: ManagedServer?
    @ObservationIgnored private var configurationError: String?

    @ObservationIgnored private let wire = WireClient()
    @ObservationIgnored private let decoder = JSONDecoder()
    @ObservationIgnored private var wantsConnection = false
    @ObservationIgnored private var connectionTask: Task<Void, Never>?
    @ObservationIgnored private var libraryTask: Task<Void, Never>?
    @ObservationIgnored private var libraryTimer: Task<Void, Never>?
    @ObservationIgnored private var needsLibrary = false
    @ObservationIgnored private var session: UInt64 = 0
    private var queryEpoch: UInt64 = 0
    @ObservationIgnored private var libraryFilterDeadline = Date.distantPast
    @ObservationIgnored private var artistOffset = 0
    @ObservationIgnored private var albumOffset = 0
    @ObservationIgnored private var trackOffset = 0
    @ObservationIgnored private var rootOffset = 0
    @ObservationIgnored private var previousArtist: UInt64?
    @ObservationIgnored private var previousAlbum: UInt64?
    @ObservationIgnored private var lastLibraryRefresh = Date.distantPast
    static let pageSize = 100
    @ObservationIgnored var activeDrag: TrackDrag?

    init() {
        let arguments = CommandLine.arguments
        for (index, argument) in arguments.enumerated() where index + 1 < arguments.count {
            if argument == "--url" { endpoint = arguments[index + 1] }
            if argument == "--token-file" { tokenPath = arguments[index + 1] }
        }
        do {
            managedServer = try ManagedServer.make(arguments: arguments)
            if let managedServer {
                endpoint = managedServer.endpoint
                tokenPath = managedServer.tokenPath
            }
        } catch {
            startupFailed = true
            message = error.localizedDescription
            configurationError = error.localizedDescription
        }
        wire.onSnapshot = { [weak self] state, instance in self?.accept(state, newInstance: instance != nil) }
        wire.onDisconnect = { [weak self] reason in self?.lostConnection(reason) }
        transport.attach(self)
        upNext.attach(self)
        savedPlaylists.attach(self)
        updateMediaControls()
    }

    var connected: Bool { connection == "connected" }
    var playlistConnectionGeneration: UInt64 { playlistSession }
    var canMutate: Bool { connected && !busy && !serverManagementBusy }
    var libraryReady: Bool {
        guard connected, libraryFilterError == nil, loadedLibraryQuery == filter,
              loadedLibraryEpoch == queryEpoch, loadedLibraryArtist == selectedArtist,
              loadedLibraryAlbum == selectedAlbum, let revision = snapshot?.library.revision,
              artists?.revision == revision, albums?.revision == revision else { return false }
        return tracks?.revision == revision || (selectedArtist != nil && selectedAlbum == nil)
    }
    var playlistEntryFilterActive: Bool {
        !playlistEntryFilter.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }
    var managesServer: Bool {
        guard let managedServer else { return false }
        return endpoint == managedServer.endpoint && tokenPath == managedServer.tokenPath
    }
    var managedLabel: String? { managesServer ? managedServer?.label : nil }
    var managedLogPath: String? { managesServer ? managedServer?.logPath : nil }
    var scanActive: Bool { ["discovering", "indexing"].contains(snapshot?.library.scan.phase ?? "") }
    var positionSeconds: Double {
        guard let state = snapshot, state.sample_rate > 0 else { return 0 }
        return Double(state.position_frames) / Double(state.sample_rate)
    }
    var currentName: String {
        guard let playing = snapshot?.now_playing else { return "Nothing playing" }
        return playing.artist.isEmpty ? playing.title : "\(playing.title) — \(playing.artist)"
    }
    var durationSeconds: Double? {
        guard let state = snapshot, state.sample_rate > 0,
              let frames = state.now_playing?.duration_frames else { return nil }
        return Double(frames) / Double(state.sample_rate)
    }

    private func updateMediaControls() {
        transport.stateChanged()
        upNext.sync()
        savedPlaylists.sync()
    }

    private func updateArtwork(invalidate: Bool = false) {
        artwork.synchronize(
            endpoint: endpoint, tokenPath: tokenPath,
            instance: connected ? wire.instanceID : nil,
            revision: connected ? snapshot?.library.revision : nil,
            entryID: connected ? snapshot?.now_playing?.entry_id : nil,
            invalidate: invalidate
        )
    }

    func shutdown() {
        transport.shutdown()
        disconnect()
    }

    func start() {
        guard !wantsConnection else { return }
        wantsConnection = true
        connectLoop()
    }
    func reconnect() {
        guard !serverManagementBusy else { return }
        disconnect()
        wantsConnection = true
        connectLoop()
    }
    func startManagedServer() {
        guard managesServer, !serverManagementBusy else { return }
        managedEnabled = true
        reconnect()
    }
    func restartManagedServer() { manageServer(restart: true) }
    func stopManagedServer() { manageServer(restart: false) }
    private func manageServer(restart: Bool) {
        guard managesServer, let managedServer, !serverManagementBusy, !busy else { return }
        disconnect()
        message = nil
        serverManagementBusy = true
        Task {
            defer { serverManagementBusy = false }
            do {
                if restart {
                    try await managedServer.restart()
                    managedEnabled = true
                    serverManagementBusy = false
                    start()
                } else {
                    try await managedServer.uninstall()
                    managedEnabled = false
                    connectionDetail = "Background job removed; data retained. Manually started servers are not stopped."
                }
            } catch {
                message = error.localizedDescription
                connectionDetail = error.localizedDescription
            }
        }
    }
    func disconnect() {
        wantsConnection = false
        session &+= 1
        connectionTask?.cancel()
        connectionTask = nil
        libraryTimer?.cancel()
        wire.disconnect()
        clearRemoteState()
        connection = "disconnected"
        connectionDetail = "Disconnected. The server continues playing independently."
        if busy { message = "Disconnected during a command. Its outcome is uncertain; reconnect and inspect state before repeating it." }
    }
    private func clearRemoteState() {
        libraryTimer?.cancel(); libraryTimer = nil
        entriesTimer?.cancel(); entriesTimer = nil
        libraryFilterDeadline = .distantPast; entryFilterDeadline = .distantPast
        loadedLibraryQuery = nil; loadedLibraryEpoch = nil; loadedEntryQuery = nil
        libraryFilterError = nil; playlistEntryFilterError = nil
        activeDrag = nil
        playbackSession &+= 1
        playlistsEpoch &+= 1; entriesEpoch &+= 1
        playlistSession &+= 1
        playlists = nil; playlistEntries = nil
        selectedPlaylistEntries = []
        playlistOffset = 0; entryOffset = 0
        needsPlaylists = false; needsEntries = false
        playlistCatalogError = nil; playlistEntryError = nil
        snapshot = nil
        artists = nil; albums = nil; tracks = nil; roots = nil
        selectedArtist = nil; selectedAlbum = nil
        previousArtist = nil; previousAlbum = nil
        selectedTracks = []
        artistOffset = 0; albumOffset = 0; trackOffset = 0; rootOffset = 0
        queryEpoch &+= 1
        needsLibrary = false
    }
    private func lostConnection(_ reason: String) {
        clearRemoteState()
        connection = "disconnected"
        connectionDetail = reason
        if wantsConnection { connectLoop() }
    }
    private func connectLoop() {
        guard connectionTask == nil, wantsConnection, !serverManagementBusy else { return }
        if let configurationError {
            startupFailed = true
            connectionDetail = configurationError
            wantsConnection = false
            return
        }
        startupFailed = false
        let generation = session
        connectionTask = Task { [weak self] in
            guard let self else { return }
            var delay: UInt64 = 1
            var attemptedBootstrap = false
            while wantsConnection, generation == session, !Task.isCancelled {
                connection = "connecting"
                connectionDetail = "Connecting to \(endpoint)…"
                do {
                    try await wire.connect(endpoint: endpoint, tokenPath: tokenPath)
                    guard generation == session, wantsConnection else { return }
                    connection = "connected"
                    connectionDetail = endpoint
                    connectionTask = nil
                    refresh()
                    return
                } catch {
                    guard generation == session, wantsConnection, !Task.isCancelled else { return }
                    if !attemptedBootstrap, managedEnabled, managesServer,
                       let failure = error as? ServerFailure,
                       ["token_read_failed", "connection_lost"].contains(failure.code),
                       let managedServer {
                        attemptedBootstrap = true
                        connectionDetail = "Starting the local background server…"
                        do {
                            try await managedServer.ensureRunning()
                            guard generation == session, wantsConnection, !Task.isCancelled else { return }
                            continue
                        } catch {
                            guard generation == session, wantsConnection, !Task.isCancelled else { return }
                            startupFailed = true
                            connection = "disconnected"
                            connectionDetail = error.localizedDescription
                            message = error.localizedDescription
                            wantsConnection = false
                            connectionTask = nil
                            return
                        }
                    }
                    connection = "disconnected"
                    connectionDetail = "\(error.localizedDescription) Retrying in \(delay)s."
                    do { try await Task.sleep(for: .seconds(delay)) } catch { return }
                    delay = min(delay * 2, 10)
                }
            }
        }
    }
    private func accept(_ state: ControlState, newInstance: Bool) {
        if !newInstance, let previous = snapshot, state.sequence < previous.sequence { return }
        let previous = snapshot
        snapshot = state
        if newInstance || previous?.current_id != state.current_id { playbackSession &+= 1 }
        // A rescan that changes only cover files keeps the library revision, so
        // invalidate artwork when a scan finishes, not only on revision change.
        if let previous, previous.library.revision == state.library.revision,
           (["discovering", "indexing"].contains(previous.library.scan.phase)
                && !["discovering", "indexing"].contains(state.library.scan.phase))
            || (previous.library.scan.phase != "complete" && state.library.scan.phase == "complete") {
            updateArtwork(invalidate: true)
        }
        guard connected else { return }
        if previous?.playlists_revision != state.playlists_revision {
            if currentPlaylist?.isSmart == true { selectedPlaylistEntries = [] }
            requestPlaylists()
            requestPlaylistEntries()
        } else if previous?.library.revision != state.library.revision {
            selectedPlaylistEntries = []
            playlistEntries = nil
            requestPlaylistEntries()
        }
        if previous?.library.revision != state.library.revision
            || previous?.library.scan.phase != state.library.scan.phase {
            let delay = scanActive ? max(0, 1 - Date().timeIntervalSince(lastLibraryRefresh)) : 0
            requestLibrary(after: delay)
        }
    }

    private func request<T: Decodable>(_ command: String, _ fields: [String: JSONValue] = [:], as type: T.Type = T.self) async throws -> T {
        try decoder.decode(type, from: await wire.request(command, fields: fields))
    }
    /// Sends one command on the shared connection and decodes its reply.
    func send<T: Decodable>(_ command: String, _ fields: [String: JSONValue] = [:]) async throws -> T {
        try await request(command, fields)
    }
    /// Refetches the snapshot after a `stale_revision` reply, so a retry uses the current revisions.
    func refreshSnapshot() async throws {
        let fresh: SnapshotReply = try await request("snapshot")
        accept(fresh.snapshot, newInstance: false)
    }
    /// Up Next has entries after the current one (spec 5.1). True when the loaded queue page cannot tell.
    var hasUpcoming: Bool {
        guard let state = snapshot else { return false }
        return UpNext.hasUpcoming(queueLength: state.queue_length, currentID: state.current_id,
                                  queueRevision: state.queue_revision, page: upNext.loadedPage)
    }
    private func pageFields(offset: Int, revision: UInt64?, query: String) -> [String: JSONValue] {
        ["offset": .int(offset), "limit": .int(Self.pageSize),
         "revision": revision.map(JSONValue.uint) ?? .null, "query": .string(query)]
    }
    func refresh() {
        queryEpoch &+= 1
        requestLibrary(after: 0)
        requestPlaylists()
        requestPlaylistEntries()
    }
    func filterChanged() {
        artistOffset = 0; albumOffset = 0; trackOffset = 0
        selectedArtist = nil; selectedAlbum = nil; previousArtist = nil; previousAlbum = nil
        selectedTracks = []
        artists = nil; albums = nil; tracks = nil
        loadedLibraryQuery = nil; loadedLibraryEpoch = nil
        libraryFilterError = nil
        if activeDrag?.source == .library { activeDrag = nil }
        libraryFilterDeadline = Date().addingTimeInterval(0.25)
        queryEpoch &+= 1
        requestLibrary(after: 0.25)
    }
    func selectionChanged() {
        if previousArtist != selectedArtist {
            selectedAlbum = nil; selectedTracks = []
            albumOffset = 0; trackOffset = 0
        } else if previousAlbum != selectedAlbum {
            selectedTracks = []; trackOffset = 0
        } else { return }
        previousArtist = selectedArtist; previousAlbum = selectedAlbum
        queryEpoch &+= 1
        requestLibrary(after: 0)
    }
    private func requestLibrary(after delay: Double) {
        guard connected else { return }
        needsLibrary = true
        loading = true
        libraryTimer?.cancel()
        let wait = max(delay, libraryFilterDeadline.timeIntervalSinceNow)
        libraryTimer = Task { [weak self] in
            if wait > 0 { do { try await Task.sleep(for: .seconds(wait)) } catch { return } }
            guard !Task.isCancelled, let self else { return }
            libraryTimer = nil
            loadLibrary()
        }
    }
    private func loadLibrary() {
        guard libraryTask == nil, connected else { return }
        libraryTask = Task { [weak self] in
            guard let self else { return }
            loading = true
            defer { loading = libraryTimer != nil; libraryTask = nil }
            while needsLibrary, connected, libraryTimer == nil {
                needsLibrary = false
                let generation = session, epoch = queryEpoch
                let artist = selectedArtist, album = selectedAlbum, query = filter
                let offsets = (artistOffset, albumOffset, trackOffset, rootOffset)
                lastLibraryRefresh = Date()
                do {
                    let newArtists: Page<AlbumArtist> = try await request("library_artists", pageFields(offset: offsets.0, revision: offsets.0 > 0 ? artists?.revision : nil, query: query))
                    var albumFields = pageFields(offset: offsets.1, revision: offsets.1 > 0 ? albums?.revision : nil, query: query)
                    albumFields["artist_id"] = artist.map(JSONValue.uint) ?? .null
                    let newAlbums: Page<Album> = try await request("library_albums", albumFields)
                    var trackFields = pageFields(offset: offsets.2, revision: offsets.2 > 0 ? tracks?.revision : nil, query: query)
                    trackFields["album_id"] = album.map(JSONValue.uint) ?? .null
                    let newTracks: Page<Track>?
                    if artist == nil || album != nil {
                        newTracks = try await request("library_tracks", trackFields)
                    } else {
                        newTracks = nil
                    }
                    let newRoots: Page<Root> = try await request("library_roots", pageFields(offset: offsets.3, revision: offsets.3 > 0 ? roots?.revision : nil, query: ""))
                    guard generation == session, epoch == queryEpoch, query == filter, connected else { continue }
                    artists = newArtists; albums = newAlbums; tracks = newTracks; roots = newRoots
                    loadedLibraryQuery = query; loadedLibraryEpoch = epoch
                    loadedLibraryArtist = artist; loadedLibraryAlbum = album
                    libraryFilterError = nil
                    selectedTracks.formIntersection(Set(newTracks?.entries.map(\.id) ?? []))
                    if let artist = selectedArtist, !newArtists.entries.contains(where: { $0.id == artist }) {
                        selectedArtist = nil; selectionChanged()
                    } else if let album = selectedAlbum, !newAlbums.entries.contains(where: { $0.id == album }) {
                        selectedAlbum = nil; selectionChanged()
                    }
                } catch {
                    guard generation == session, epoch == queryEpoch, query == filter, connected else { continue }
                    artists = nil; albums = nil; tracks = nil; selectedTracks = []
                    loadedLibraryQuery = nil; loadedLibraryEpoch = nil
                    if let failure = error as? ServerFailure, failure.code == "stale_library_revision" {
                        artistOffset = 0; albumOffset = 0; trackOffset = 0; rootOffset = 0
                        queryEpoch &+= 1; needsLibrary = true
                    } else if let failure = error as? ServerFailure, failure.code == "invalid_filter" {
                        libraryFilterError = failure.message
                    } else { message = error.localizedDescription }
                }
                if needsLibrary, scanActive { try? await Task.sleep(for: .seconds(1)) }
            }
        }
    }
    private func offset(_ current: Int, _ delta: Int, _ total: Int) -> Int {
        min(max(0, current + delta * Self.pageSize), max(0, ((max(1, total) - 1) / Self.pageSize) * Self.pageSize))
    }
    func pageArtists(_ delta: Int) { artistOffset = offset(artistOffset, delta, artists?.total ?? 0); selectedArtist = nil; selectionChanged(); queryEpoch &+= 1; requestLibrary(after: 0) }
    func pageAlbums(_ delta: Int) { albumOffset = offset(albumOffset, delta, albums?.total ?? 0); selectedAlbum = nil; selectionChanged(); queryEpoch &+= 1; requestLibrary(after: 0) }
    func pageTracks(_ delta: Int) { trackOffset = offset(trackOffset, delta, tracks?.total ?? 0); selectedTracks = []; queryEpoch &+= 1; requestLibrary(after: 0) }
    func pageRoots(_ delta: Int) { rootOffset = offset(rootOffset, delta, roots?.total ?? 0); queryEpoch &+= 1; requestLibrary(after: 0) }

    func chooseFolder() {
        guard canMutate,
              let parent = NSApp.keyWindow ?? NSApp.mainWindow
                ?? NSApp.windows.first(where: { $0.isVisible && $0.canBecomeMain }) else { return }
        let window = parent.attachedSheet ?? parent
        let panel = NSOpenPanel()
        panel.title = "Choose Music Folder"
        panel.prompt = "Add Music Folder"
        panel.canChooseDirectories = true; panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        #if DEBUG
        NativeSmoke.prepareFolderPanel(panel)
        #endif
        panel.beginSheetModal(for: window) { [weak self] response in
            guard response == .OK, let path = panel.url?.path else { return }
            Task { @MainActor in await self?.addRoot(path) }
        }
    }
    func addRoot(_ path: String) async {
        guard canMutate else { return }
        busy = true
        defer { busy = false }
        do {
            let _: RootAdded = try await request("add_root", ["path": .string(path)])
            message = nil; refresh()
        } catch { message = error.localizedDescription }
    }
    /// Runs one user mutation: marks the model busy, reports failure in `message`,
    /// afterwards.
    func mutate(_ action: @escaping @MainActor () async throws -> Void) {
        guard canMutate else { return }
        busy = true; message = nil
        Task {
            defer { busy = false }
            do { try await action() } catch { message = error.localizedDescription }
        }
    }
    func rescan(_ rootID: UInt64) {
        mutate { let _: ScanAccepted = try await self.request("rescan", ["root_id": .uint(rootID)]); self.refresh() }
    }
    func enqueueSelection(play: Bool) {
        guard libraryReady, let page = tracks else { return }
        let selection = page.entries.filter { selectedTracks.contains($0.id) }
        guard !selection.isEmpty else { return }
        mutate {
            var added = 0
            var first: UInt64?
            do {
                for track in selection {
                    guard let state = self.snapshot else { throw CancellationError() }
                    let reply: Applied = try await self.request("append_track", ["expected_revision": .uint(state.queue_revision), "library_revision": .uint(page.revision), "track_id": .uint(track.id)])
                    added += 1
                    if first == nil { first = reply.entry_id }
                }
                if play, let first {
                    let _: Applied = try await self.request("play", ["entry_id": .uint(first)])
                }
            } catch {
                self.message = "\(added) of \(selection.count) tracks acknowledged as queued. \(error.localizedDescription) Inspect the queue before retrying; no command was replayed."
            }
        }
    }
    func playLibraryTrack(_ id: UInt64) {
        guard libraryReady, let page = tracks, page.entries.contains(where: { $0.id == id }) else { return }
        enqueueAndPlayTrack(id, libraryRevision: page.revision)
    }
    func playPlaylistEntry(_ id: UInt64) {
        guard playlistReady, let page = playlistEntries,
              let entry = page.entries.first(where: { $0.id == id }), entry.available else { return }
        enqueueAndPlayTrack(entry.track_id, libraryRevision: page.library_revision)
    }
    private func enqueueAndPlayTrack(_ id: UInt64, libraryRevision: UInt64) {
        guard let revision = snapshot?.queue_revision else { return }
        let generation = playlistSession
        mutate {
            var queued = false
            do {
                let reply: Applied = try await self.request("append_track", [
                    "expected_revision": .uint(revision), "library_revision": .uint(libraryRevision),
                    "track_id": .uint(id)])
                queued = true
                guard generation == self.playlistSession, self.connected else { throw CancellationError() }
                guard let entryID = reply.entry_id else {
                    throw ServerFailure(code: "invalid_reply", message: "Queue reply omitted the new occurrence.")
                }
                let _: Applied = try await self.request("play", ["entry_id": .uint(entryID)])
            } catch {
                guard generation == self.playlistSession, self.connected else { return }
                let outcome = queued ? "Track was queued, but playback was not confirmed." : "Queueing was not confirmed."
                self.message = "\(outcome) \(error.localizedDescription) Review the queue before trying again; no command was replayed."
            }
        }
    }
    var currentPlaylist: Playlist? {
        let visible = playlists?.entries.first { $0.id == selectedPlaylist }
        guard let retainedPlaylist, retainedPlaylist.id == selectedPlaylist else { return visible }
        if let visible, visible.revision > retainedPlaylist.revision { return visible }
        return retainedPlaylist
    }
    var playlistReady: Bool {
        guard let playlist = currentPlaylist, let page = playlistEntries else { return false }
        return connected && playlistEntryFilterError == nil && loadedEntryQuery == playlistEntryFilter
            && !playlistsLoading && !playlistEntriesLoading
            && playlists?.revision == snapshot?.playlists_revision
            && page.playlist_id == playlist.id && page.revision == playlist.revision
            && page.library_revision == snapshot?.library.revision
    }
    func playlistFilterChanged() {
        playlistOffset = 0
        requestPlaylists()
    }
    func playlistEntryFilterChanged() {
        entryOffset = 0; playlistEntries = nil; selectedPlaylistEntries = []
        loadedEntryQuery = nil; playlistEntryError = nil; playlistEntryFilterError = nil
        entryFilterEpoch &+= 1
        if let drag = activeDrag, case .playlist = drag.source { activeDrag = nil }
        entryFilterDeadline = Date().addingTimeInterval(0.25)
        requestPlaylistEntries()
    }
    func playlistSelectionChanged() {
        if let playlist = currentPlaylist { retainedPlaylist = playlist }
        entryOffset = 0; playlistEntries = nil; selectedPlaylistEntries = []
        loadedEntryQuery = nil; playlistEntryError = nil; playlistEntryFilterError = nil
        requestPlaylistEntries()
    }
    func pagePlaylists(_ delta: Int) {
        playlistOffset = offset(playlistOffset, delta, playlists?.total ?? 0)
        requestPlaylists()
    }
    func pagePlaylistEntries(_ delta: Int) {
        entryOffset = offset(entryOffset, delta, playlistEntries?.total ?? 0)
        selectedPlaylistEntries = []
        requestPlaylistEntries()
    }
    private func requestPlaylists() {
        playlistsEpoch &+= 1
        guard connected else { return }
        needsPlaylists = true
        playlistsLoading = true
        guard playlistsTask == nil else { return }
        // Coalesce superseded queries rather than cancel a shared WebSocket request:
        // cancellation cannot retract a sent frame. Epochs discard obsolete replies.
        playlistsTask = Task {
            defer { playlistsTask = nil; playlistsLoading = false }
            while needsPlaylists, connected {
                needsPlaylists = false
                let epoch = playlistsEpoch, generation = session
                let revision = snapshot?.playlists_revision
                do {
                    let page: Page<Playlist> = try await request("saved_playlists",
                        pageFields(offset: playlistOffset, revision: revision, query: playlistFilter))
                    guard generation == session, epoch == playlistsEpoch, connected else { continue }
                    if page.offset > 0 && page.entries.isEmpty {
                        playlistOffset = offset(page.offset, -1, page.total)
                        needsPlaylists = true
                        continue
                    }
                    playlists = page
                    playlistCatalogError = nil
                    if let selected = selectedPlaylist {
                        if let found = page.entries.first(where: { $0.id == selected }) {
                            retainedPlaylist = found
                        } else {
                            // Resolve a retained selection independently of the visible catalog filter.
                            // Keep only one bounded catalog page in memory, even after an external rename.
                            var offset = 0
                            var found: Playlist?
                            repeat {
                                let candidates: Page<Playlist> = try await request("saved_playlists",
                                    pageFields(offset: offset, revision: page.revision, query: ""))
                                guard generation == session, epoch == playlistsEpoch, connected,
                                      selectedPlaylist == selected else { break }
                                found = candidates.entries.first { $0.id == selected }
                                offset += candidates.entries.count
                                if found != nil || offset >= candidates.total || candidates.entries.isEmpty { break }
                            } while true
                            guard generation == session, epoch == playlistsEpoch, connected,
                                  selectedPlaylist == selected else { continue }
                            retainedPlaylist = found
                            if found == nil {
                                selectedPlaylist = nil
                                playlistSelectionChanged()
                            }
                        }
                    }
                    if let playlist = currentPlaylist,
                              playlistEntries?.playlist_id != playlist.id
                                || playlistEntries?.revision != playlist.revision
                                || playlistEntries?.library_revision != snapshot?.library.revision
                                || playlistEntries?.offset != entryOffset
                                || loadedEntryQuery != playlistEntryFilter {
                        requestPlaylistEntries()
                    }
                } catch {
                    guard generation == session, epoch == playlistsEpoch, connected else { continue }
                    playlistCatalogError = error.localizedDescription
                    // Refresh reads only. A rejected mutation is never replayed.
                    if let failure = error as? ServerFailure, failure.code == "stale_playlist_revision" {
                        playlistOffset = 0
                        if let fresh: SnapshotReply = try? await request("snapshot"),
                           generation == session, epoch == playlistsEpoch {
                            accept(fresh.snapshot, newInstance: false)
                        }
                    }
                }
            }
        }
    }
    private func requestPlaylistEntries() {
        entriesEpoch &+= 1
        guard connected else { return }
        needsEntries = true
        playlistEntriesLoading = true
        entriesTimer?.cancel(); entriesTimer = nil
        let wait = entryFilterDeadline.timeIntervalSinceNow
        if wait > 0 {
            entriesTimer = Task { [weak self] in
                do { try await Task.sleep(for: .seconds(wait)) } catch { return }
                guard !Task.isCancelled, let self else { return }
                entriesTimer = nil
                loadPlaylistEntries()
            }
        } else {
            loadPlaylistEntries()
        }
    }
    private func loadPlaylistEntries() {
        guard entriesTask == nil, connected else { return }
        entriesTask = Task {
            defer { entriesTask = nil; playlistEntriesLoading = entriesTimer != nil }
            while needsEntries, connected, entriesTimer == nil {
                needsEntries = false
                guard let id = selectedPlaylist else { playlistEntries = nil; continue }
                // The catalog establishes the current individual revision.
                // Its completion resumes this read at the desired filtered offset.
                guard playlists?.revision == snapshot?.playlists_revision,
                      let playlist = currentPlaylist,
                      let libraryRevision = snapshot?.library.revision else { continue }
                let epoch = entriesEpoch, generation = session, query = playlistEntryFilter
                do {
                    let page: PlaylistEntries = try await request("playlist_entries", [
                        "playlist_id": .uint(id), "revision": .uint(playlist.revision),
                        "library_revision": .uint(libraryRevision), "query": .string(query),
                        "offset": .int(entryOffset), "limit": .int(Self.pageSize)])
                    guard generation == session, epoch == entriesEpoch, query == playlistEntryFilter,
                          connected, selectedPlaylist == id else { continue }
                    if page.offset > 0 && page.entries.isEmpty {
                        entryOffset = offset(page.offset, -1, page.total); needsEntries = true
                        continue
                    }
                    playlistEntries = page; loadedEntryQuery = query
                    selectedPlaylistEntries.formIntersection(Set(page.entries.map(\.id)))
                    playlistEntryError = nil; playlistEntryFilterError = nil
                } catch {
                    guard generation == session, epoch == entriesEpoch, query == playlistEntryFilter,
                          connected, selectedPlaylist == id else { continue }
                    playlistEntries = nil; loadedEntryQuery = nil; selectedPlaylistEntries = []
                    if let failure = error as? ServerFailure, failure.code == "invalid_filter" {
                        playlistEntryFilterError = failure.message
                    } else { playlistEntryError = error.localizedDescription }
                }
            }
        }
    }
    private func changePlaylist(_ command: String, _ fields: [String: JSONValue],
                                selectCreated: Bool = false, successMessage: String? = nil) {
        let generation = playlistSession
        mutate {
            do {
                let reply: PlaylistApplied = try await self.request(command, fields)
                guard generation == self.playlistSession, self.connected else { return }
                if selectCreated, let playlist = reply.playlist {
                    self.retainedPlaylist = playlist
                    self.playlistFilter = ""; self.playlistOffset = 0
                    self.selectedPlaylist = playlist.id
                    self.entryOffset = 0
                }
                if let playlist = reply.playlist, playlist.id == self.selectedPlaylist {
                    self.retainedPlaylist = playlist
                }
                if command == "delete_playlist" {
                    self.selectedPlaylist = nil; self.playlistEntries = nil
                }
                if let playlist = reply.playlist, command == "create_playlist" || command == "rename_playlist" {
                    self.message = "Saved playlist “\(playlist.name)”."
                }
                if let successMessage { self.message = successMessage }
                self.requestPlaylists()
                self.requestPlaylistEntries()
            } catch {
                guard generation == self.playlistSession, self.connected else { return }
                self.message = "\(error.localizedDescription) No command was replayed; review refreshed state before trying again."
                self.requestPlaylists(); self.requestPlaylistEntries()
            }
        }
    }
    func createPlaylist(_ name: String) {
        guard let revision = snapshot?.playlists_revision else { return }
        changePlaylist("create_playlist", ["expected_revision": .uint(revision), "name": .string(name)], selectCreated: true)
    }
    func createPlaylistAddingSelection(_ name: String, completion: @escaping @MainActor (String?) -> Void) {
        guard canMutate, libraryReady, let tracks, let revision = snapshot?.playlists_revision else { return }
        let ids = tracks.entries.filter { selectedTracks.contains($0.id) }.map(\.id)
        guard !ids.isEmpty else { return }
        let generation = playlistSession
        mutate {
            var created: Playlist?
            defer {
                if generation == self.playlistSession, self.connected {
                    self.requestPlaylists(); self.requestPlaylistEntries()
                }
            }
            do {
                let reply: PlaylistApplied = try await self.request("create_playlist", [
                    "expected_revision": .uint(revision), "name": .string(name)])
                created = reply.playlist
                guard generation == self.playlistSession, self.connected else { throw CancellationError() }
                guard let playlist = created else {
                    throw ServerFailure(code: "invalid_reply", message: "Creation reply omitted the playlist.")
                }
                self.playlistFilter = ""; self.playlistOffset = 0
                self.retainedPlaylist = playlist
                self.selectedPlaylist = playlist.id; self.entryOffset = 0
                // Use the returned identity directly: the new playlist may sort
                // beyond the currently loaded catalog page.
                let _: PlaylistApplied = try await self.request("playlist_add", [
                    "playlist_id": .uint(playlist.id), "expected_revision": .uint(playlist.revision),
                    "library_revision": .uint(tracks.revision), "track_ids": .uints(ids), "before": .null])
                guard generation == self.playlistSession, self.connected else { throw CancellationError() }
                self.message = "Created “\(playlist.name)” and added \(ids.count) tracks."
                    + (self.playlistEntryFilterActive ? " Some added tracks may be hidden by the entry filter; clear it to see all tracks." : "")
                completion(nil)
            } catch {
                let outcome = created.map { "Playlist “\($0.name)” was created, but adding tracks was not confirmed." }
                    ?? "Playlist creation was not confirmed."
                let detail = "\(outcome) \(error.localizedDescription) Review the playlist before trying again; no command was replayed."
                completion(detail)
            }
        }
    }
    func renamePlaylist(_ playlist: Playlist, name: String) {
        changePlaylist("rename_playlist", ["playlist_id": .uint(playlist.id),
            "expected_revision": .uint(playlist.revision), "name": .string(name)])
    }
    func deletePlaylist(_ playlist: Playlist) {
        changePlaylist("delete_playlist", ["playlist_id": .uint(playlist.id),
            "expected_revision": .uint(playlist.revision)])
    }
    func addSelectionToPlaylist(_ playlist: Playlist) {
        guard !playlist.isSmart, libraryReady, let tracks else { return }
        let ids = tracks.entries.filter { selectedTracks.contains($0.id) }.map(\.id)
        guard !ids.isEmpty else { return }
        changePlaylist("playlist_add", ["playlist_id": .uint(playlist.id),
            "expected_revision": .uint(playlist.revision), "library_revision": .uint(tracks.revision),
            "track_ids": .uints(ids), "before": .null],
            successMessage: "Added \(ids.count) tracks to “\(playlist.name)”."
                + (playlistEntryFilterActive ? " Some added tracks may be hidden by the entry filter; clear it to see all tracks." : ""))
    }
    func removePlaylistSelection() {
        guard currentPlaylist?.isSmart == false, playlistReady, let page = playlistEntries else { return }
        let ids = page.entries.filter { selectedPlaylistEntries.contains($0.id) }.map(\.id)
        guard !ids.isEmpty else { return }
        changePlaylist("playlist_remove", ["playlist_id": .uint(page.playlist_id),
            "expected_revision": .uint(page.revision), "entry_ids": .uints(ids)])
    }
    func canMovePlaylist(_ delta: Int) -> Bool {
        guard currentPlaylist?.isSmart == false, playlistReady, delta == -1 || delta == 1,
              selectedPlaylistEntries.count == 1, let page = playlistEntries,
              let index = page.entries.firstIndex(where: { selectedPlaylistEntries.contains($0.id) }) else { return false }
        return (0..<page.total).contains(page.offset + index + delta)
    }
    func movePlaylistSelection(_ delta: Int) {
        guard canMovePlaylist(delta), let page = playlistEntries,
              let index = page.entries.firstIndex(where: { selectedPlaylistEntries.contains($0.id) }) else { return }
        let beforeIndex = page.offset + index + (delta < 0 ? -1 : 2)
        let generation = playlistSession
        let query = playlistEntryFilter, filterEpoch = entryFilterEpoch
        mutate {
            do {
                let before: UInt64?
                if beforeIndex == page.total { before = nil }
                else if (page.offset..<(page.offset + page.entries.count)).contains(beforeIndex) {
                    before = page.entries[beforeIndex - page.offset].id
                } else {
                    let neighbor: PlaylistEntries = try await self.request("playlist_entries", [
                        "playlist_id": .uint(page.playlist_id), "revision": .uint(page.revision),
                        "library_revision": .uint(page.library_revision), "query": .string(query),
                        "offset": .int(beforeIndex), "limit": .int(1)])
                    guard let entry = neighbor.entries.first else {
                        throw ServerFailure(code: "stale_playlist_revision", message: "Adjacent entry is unavailable. Refresh before moving.")
                    }
                    before = entry.id
                }
                guard generation == self.playlistSession, self.connected else { return }
                guard query == self.playlistEntryFilter, filterEpoch == self.entryFilterEpoch,
                      self.selectedPlaylist == page.playlist_id else {
                    throw ServerFailure(code: "filter_changed", message: "The playlist filter or selection changed before the move. Select the entries again.")
                }
                let _: PlaylistApplied = try await self.request("playlist_move", [
                    "playlist_id": .uint(page.playlist_id), "expected_revision": .uint(page.revision),
                    "entry_ids": .uints([page.entries[index].id]), "before": before.map(JSONValue.uint) ?? .null])
                guard generation == self.playlistSession, self.connected else { return }
                if query == self.playlistEntryFilter, filterEpoch == self.entryFilterEpoch,
                   self.selectedPlaylist == page.playlist_id {
                    let destination = page.offset + index + delta
                    self.entryOffset = (destination / Self.pageSize) * Self.pageSize
                    self.selectedPlaylistEntries = [page.entries[index].id]
                }
                self.requestPlaylists(); self.requestPlaylistEntries()
            } catch {
                guard generation == self.playlistSession, self.connected else { return }
                self.message = "\(error.localizedDescription) Move was not replayed."
                self.requestPlaylists(); self.requestPlaylistEntries()
            }
        }
    }
    func enqueuePlaylist(play: Bool) {
        guard playlistReady, let page = playlistEntries, let state = snapshot else { return }
        mutate {
            let _: Applied = try await self.request("enqueue_playlist", [
                "playlist_id": .uint(page.playlist_id), "playlist_revision": .uint(page.revision),
                "library_revision": .uint(page.library_revision),
                "expected_revision": .uint(state.queue_revision), "play": .bool(play)])
        }
    }

    /// Server-parsed global preview. Never inherits artist/album pane selection.
    func previewSmartRule(_ query: String) async throws -> Page<Track> {
        let generation = playlistSession
        // SwiftUI cancels superseded preview tasks; never propagate that cancellation
        // to a sent WebSocket frame (which would tear down the shared connection).
        let page: Page<Track> = try await Task { @MainActor in
            try await self.request("library_tracks", [
                "query": .string(query), "album_id": .null, "revision": .null,
                "offset": .int(0), "limit": .int(Self.pageSize)], as: Page<Track>.self)
        }.value
        guard generation == playlistSession, connected, page.revision == snapshot?.library.revision else {
            throw CancellationError()
        }
        return page
    }

    func regularPlaylistPage(query: String, offset: Int) async throws -> Page<Playlist> {
        let generation = playlistSession
        var fields = pageFields(offset: offset, revision: snapshot?.playlists_revision, query: query)
        fields["kind"] = .string("regular")
        let requestFields = fields
        let page: Page<Playlist> = try await Task { @MainActor in
            try await self.request("saved_playlists", requestFields, as: Page<Playlist>.self)
        }.value
        guard generation == playlistSession, connected else { throw CancellationError() }
        return page
    }

    func selectPlaylist(_ playlist: Playlist) {
        retainedPlaylist = playlist
        selectedPlaylist = playlist.id
        playlistSelectionChanged()
    }

    func saveSmartPlaylist(_ playlist: Playlist?, name: String, query: String, connectionGeneration: UInt64,
                           completion: @escaping @MainActor (String?) -> Void) {
        guard connectionGeneration == playlistSession, canMutate, let revision = snapshot?.playlists_revision else {
            completion("The connection changed. Close this editor and reopen it before saving."); return
        }
        var fields: [String: JSONValue] = ["query": .string(query)]
        if let playlist {
            fields["playlist_id"] = .uint(playlist.id)
            fields["expected_revision"] = .uint(playlist.revision)
        } else {
            fields["expected_revision"] = .uint(revision)
            fields["name"] = .string(name)
        }
        performSmartChange(playlist == nil ? "create_smart_playlist" : "update_smart_playlist",
            fields, selectCreated: playlist == nil, completion: completion)
    }

    func snapshotSmartPlaylist(_ playlist: Playlist, libraryRevision: UInt64, catalogRevision: UInt64,
                               name: String, connectionGeneration: UInt64,
                               completion: @escaping @MainActor (String?) -> Void) {
        guard playlist.isSmart, connectionGeneration == playlistSession, canMutate else {
            completion("The connection changed. Close this dialog and reopen it before creating a copy."); return
        }
        performSmartChange("snapshot_smart_playlist", [
            "playlist_id": .uint(playlist.id), "playlist_revision": .uint(playlist.revision),
            "library_revision": .uint(libraryRevision), "expected_revision": .uint(catalogRevision),
            "name": .string(name)], selectCreated: true, completion: completion)
    }

    private func performSmartChange(_ command: String, _ fields: [String: JSONValue], selectCreated: Bool,
                                    completion: @escaping @MainActor (String?) -> Void) {
        let generation = playlistSession
        mutate {
            do {
                guard generation == self.playlistSession, self.connected else { throw CancellationError() }
                let reply: PlaylistApplied = try await self.request(command, fields)
                guard generation == self.playlistSession, self.connected else { throw CancellationError() }
                if let playlist = reply.playlist {
                    if selectCreated {
                        self.playlistFilter = ""; self.playlistOffset = 0
                        self.playlistEntryFilter = ""
                        self.selectedPlaylist = playlist.id
                        self.showPlaylists = true
                    }
                    if playlist.id == self.selectedPlaylist {
                        self.retainedPlaylist = playlist
                        self.playlistSelectionChanged()
                    }
                }
                self.requestPlaylists(); self.requestPlaylistEntries()
                completion(nil)
            } catch {
                completion("\(error.localizedDescription) Nothing was replayed; review current state before trying again.")
                self.requestPlaylists(); self.requestPlaylistEntries()
            }
        }
    }
}

extension PlayerModel {
    var trackDragIsCopy: Bool { activeDrag?.source == .library }

    /// Starts a track-list drag. The rows travel as a selection: IDs for a small loaded
    /// selection, otherwise the list description with ranges (spec 5.2).
    func beginSelectionDrag(_ selection: TrackSelection, count: Int, label: String, trackIDs: [UInt64]? = nil) -> String? {
        guard canMutate, count > 0, snapshot != nil else { return nil }
        let revision: UInt64
        switch selection {
        case .ids(let libraryRevision, _): revision = libraryRevision
        case .list(_, let libraryRevision, _, _, _): revision = libraryRevision
        }
        let drag = TrackDrag(token: UUID().uuidString, connection: playlistSession, source: .library,
            ids: [], positions: [], revision: revision, libraryRevision: revision,
            selection: selection, count: count, label: label, trackIDs: trackIDs)
        activeDrag = drag
        return drag.token
    }

    /// Up Next appends; a regular playlist adds at its end. The server checks the
    /// selection's library revision. A queue edit that races another is retried once.
    private func dropSelection(_ selection: TrackSelection, of drag: TrackDrag, on destination: TrackDragSource) -> Bool {
        let count = drag.count
        let noun = count == 1 ? "track" : "tracks"
        switch destination {
        case .library:
            return false
        case .queue:
            mutate {
                do {
                    try await self.appendSelection(selection, label: drag.label, connection: drag.connection)
                    self.message = "Added \(DisplayFormat.count(count)) \(noun) to Up Next."
                } catch {
                    self.message = "\(error.localizedDescription) Drop was not replayed; review refreshed state before trying again."
                }
            }
        case .playlist(let id):
            guard let playlist = savedPlaylists.playlist(id: id), !playlist.isSmart else {
                message = "Wait for the playlist catalog to finish loading before dropping."
                return false
            }
            mutate {
                guard self.connected, self.playlistSession == drag.connection else {
                    throw ServerFailure(code: "connection_changed", message: "The drag connection changed. Nothing was replayed.")
                }
                try await self.savedPlaylists.addSelection(to: playlist, selection: selection, count: count)
            }
        }
        activeDrag = nil
        return true
    }

    func appendSelection(_ selection: TrackSelection, label: String, connection: UInt64) async throws {
        var retried = false
        while true {
            guard connected, playlistSession == connection, let state = snapshot else {
                throw ServerFailure(code: "connection_changed", message: "The drag connection changed. Nothing was replayed.")
            }
            do {
                let _: Applied = try await request("enqueue_tracks", [
                    "selection": try JSONValue.encoding(selection), "expected_revision": .uint(state.queue_revision),
                    "placement": .string(Placement.append.rawValue), "play": .bool(false),
                    "source": try JSONValue.encoding(QueueSource(kind: .drag, name: label))])
                return
            } catch let failure as ServerFailure where failure.code == "stale_revision" && !retried {
                // Appending does not depend on the queue's contents: refresh the revision and retry once.
                retried = true
                let _: SnapshotReply = try await request("snapshot")
            }
        }
    }

    func validateTrackHover(destination: TrackDragSource?) -> Bool {
        guard let drag = activeDrag else { return false }
        return validateTrackDrop(token: drag.token, destination: destination)
    }

    func validateTrackDrop(token: String, destination: TrackDragSource?) -> Bool {
        guard let drag = activeDrag, drag.token == token else { return false }
        // Smart playlists, and playlists not loaded yet, refuse drops.
        if case .playlist(let id) = destination {
            guard let playlist = savedPlaylists.playlist(id: id),
                  PlaylistDrop.onRow(playlist.isSmart ? .smartPlaylist : .regularPlaylist) == .addSelection else { return false }
        }
        let problem: String?
        if !connected || drag.connection != playlistSession {
            problem = "The connection changed during this drag. Start a new drag; nothing was replayed."
        } else if !canMutate {
            problem = "Wait for the current command to finish, then start a new drag."
        } else if drag.source == .queue && drag.revision != snapshot?.queue_revision {
            problem = "The queue changed during this drag. Start a new drag from the refreshed queue."
        } else if destination == .library {
            return false
        } else if let destination, drag.source != .library && destination != drag.source {
            return false
        } else {
            problem = nil
        }
        if let problem {
            if message != problem { message = problem }
            return false
        }
        return true
    }

    /// Drops the active drag on a source-list row or the drawer: Up Next appends, a regular
    /// playlist adds at its end. Drops inside a playlist's table go through its `ListModel`.
    @discardableResult
    func dropTracks(token: String, destination: TrackDragSource) -> Bool {
        guard validateTrackDrop(token: token, destination: destination), let drag = activeDrag,
              let selection = drag.selection else { return false }
        return dropSelection(selection, of: drag, on: destination)
    }
}
