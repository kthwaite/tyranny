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
    /// Plays and adds runs from every list, grid and table, and asks "Clear Up Next?".
    let activation = ActivationModel()
    /// The music folders for Settings. One bounded page; the server limits a page to 500.
    private(set) var roots: Page<Root>?
    var message: String?
    @ObservationIgnored private var playlistSession: UInt64 = 0
    private(set) var busy = false {
        didSet { updateMediaControls() }
    }
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
    @ObservationIgnored private var rootsTask: Task<Void, Never>?
    @ObservationIgnored private var rootsTimer: Task<Void, Never>?
    @ObservationIgnored private var needsRoots = false
    @ObservationIgnored private var session: UInt64 = 0
    @ObservationIgnored private var lastRootsRefresh = Date.distantPast
    /// Set on the first hello of this run, so a quit note is decided once, never on reconnects.
    @ObservationIgnored private var launchSuspensionChecked = false
    /// The hello arrives before `connected` is true, and `play` needs `connected`.
    /// So the decision waits here until the connection loop marks the connection connected.
    @ObservationIgnored private var launchResumePending = false
    /// Where quit notes live. The app picks it from the launch arguments.
    @ObservationIgnored private let quitSuspensions: QuitSuspensionStore
    /// The server's page limit.
    private static let catalogLimit = 500
    @ObservationIgnored var activeDrag: TrackDrag?

    init(quitSuspensions: QuitSuspensionStore) {
        self.quitSuspensions = quitSuspensions
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
        activation.attach(self)
        updateMediaControls()
    }

    var connected: Bool { connection == "connected" }
    var playlistConnectionGeneration: UInt64 { playlistSession }
    var canMutate: Bool { connected && !busy && !serverManagementBusy }
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

    /// Pauses the server if it is playing, waiting at most 1 s for the reply.
    /// It writes the quit note only when the reply arrived in time.
    /// It skips `mutate`, so a busy model cannot block quitting.
    func suspendForQuit() async {
        guard connected, let state = snapshot,
              let note = QuitSuspension.note(endpoint: endpoint, transport: state.transport,
                                             currentID: state.current_id) else { return }
        let reply = await Deadline.race(.seconds(1)) { try await self.send("pause") as Applied }
        guard reply != nil else { return }
        quitSuspensions.save(note)
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
        rootsTimer?.cancel()
        wire.disconnect()
        clearRemoteState()
        connection = "disconnected"
        connectionDetail = "Disconnected. The server continues playing independently."
        if busy { message = "Disconnected during a command. Its outcome is uncertain; reconnect and inspect state before repeating it." }
    }
    private func clearRemoteState() {
        rootsTimer?.cancel(); rootsTimer = nil
        needsRoots = false
        activeDrag = nil
        playbackSession &+= 1
        playlistSession &+= 1
        snapshot = nil
        roots = nil
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
                    let resume = launchResumePending
                    launchResumePending = false
                    guard generation == session, wantsConnection else { return }
                    connection = "connected"
                    connectionDetail = endpoint
                    connectionTask = nil
                    refresh()
                    if resume { transport.play() }
                    return
                } catch {
                    launchResumePending = false
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
        if newInstance, !launchSuspensionChecked {
            launchSuspensionChecked = true
            if let note = quitSuspensions.take(),
               note.shouldResume(endpoint: endpoint, transport: state.transport, currentID: state.current_id) {
                launchResumePending = true
            }
        }
        // A rescan that changes only cover files keeps the library revision, so
        // invalidate artwork when a scan finishes, not only on revision change.
        if let previous, previous.library.revision == state.library.revision,
           (["discovering", "indexing"].contains(previous.library.scan.phase)
                && !["discovering", "indexing"].contains(state.library.scan.phase))
            || (previous.library.scan.phase != "complete" && state.library.scan.phase == "complete") {
            updateArtwork(invalidate: true)
        }
        guard connected else { return }
        if previous?.library.revision != state.library.revision
            || previous?.library.scan.phase != state.library.scan.phase {
            let delay = scanActive ? max(0, 1 - Date().timeIntervalSince(lastRootsRefresh)) : 0
            requestRoots(after: delay)
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
    /// Reloads what the model itself keeps (the music folders); the snapshot arrives by event.
    private func refresh() {
        requestRoots(after: 0)
    }
    private func requestRoots(after delay: Double) {
        guard connected else { return }
        needsRoots = true
        rootsTimer?.cancel()
        rootsTimer = Task { [weak self] in
            if delay > 0 { do { try await Task.sleep(for: .seconds(delay)) } catch { return } }
            guard !Task.isCancelled, let self else { return }
            rootsTimer = nil
            loadRoots()
        }
    }
    private func loadRoots() {
        guard rootsTask == nil, connected else { return }
        rootsTask = Task { [weak self] in
            guard let self else { return }
            defer { rootsTask = nil }
            while needsRoots, connected, rootsTimer == nil {
                needsRoots = false
                let generation = session
                lastRootsRefresh = Date()
                do {
                    // Every page, so no folder is hidden from Settings.
                    var all: [Root] = []
                    var last: Page<Root>?
                    repeat {
                        let page: Page<Root> = try await request("library_roots", [
                            "offset": .int(all.count), "limit": .int(Self.catalogLimit),
                            "revision": last.map { JSONValue.uint($0.revision) } ?? .null, "query": .string("")])
                        guard generation == session, connected else { break }
                        all += page.entries
                        last = page
                        if page.entries.isEmpty { break }
                    } while all.count < (last?.total ?? 0)
                    guard generation == session, connected, let last else { continue }
                    roots = Page(revision: last.revision, total: last.total, offset: 0, entries: all)
                } catch {
                    guard generation == session, connected else { continue }
                    message = error.localizedDescription
                }
                if needsRoots, scanActive { try? await Task.sleep(for: .seconds(1)) }
            }
        }
    }

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
    /// One page of regular (non-smart) saved playlists, as the server's catalog holds them.
    func regularPlaylistPage(query: String, offset: Int) async throws -> Page<Playlist> {
        let generation = playlistSession
        let fields: [String: JSONValue] = [
            "offset": .int(offset), "limit": .int(Self.catalogLimit),
            "revision": snapshot.map { JSONValue.uint($0.playlists_revision) } ?? .null,
            "query": .string(query), "kind": .string("regular")]
        let page: Page<Playlist> = try await Task { @MainActor in
            try await self.request("saved_playlists", fields, as: Page<Playlist>.self)
        }.value
        guard generation == playlistSession, connected else { throw CancellationError() }
        return page
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
