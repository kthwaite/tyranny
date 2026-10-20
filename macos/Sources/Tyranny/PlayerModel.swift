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
    let notifications = TrackNotifications()
    /// Transport commands and the player bar display. Owns the system media controls.
    let transport = TransportModel()
    /// The Up Next view's and drawer's queue windows.
    let upNext = UpNextModel()
    /// The saved playlists in the source list, and the edits made to them.
    let savedPlaylists = PlaylistsModel()
    /// The Radio source's saved stations.
    let stations = StationsModel()
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
    @ObservationIgnored private var commandConnectionReply: CheckedContinuation<Void, Error>?
    @ObservationIgnored private var rootsTask: Task<Void, Never>?
    @ObservationIgnored private var rootsTimer: Task<Void, Never>?
    @ObservationIgnored private var needsRoots = false
    @ObservationIgnored private var session: UInt64 = 0
    @ObservationIgnored private var lastRootsRefresh = Date.distantPast
    /// Set on the first hello of this run, so a quit note is decided once, never on reconnects.
    @ObservationIgnored private var launchSuspensionChecked = false
    /// Set on the first authoritative snapshot of this run, so a launch import is decided once.
    @ObservationIgnored private var launchImportChecked = false
    /// The onboarding import in progress. It outlives disconnects.
    @ObservationIgnored private var importSession: ImportSession?
    /// The session's root, observed so the folder line updates when roots load.
    private(set) var importRootID: UInt64?
    /// What the import screen shows. Nil when no import screen is wanted.
    private(set) var importProgress: ImportProgress?
    #if DEBUG
    /// Shown by the import screen instead of `importProgress`.
    var importOverride: ImportProgress?
    /// Shown as the folder line while `importOverride` is set.
    var importFolderOverride: String?
    #endif
    /// The hello arrives before `connected` is true, and `play` needs `connected`.
    /// So the decision waits here until the connection loop marks the connection connected.
    @ObservationIgnored private var launchResumePending = false
    /// Where quit notes live. The app picks it from the launch arguments.
    @ObservationIgnored private let quitSuspensions: QuitSuspensionStore
    /// Where the live import's note lives. It shares the quit note's defaults suite.
    @ObservationIgnored private let importNotes: ImportNoteStore
    /// The server's page limit.
    private static let catalogLimit = 500
    @ObservationIgnored var activeDrag: TrackDrag?
    /// The launch prefetch, until the first `listTracks` takes it. It names the server it asked.
    @ObservationIgnored private var launchPrefetch: (slot: LaunchPrefetchSlot, deadline: ContinuousClock.Instant,
                                                     endpoint: String, tokenPath: String)?

    init(quitSuspensions: QuitSuspensionStore, importNotes: ImportNoteStore) {
        self.quitSuspensions = quitSuspensions
        self.importNotes = importNotes
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
        startLaunchPrefetch()
        wire.onSnapshot = { [weak self] state, instance in self?.accept(state, newInstance: instance != nil) }
        wire.onDisconnect = { [weak self] reason in self?.lostConnection(reason) }
        transport.attach(self)
        upNext.attach(self)
        savedPlaylists.attach(self)
        stations.attach(self)
        activation.attach(self)
        updateMediaControls()
    }

    /// The main queue is busy building the window for the first ≈0.5 s, so the main connection
    /// and the Songs list's first `list_tracks` start late. The prefetch sends that request now,
    /// off the main actor. The grouping is read where the window's `NavigationModel()` reads it.
    private func startLaunchPrefetch() {
        guard configurationError == nil,
              let request = try? TrackListRequest.launchSongs(
                  defaultSort: Destination.songs.defaultSort,
                  group: NavigationModel.grouping(for: .songs, in: .standard)) else { return }
        let started = LaunchPrefetch.start(endpoint: endpoint, tokenPath: tokenPath, request: request)
        launchPrefetch = (started.slot, started.deadline, endpoint, tokenPath)
    }

    /// One `list_tracks` window. The first call may be answered by the launch prefetch instead: only
    /// when `fields` is the prefetch's request, the server is still the one it asked, and the reply
    /// is fresh for the main connection (`LaunchPrefetchReply.isFresh`). A finished prefetch is
    /// checked at once. One still in flight races a normal send: the first ok reply wins.
    /// The server's library worker is serial, so in the race the send queues behind the prefetch
    /// query (or the reverse). That is rare and bounded by the prefetch's 2 s deadline.
    func listTracks(_ fields: [String: JSONValue]) async throws -> ListTracksResponse {
        guard let launch = launchPrefetch else { return try await send("list_tracks", fields) }
        launchPrefetch = nil
        guard launch.endpoint == endpoint, launch.tokenPath == tokenPath else {
            return try await send("list_tracks", fields)
        }
        guard await launch.slot.isPending else {
            if let page = freshPrefetchPage(await launch.slot.claim(fields, until: launch.deadline)) { return page }
            return try await send("list_tracks", fields)
        }
        return try await withCheckedThrowingContinuation { continuation in
            let race = FirstListReply(continuation)
            // A cancelled request would close the shared connection: send it unstructured. Neither
            // task is cancelled; the loser's reply is dropped.
            Task { @MainActor in
                do {
                    race.sent(.success(try await self.send("list_tracks", fields)))
                } catch {
                    race.sent(.failure(error))
                }
            }
            Task { @MainActor [weak self] in
                let claimed = await launch.slot.claim(fields, until: launch.deadline)
                race.prefetched(self?.freshPrefetchPage(claimed))
            }
        }
    }

    /// The claimed reply's page, when it is fresh for the main connection now.
    private func freshPrefetchPage(_ reply: LaunchPrefetchReply?) -> ListTracksResponse? {
        guard let reply, let page = try? decoder.decode(ListTracksResponse.self, from: reply.body),
              reply.isFresh(pageRevision: page.revision, instanceID: wire.instanceID,
                            libraryRevision: snapshot?.library.revision,
                            historyRevision: snapshot?.history_revision) else { return nil }
        return page
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
        stations.sync()
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
        notifications.shutdown()
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
        Task {
            do { try await manageServerForCommand(restart ? "restart" : "stop") }
            catch {
                message = error.localizedDescription
                connectionDetail = error.localizedDescription
            }
        }
    }
    func disconnect() {
        finishConnectionCommand(.failure(CommandLanguageError("The connection attempt was cancelled.")))
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
        notifications.disconnected()
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
                    finishConnectionCommand(.success(()))
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
                            finishConnectionCommand(.failure(error))
                            return
                        }
                    }
                    connection = "disconnected"
                    connectionDetail = "\(error.localizedDescription) Retrying in \(delay)s."
                    finishConnectionCommand(.failure(error))
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
        notifications.receive(state, newSession: newInstance, artwork: artwork)
        if newInstance || previous?.current_id != state.current_id { playbackSession &+= 1 }
        if newInstance, !launchImportChecked {
            launchImportChecked = true
            let note = importNotes.take()
            if importSession == nil {
                setImportSession(ImportSession.atLaunch(library: state.library, note: note, endpoint: endpoint))
            }
        }
        updateImportProgress()
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
    /// Sends one command and returns its reply's result object, for a TyrannyCore model to decode.
    func sendRaw(_ request: CommandRequest) async throws -> Data {
        try await wire.request(request.name, fields: request.fields)
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
        Task {
            do { try await chooseFolderForCommand() }
            catch is CancellationError { }
            catch { message = error.localizedDescription }
        }
    }
    /// The path of the root being imported, abbreviated, once the root list has it.
    var importRootPath: String? {
        #if DEBUG
        if importOverride != nil, let importFolderOverride { return importFolderOverride }
        #endif
        guard let importRootID, let root = roots?.entries.first(where: { $0.id == importRootID }) else { return nil }
        return DisplayFormat.abbreviatedPath(root.path, home: NSHomeDirectory())
    }
    /// Closes the import screen.
    func endImport() {
        guard importSession != nil || importProgress != nil else { return }
        setImportSession(nil)
    }
    /// Try Again: a fresh baseline, then a rescan of the same root.
    func retryImport() {
        Task {
            do { try await retryImportForCommand() }
            catch { message = error.localizedDescription }
        }
    }
    private func setImportSession(_ session: ImportSession?) {
        importSession = session
        importRootID = session?.rootID
        if let session {
            importNotes.save(ImportNote(endpoint: endpoint, rootID: session.rootID))
        } else {
            importNotes.clear()
        }
        importProgress = nil
        updateImportProgress()
    }
    /// Recomputes the screen from the current snapshot. Call once per accepted snapshot: it remembers the shown fraction.
    private func updateImportProgress() {
        guard var session = importSession, let library = snapshot?.library else { return }
        importProgress = session.progress(library: library)
        importSession = session
    }
    func addRoot(_ path: String) async {
        do { try await addRootAwaitingReply(path) }
        catch { message = error.localizedDescription }
    }

    private func addRootAwaitingReply(_ path: String) async throws {
        try await performMutation {
            let rootsBefore = snapshot?.library.roots ?? 0
            let baseline = snapshot?.library.scan
            let added: RootAdded = try await request("add_root", ["path": .string(path)])
            setImportSession(ImportSession.afterAddRoot(
                rootID: added.root.id, rootsBefore: rootsBefore, current: importSession, baseline: baseline))
            refresh()
        }
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

    /// Awaitable user mutation for non-view callers. The same busy gate as `mutate`, but
    /// failure reaches the caller and completion means the remote operation has replied.
    func performMutation(_ action: @MainActor () async throws -> Void) async throws {
        guard connected else { throw CommandLanguageError("Disconnected. Connect before running this command.") }
        guard canMutate else { throw CommandLanguageError("Wait for the current command to finish.") }
        busy = true
        message = nil
        defer { busy = false }
        do { try await action() }
        catch { message = error.localizedDescription; throw error }
    }

    func connectForCommand() async throws {
        guard !serverManagementBusy, !busy else {
            throw CommandLanguageError("Wait for the current command to finish.")
        }
        if connected { return }
        if connectionTask == nil { reconnect() }
        guard connectionTask != nil else { throw CommandLanguageError(connectionDetail) }
        guard commandConnectionReply == nil else { throw CommandLanguageError("A connection command is already waiting.") }
        try await withCheckedThrowingContinuation { commandConnectionReply = $0 }
    }

    private func finishConnectionCommand(_ result: Result<Void, Error>) {
        let reply = commandConnectionReply
        commandConnectionReply = nil
        reply?.resume(with: result)
    }

    func manageServerForCommand(_ operation: String) async throws {
        guard managesServer, let managedServer else {
            throw CommandLanguageError("This connection does not use the managed local server.")
        }
        guard !serverManagementBusy, !busy, connection != "connecting" else {
            throw CommandLanguageError("Wait for the current command to finish.")
        }
        if operation == "start" {
            managedEnabled = true
            try await connectForCommand()
            return
        }
        disconnect()
        serverManagementBusy = true
        defer { serverManagementBusy = false }
        if operation == "restart" {
            try await managedServer.restart()
            managedEnabled = true
            serverManagementBusy = false
            try await connectForCommand()
        } else {
            try await managedServer.uninstall()
            managedEnabled = false
            connectionDetail = "Background job removed; data retained. Manually started servers are not stopped."
        }
    }

    func rescanForCommand(_ rootID: UInt64) async throws {
        guard !scanActive else { throw CommandLanguageError("A library scan is already running.") }
        try await performMutation {
            let _: ScanAccepted = try await request("rescan", ["root_id": .uint(rootID)])
            refresh()
        }
    }

    func retryImportForCommand() async throws {
        guard !scanActive, importProgress?.stage == .stopped,
              let scan = snapshot?.library.scan, var session = importSession else {
            throw CommandLanguageError("There is no stopped import to retry.")
        }
        try await performMutation {
            session.retry(baseline: scan)
            importSession = session
            updateImportProgress()
            do {
                let _: ScanAccepted = try await request("rescan", ["root_id": .uint(session.rootID)])
                refresh()
            } catch {
                if var current = importSession, current.rootID == session.rootID {
                    current.cancelRetry()
                    importSession = current
                    updateImportProgress()
                }
                throw error
            }
        }
    }

    func chooseFolderForCommand() async throws {
        guard canMutate, !scanActive else {
            throw CommandLanguageError("Connect and wait for the current command or scan to finish.")
        }
        guard let parent = NSApp.keyWindow ?? NSApp.mainWindow
            ?? NSApp.windows.first(where: { $0.isVisible && $0.canBecomeMain }) else {
            throw CommandLanguageError("Open a window before choosing a music folder.")
        }
        let panel = NSOpenPanel()
        panel.title = "Choose Music Folder"
        panel.prompt = "Add Music Folder"
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        #if DEBUG
        NativeSmoke.prepareFolderPanel(panel)
        #endif
        let response = await withCheckedContinuation { continuation in
            panel.beginSheetModal(for: parent.attachedSheet ?? parent) { continuation.resume(returning: $0) }
        }
        guard response == .OK, let path = panel.url?.path else { throw CancellationError() }
        try await addRootAwaitingReply(path)
    }
    func rescan(_ rootID: UInt64) {
        Task {
            do { try await rescanForCommand(rootID) }
            catch { message = error.localizedDescription }
        }
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

/// The first ok reply of a launch-time `list_tracks` send and the launch prefetch. A failed send
/// waits for the prefetch; a stale or failed prefetch waits for the send.
@MainActor
private final class FirstListReply {
    private var continuation: CheckedContinuation<ListTracksResponse, any Error>?
    private var sendFailure: (any Error)?
    private var prefetchFinished = false

    init(_ continuation: CheckedContinuation<ListTracksResponse, any Error>) {
        self.continuation = continuation
    }

    func sent(_ result: Result<ListTracksResponse, any Error>) {
        switch result {
        case .success(let page):
            finish(.success(page))
        case .failure(let error):
            if prefetchFinished { finish(.failure(error)) } else { sendFailure = error }
        }
    }

    /// `page` is the fresh prefetched page, or nil when there is none.
    func prefetched(_ page: ListTracksResponse?) {
        if let page {
            finish(.success(page))
            return
        }
        prefetchFinished = true
        if let sendFailure { finish(.failure(sendFailure)) }
    }

    private func finish(_ result: Result<ListTracksResponse, any Error>) {
        guard let continuation else { return }
        self.continuation = nil
        continuation.resume(with: result)
    }
}
