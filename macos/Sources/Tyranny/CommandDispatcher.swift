import AppKit
import Foundation
import TyrannyCore

/// The text-command adapter. It reads the same models and selection stores as the window;
/// structured requests, revision checks and activation prompts remain in their existing owners.
@MainActor
struct CommandDispatcher {
    let player: PlayerModel
    let navigation: NavigationModel
    let lists: TrackLists

    func completions(_ text: String) -> [String] {
        player.savedPlaylists.sync()
        player.savedPlaylists.loadAll()
        return CommandLanguage.completions(text, playlistNames: player.savedPlaylists.rows.map(\.name))
    }

    func execute(_ text: String) async throws {
        let command = try CommandLanguage.parse(text)
        let arguments = command.arguments
        switch command.id {
        case .play, .pause, .toggle, .stop, .next, .previous, .seek, .volume,
             .repeatMode, .shuffle, .gain, .resume:
            try await player.performMutation {
                guard let state = player.snapshot,
                      let request = try CommandLanguage.transportRequest(command, state: state) else {
                    throw CommandLanguageError("Playback state is unavailable.")
                }
                let _: Applied = try await player.send(request.name, request.fields)
            }
        case .go:
            navigation.destination = try destination(arguments[0])
            player.endImport()
        case .filter:
            guard ![.history, .upNext, .radio].contains(navigation.destination) else {
                throw CommandLanguageError("This view does not have a filter.")
            }
            try requireConnection()
            var description = currentList?.description ?? ListDescription(source: .library)
            description.query = arguments[0]
            let _: ListTracksResponse = try await player.send("list_tracks", [
                "list": try JSONValue.encoding(description), "revision": .null,
                "offset": .int(0), "limit": .int(1)])
            navigation.filter = arguments[0]
            currentList?.sync()
            if navigation.destination == .albums { grid.sync() }
        case .focusFilter:
            guard ![.history, .upNext, .radio].contains(navigation.destination) else {
                throw CommandLanguageError("This view does not have a filter.")
            }
            navigation.focusFilter()
        case .revealPlaying:
            try revealPlaying()
        case .browser:
            navigation.columnBrowserVisible = flag(arguments[0], current: navigation.columnBrowserVisible)
            currentList?.sync()
        case .drawer:
            navigation.upNextDrawerVisible = flag(arguments[0], current: navigation.upNextDrawerVisible)
        case .artwork:
            navigation.nowPlayingArtExpanded = flag(arguments[0], current: navigation.nowPlayingArtExpanded)
        case .settings:
            guard let tab = SettingsTab(rawValue: arguments[0]) else { throw CommandLanguageError("Unknown Settings tab.") }
            navigation.showSettings(tab)
        case .connect:
            try await player.connectForCommand()
        case .disconnect:
            guard !player.serverManagementBusy else { throw CommandLanguageError("Wait for server management to finish.") }
            player.disconnect()
        case .server:
            try await player.manageServerForCommand(arguments[0])
        case .chooseFolder:
            try await player.chooseFolderForCommand()
        case .rescan:
            try await player.rescanForCommand(try identifier(arguments[0]))
        case .retryImport:
            try await player.retryImportForCommand()
        case .browseLibrary:
            guard player.importProgress != nil else { throw CommandLanguageError("No import screen is open.") }
            player.endImport()
        case .quit:
            NSApp.terminate(nil)
        case .playlistOpen:
            try requireConnection()
            let playlist = try await player.savedPlaylists.named(arguments[0])
            navigation.destination = .playlist(playlist.id)
            player.endImport()
        case .playlistCreate:
            try await player.performMutation { _ = try await player.savedPlaylists.create(name: arguments[0]) }
        case .playlistRename, .playlistDelete, .playlistPlay, .playlistEnqueue, .playlistAdd,
             .smartUpdate, .smartSnapshot:
            try await playlistCommand(command)
        case .smartCreate:
            try await player.performMutation {
                _ = try await player.savedPlaylists.saveSmart(nil, name: arguments[0], rule: arguments[1])
            }
        case .queueClear, .queueShuffle, .queueSave:
            try await player.performMutation {
                guard let count = player.upNext.upcomingCount, count > 0 else {
                    throw CommandLanguageError("Up Next has no upcoming entries.")
                }
                try await player.upNext.execute(command.id, name: arguments.first ?? "")
            }
        case .selectionPlay, .selectionRun, .selectionNext, .selectionEnqueue:
            try requireTrackSelection()
            try await playSelection(command.id)
        case .selectionRemove, .selectionMove:
            try requireTrackSelection()
            try await editSelection(command)
        case .selectionAlbum, .selectionArtist:
            try requireTrackSelection()
            try await revealSelection(artist: command.id == .selectionArtist)
        case .selectionAll, .selectionClear:
            try requireTrackSelection()
            try await selectAll(command.id == .selectionAll)
        case .group:
            let list = try requireList()
            guard let grouping = Grouping(rawValue: arguments[0]) else { throw CommandLanguageError("Unknown grouping.") }
            navigation.setGrouping(grouping, for: list.destination)
            list.sync()
        case .sort:
            let list = try requireList()
            if arguments[0] == "none" { list.setSort([]) }
            else {
                guard let field = SortField(rawValue: arguments[0]) else { throw CommandLanguageError("Unknown sort field.") }
                list.setSort([SortKey(field: field, descending: arguments.count > 1 && arguments[1] == "desc")])
            }
        case .column:
            let list = try requireList()
            let id = arguments[0]
            guard TrackColumns.column(id)?.hideable == true else {
                throw CommandLanguageError("That column cannot be shown or hidden.")
            }
            let preferences = ColumnPreferences.shared
            let shown = preferences.isShown(id, sortFields: list.sortKeys.map(\.field))
            preferences.setHidden(id, !flag(arguments[1], current: shown))
        case .albumSort:
            try requireAlbums()
            guard let sort = AlbumSort(rawValue: arguments[0]) else { throw CommandLanguageError("Unknown album sort.") }
            grid.setSort(sort)
        case .coverSize:
            try requireAlbums()
            guard let value = Double(arguments[0]), AlbumGridGeometry.coverRange.contains(value) else {
                throw CommandLanguageError("Cover size must be between 96 and 200.")
            }
            UserDefaults.standard.set(value, forKey: "albumGridCoverSize")
        case .albumExpand:
            try requireAlbums()
            let album = try selectedAlbum()
            guard grid.expansion?.albumID != album.id else { throw CommandLanguageError("That album is already expanded.") }
            grid.toggleExpansion(album)
        case .albumCollapse:
            try requireAlbums()
            guard grid.expansion != nil else { throw CommandLanguageError("No album is expanded.") }
            grid.collapse()
        case .browserGenre, .browserArtist, .browserAlbum:
            try await setBrowser(command)
        case .upNextPolicy:
            UserDefaults.standard.set(arguments[0], forKey: "upNextOnPlay")
        case .prompt:
            if arguments[0] == "cancel" { try await player.activation.answerForCommand(nil) }
            else {
                try await player.performMutation {
                    try await player.activation.answerForCommand(arguments[0] == "keep" ? .keep : .clear)
                }
            }
        case .help:
            let definitions = CommandLanguage.registry.filter { arguments.isEmpty || $0.id.rawValue == arguments[0] }
            player.message = definitions.map { "\($0.usage) — \($0.summary)" }.joined(separator: "\n")
        }
    }

    private var grid: AlbumGridModel { lists.albumGrid(player: player, navigation: navigation) }

    private var currentList: ListModel? {
        lists.current(player: player, navigation: navigation)
    }

    private func requireConnection() throws {
        guard player.connected, player.snapshot != nil else {
            throw CommandLanguageError("Disconnected. Connect before running this command.")
        }
    }

    /// Radio lists saved stations, not tracks, so the selection commands have nothing to act on.
    private func requireTrackSelection() throws {
        guard navigation.destination != .radio else { throw CommandLanguageError(Self.radioHasNoSelection) }
    }

    private static let radioHasNoSelection = "Radio has no track selection. Play or enqueue a station from its row."

    private func requireList() throws -> ListModel {
        try requireConnection()
        guard let list = currentList else { throw CommandLanguageError("This command requires a track list.") }
        list.sync()
        return list
    }

    private func requireAlbums() throws {
        try requireConnection()
        guard navigation.destination == .albums else { throw CommandLanguageError("Open Albums before running this command.") }
        grid.sync()
    }

    private func destination(_ value: String) throws -> Destination {
        switch value {
        case "songs": return .songs
        case "albums": return .albums
        case "artists": return .artists
        case "genres": return .genres
        case "recent": return .recent
        case "history": return .history
        case "up-next": return .upNext
        case "radio": return .radio
        default: throw CommandLanguageError("Unknown destination.")
        }
    }

    private func flag(_ value: String, current: Bool) -> Bool { value == "toggle" ? !current : value == "on" }

    private func identifier(_ value: String) throws -> UInt64 {
        guard let id = UInt64(value) else { throw CommandLanguageError("Expected an unsigned integer ID.") }
        return id
    }

    private struct Selection {
        let request: SelectionRequest
        let source: QueueSource
        let count: Int
    }

    private func selectedAlbum() throws -> AlbumSummary {
        guard let id = grid.selectedID else { throw CommandLanguageError("Select an album first.") }
        if let expansion = grid.expansion, expansion.albumID == id { return expansion.album }
        guard let position = grid.cache.position(of: id), let album = grid.album(at: position) else {
            throw CommandLanguageError("Wait for the selected album to load.")
        }
        return album
    }

    private func selectedTracks(asRun: Bool = false) async throws -> Selection {
        try requireConnection()
        if let list = currentList {
            list.sync()
            guard !list.selectedPositions.isEmpty else { throw CommandLanguageError("Select one or more tracks first.") }
            return Selection(request: try list.selectionRequest(positions: list.selectedPositions, asRun: asRun),
                             source: list.queueSource, count: list.selectedPositions.count)
        }
        switch navigation.destination {
        case .albums:
            grid.sync()
            if let expansion = grid.expansion, !expansion.selectedPositions.isEmpty {
                return Selection(request: try grid.selectionRequest(expansion, positions: expansion.selectedPositions, asRun: asRun),
                                 source: AlbumPlayback.source(title: expansion.album.title), count: expansion.selectedPositions.count)
            }
            let album = try selectedAlbum()
            guard let request = grid.wholeAlbum(album.id) else { throw EmptySelection() }
            return Selection(request: request, source: AlbumPlayback.source(title: album.title), count: Int(clamping: album.track_count))
        case .history:
            let history = lists.history(player: player)
            history.sync()
            guard let id = history.selectedID, let position = history.cache.position(of: id),
                  let entry = history.cache.loadedRow(at: position) else { throw CommandLanguageError("Select a loaded history entry first.") }
            guard let trackID = entry.track_id, let revision = player.snapshot?.library.revision else {
                throw CommandLanguageError("“\(entry.title)” is no longer in the library.")
            }
            return Selection(request: .source(HistoryPlayback.selection(trackID: trackID, libraryRevision: revision), player: player),
                             source: HistoryPlayback.source, count: 1)
        case .upNext:
            let entries = try await queueEntries(selectedOnly: true)
            guard entries.count <= 500 else { throw CommandLanguageError("Select 500 or fewer queue entries to add to a playlist.") }
            let ids = entries.compactMap(\.track_id)
            guard ids.count == entries.count, let revision = player.snapshot?.library.revision else {
                throw CommandLanguageError("A selected queue entry is no longer in the library.")
            }
            return Selection(request: .source(.ids(libraryRevision: revision, trackIDs: ids), player: player),
                             source: RunSource.queueSource(list: .library, name: "Up Next"), count: ids.count)
        case .radio:
            throw CommandLanguageError(Self.radioHasNoSelection)
        default:
            throw CommandLanguageError("Wait for the current list to load, then select tracks.")
        }
    }

    private func playSelection(_ id: CommandID) async throws {
        try await player.performMutation {
            if navigation.destination == .upNext {
                guard id == .selectionPlay || id == .selectionRun else {
                    throw CommandLanguageError("Use selection-move to reorder entries already in Up Next.")
                }
                let entries = try await queueEntries(selectedOnly: true)
                try await player.upNext.execute(id, ids: entries.map(\.id))
                return
            }
            let selection = try await selectedTracks(asRun: id == .selectionRun)
            let action: ActivationAction = id == .selectionNext ? .playNext : (id == .selectionEnqueue ? .addToUpNext : .play)
            try await player.activation.execute(EnqueueJob(action: action, source: selection.source, request: selection.request))
        }
    }

    private func playlistCommand(_ command: ParsedCommand) async throws {
        try await player.performMutation {
            let args = command.arguments
            let playlist = try await player.savedPlaylists.named(args[0])
            switch command.id {
            case .playlistRename:
                try await player.savedPlaylists.rename(playlist, name: args[1])
            case .playlistDelete:
                try await player.savedPlaylists.delete(playlist)
                if navigation.destination == .playlist(playlist.id) { navigation.destination = .songs }
            case .playlistPlay, .playlistEnqueue:
                // Do not use enqueue_playlist's unconditional play: the common activation path
                // preserves the Ask/Keep/Replace policy and the queue revision it decided from.
                let list = lists.playlist(playlist, player: player, navigation: navigation)
                guard let revision = player.snapshot?.library.revision else {
                    throw CommandLanguageError("Disconnected. Nothing was queued.")
                }
                let description = ListDescription(source: PlaylistsModel.listSource(playlist))
                let selection = SelectionEncoder.source(list: description, revision: revision)
                try await player.activation.execute(EnqueueJob(
                    action: command.id == .playlistPlay ? .play : .addToUpNext,
                    source: list.queueSource, request: .source(selection, player: player)))
            case .playlistAdd:
                guard !playlist.isSmart else { throw CommandLanguageError("Tracks cannot be added to a smart playlist.") }
                let selected = try await selectedTracks()
                try await player.savedPlaylists.addSelection(to: playlist, selection: selected.request.selection,
                    count: selected.count, retarget: selected.request.retarget)
            case .smartUpdate:
                guard playlist.isSmart else { throw CommandLanguageError("That playlist is not smart.") }
                _ = try await player.savedPlaylists.saveSmart(playlist, name: playlist.name, rule: args[1])
            case .smartSnapshot:
                guard playlist.isSmart else { throw CommandLanguageError("That playlist is not smart.") }
                _ = try await player.savedPlaylists.snapshotSmart(playlist, name: args[1])
            default:
                throw CommandLanguageError("This command does not edit a playlist.")
            }
        }
    }

    private func editSelection(_ command: ParsedCommand) async throws {
        try await player.performMutation {
            let before = command.arguments.first.flatMap { $0 == "end" ? nil : UInt64($0) }
            if navigation.destination == .upNext {
                let entries = try await queueEntries(selectedOnly: true)
                try await player.upNext.execute(command.id, ids: entries.map(\.id), before: before)
                return
            }
            let list = try requireList()
            guard case .playlist(let id) = navigation.destination,
                  let playlist = player.savedPlaylists.playlist(id: id), !playlist.isSmart else {
                throw CommandLanguageError("Entries can only be removed or moved in a regular playlist or Up Next.")
            }
            let ids: [UInt64]
            switch PlaylistRemoval.plan(positions: list.selectedPositions, entryID: { list.cache.loadedRow(at: $0)?.row_id }) {
            case .nothing: throw CommandLanguageError("Select playlist entries first.")
            case .notLoaded: throw CommandLanguageError(PlaylistRemoval.notLoadedStatus)
            case .tooMany: throw CommandLanguageError(PlaylistRemoval.tooManyStatus)
            case .remove(let entries): ids = entries
            }
            if command.id == .selectionRemove {
                try await player.savedPlaylists.remove(in: playlist, entryIDs: ids)
                list.selectForCommand([])
            } else {
                guard list.allowsReorder else { throw CommandLanguageError("Set Group to None and clear sorting to reorder.") }
                guard !ids.contains(before ?? 0) else { throw CommandLanguageError("The insertion point is inside the selection.") }
                try await player.savedPlaylists.move(in: playlist, entryIDs: ids, before: before)
            }
        }
    }

    private func selectAll(_ all: Bool) async throws {
        try requireConnection()
        if let list = currentList {
            list.sync()
            guard !all || list.cache.revision != nil else { throw CommandLanguageError("Wait for the list to load.") }
            list.selectForCommand(all ? IndexSet(integersIn: 0..<list.cache.total) : [])
            return
        }
        switch navigation.destination {
        case .albums:
            grid.sync()
            if let expansion = grid.expansion {
                guard !all || expansion.isLoaded else { throw CommandLanguageError("Wait for the album tracks to load.") }
                guard !all || expansion.total == expansion.tracks.count else {
                    throw CommandLanguageError("Only part of this album is displayed. Select the album to act on all its tracks.")
                }
                expansion.selectedRowIDs = all ? Set(expansion.tracks.map(\.row_id)) : []
                if !all { grid.selectedID = nil }
            } else if all {
                throw CommandLanguageError("The album grid selects one album at a time. Expand an album to select all its tracks.")
            } else { grid.selectedID = nil }
        case .upNext:
            player.upNext.selection = all ? Set(try await queueEntries(selectedOnly: false).map(\.id)) : []
        case .history:
            guard !all else { throw CommandLanguageError("History selects one play at a time.") }
            lists.history(player: player).selectedID = nil
        default:
            throw CommandLanguageError("This view has no selectable rows.")
        }
    }

    /// Queue selection is keyed by entry ID, including offscreen entries. Reading bounded,
    /// revision-pinned pages avoids silently dropping selections outside the cache windows.
    private func queueEntries(selectedOnly: Bool) async throws -> [QueueItem] {
        try requireConnection()
        let selected = player.upNext.selection
        guard !selectedOnly || !selected.isEmpty else { throw CommandLanguageError("Select queue entries first.") }
        guard let revision = player.snapshot?.queue_revision else { throw CommandLanguageError("Wait for Up Next to load.") }
        if selectedOnly, player.upNext.cache.revision == revision {
            let cached = player.upNext.cache.loadedRows().compactMap { selected.contains($0.row.id) ? $0.row : nil }
            if cached.count == selected.count { return cached }
        }
        var result: [QueueItem] = []
        var offset = 0
        while true {
            let page: Page<QueueItem> = try await player.send("queue_page", [
                "revision": .uint(revision), "offset": .int(offset), "limit": .int(500)])
            if selectedOnly {
                for entry in page.entries where selected.contains(entry.id) { result.append(entry) }
            } else { result.append(contentsOf: page.entries) }
            offset += page.entries.count
            if (selectedOnly && result.count == selected.count) || offset >= page.total || page.entries.isEmpty { break }
        }
        guard !selectedOnly || result.count == selected.count else { throw ListChangedError() }
        return result
    }

    private func revealPlaying() throws {
        try requireConnection()
        guard let now = player.snapshot?.now_playing else { throw CommandLanguageError("Nothing is playing.") }
        if let list = currentList {
            guard list.cache.loadedRows().contains(where: { $0.row.track.id == now.track_id }) else {
                throw CommandLanguageError("The playing track is not among the loaded rows of this list.")
            }
            list.revealPlaying()
        } else if navigation.destination == .upNext {
            try player.upNext.revealPlaying()
        } else { throw CommandLanguageError("Reveal Playing requires a track list or Up Next.") }
    }

    private func revealSelection(artist: Bool) async throws {
        try requireConnection()
        if let list = currentList {
            guard let track = list.firstSelectedTrack else { throw CommandLanguageError("Select a loaded track first.") }
            try await reveal(artist ? .artist(track.album_artist_id) : .album(track.album_id))
        } else if navigation.destination == .albums {
            grid.sync()
            if let expansion = grid.expansion, let position = expansion.selectedPositions.first {
                let track = expansion.tracks[position].track
                try await reveal(artist ? .artist(track.album_artist_id) : .album(track.album_id))
            } else {
                let album = try selectedAlbum()
                try await reveal(artist ? .artist(album.album_artist_id) : .album(album.id))
            }
        } else if navigation.destination == .upNext {
            let entries = try await queueEntries(selectedOnly: true)
            guard let albumID = entries.first?.album_id else { throw CommandLanguageError("The selected entry has no library album.") }
            if artist {
                let reply: AlbumSummariesResponse = try await player.send("album_summaries", ["album_ids": .uints([albumID]), "revision": .null])
                guard let album = reply.entries.first else { throw CommandLanguageError("That album is no longer in the library.") }
                try await reveal(.artist(album.album_artist_id))
            } else { try await reveal(.album(albumID)) }
        } else { throw CommandLanguageError("Show in Album and Show in Artist require a library track or album selection.") }
    }

    private func reveal(_ target: NavigationModel.RevealTarget) async throws {
        // Use the same destination models even when no SwiftUI observer is mounted (smoke and
        // future command surfaces); no duplicate selection state is introduced.
        switch target {
        case .album(let id):
            navigation.destination = .albums
            try await grid.revealAwaitingReply(albumID: id)
        case .artist(let id):
            navigation.destination = .artists
            lists.browse(.artists, player: player, navigation: navigation).select([id])
        }
        player.endImport()
    }

    private func setBrowser(_ command: ParsedCommand) async throws {
        let list = try requireList()
        guard list.usesBrowser else { throw CommandLanguageError("The current list has no column browser.") }
        let ids: Set<UInt64>
        if command.arguments[0] == "all" { ids = [] }
        else {
            let id = try identifier(command.arguments[0])
            try await validateBrowserID(id, kind: command.id)
            ids = [id]
        }
        navigation.columnBrowserVisible = true
        switch command.id {
        case .browserGenre: list.setBrowserGenres(ids)
        case .browserArtist: list.setBrowserArtists(ids)
        case .browserAlbum: list.setBrowserAlbums(ids)
        default: throw CommandLanguageError("Unknown browser column.")
        }
    }

    private func validateBrowserID(_ id: UInt64, kind: CommandID) async throws {
        if kind == .browserAlbum {
            let reply: AlbumSummariesResponse = try await player.send("album_summaries", ["album_ids": .uints([id]), "revision": .null])
            guard reply.entries.contains(where: { $0.id == id }) else { throw CommandLanguageError("Unknown album ID \(id).") }
            return
        }
        var offset = 0
        var revision: UInt64?
        while true {
            var fields: [String: JSONValue] = ["query": .string(""), "offset": .int(offset), "limit": .int(500),
                                               "revision": revision.map(JSONValue.uint) ?? .null]
            let entries: [NamedCount], total: Int
            if kind == .browserArtist {
                fields["list"] = .null
                let reply: ListArtistsResponse = try await player.send("list_artists", fields)
                if id == BrowserSelection.compilationsID, reply.compilations != nil { return }
                entries = reply.entries; total = Int(reply.total); revision = reply.revision
            } else {
                let reply: ListGenresResponse = try await player.send("list_genres", fields)
                entries = reply.entries; total = Int(reply.total); revision = reply.revision
            }
            if entries.contains(where: { $0.id == id }) { return }
            offset += entries.count
            if offset >= total || entries.isEmpty { break }
        }
        throw CommandLanguageError("Unknown browser ID \(id).")
    }
}
