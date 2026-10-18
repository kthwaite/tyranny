import SwiftUI

struct PlaylistView: View {
    @Bindable var model: PlayerModel
    @State private var editingName = false
    @State private var creating = false
    @State private var name = ""
    @State private var confirmingDelete = false
    @State private var dialogPlaylist: Playlist?
    @State private var editingRule = false
    @State private var snapshotting = false
    @State private var snapshotLibraryRevision: UInt64 = 0
    @State private var snapshotCatalogRevision: UInt64 = 0

    var body: some View {
        HSplitView {
            VStack(spacing: 8) {
                TextField("Filter playlists", text: $model.playlistFilter)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityIdentifier("playlist-filter")
                    .help("Literal playlist-name search. Use the separate entry filter to search tracks within a playlist.")
                PlaylistCatalog(model: model, identifier: "playlist-list")
                Button("Create Playlist…") { creating = true; name = ""; editingName = true }
                    .disabled(!model.canMutate)
                    .accessibilityIdentifier("playlist-create")
            }.padding(8).frame(minWidth: 220, idealWidth: 260, maxWidth: 320)
            VStack(spacing: 0) {
                if let playlist = model.currentPlaylist {
                    HStack {
                        Text(playlist.name).font(.headline)
                        Text(playlist.summary).font(.caption).foregroundStyle(.secondary)
                        Spacer()
                        Button("Rename…") { creating = false; dialogPlaylist = playlist; name = playlist.name; editingName = true }
                            .accessibilityIdentifier("playlist-rename")
                        Button("Delete…", role: .destructive) { dialogPlaylist = playlist; confirmingDelete = true }
                            .accessibilityIdentifier("playlist-delete")
                    }.padding(10).disabled(!model.canMutate || !model.playlistReady)
                    if playlist.isSmart {
                        HStack {
                            Button("Edit Rule…") { dialogPlaylist = playlist; editingRule = true }
                                .accessibilityIdentifier("playlist-edit-rule")
                            Button("Create Regular Playlist from Results…") {
                                dialogPlaylist = playlist
                                snapshotLibraryRevision = model.snapshot?.library.revision ?? 0
                                snapshotCatalogRevision = model.snapshot?.playlists_revision ?? 0
                                snapshotting = true
                            }.accessibilityIdentifier("playlist-snapshot")
                            Spacer()
                        }.padding(.horizontal, 10).padding(.bottom, 8)
                            .disabled(!model.canMutate || !model.playlistReady)
                    }
                    HStack {
                        TextField("Filter playlist entries", text: $model.playlistEntryFilter)
                            .textFieldStyle(.roundedBorder)
                            .accessibilityIdentifier("playlist-entry-filter")
                            .help("Filter title, artist or album. This does not change the playlist catalog or whole-playlist playback.")
                        TrackFilterHelp(identifier: "playlist-entry-filter-help")
                    }.padding(.horizontal, 10).padding(.bottom, 8)
                    if let error = model.playlistEntryFilterError {
                        Text("Filter: \(error) Edit or clear the query; see Filter syntax for examples.")
                            .foregroundStyle(.orange).textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading).padding(8)
                            .accessibilityIdentifier("playlist-entry-filter-error")
                    }
                    DragTrackTable(model: model, source: .playlist(playlist.id), identifier: "playlist-entry-table",
                        columns: [.init(title: "Title", width: 260), .init(title: "Artist", width: 180),
                            .init(title: "Album", width: 200)],
                        rows: (model.playlistEntries?.entries ?? []).map { entry in
                            .init(id: entry.id, cells: [entry.metadata.title
                                + (entry.available ? "" : " — Unavailable")
                                + (entry.error.map { " — \($0)" } ?? ""),
                                entry.metadata.artist, entry.metadata.album], help: entry.path)
                        }, selection: $model.selectedPlaylistEntries, activate: model.playPlaylistEntry)
                    .overlay {
                        if model.playlistEntriesLoading { ProgressView("Loading entries…") }
                        else if model.playlistEntries?.total == 0 {
                            Text(model.playlistEntryFilterActive ? "No matching entries. Edit or clear the filter."
                                : playlist.isSmart ? "No available tracks match this rule." : "This playlist is empty. Drop library tracks here.")
                                .foregroundStyle(.secondary).allowsHitTesting(false)
                        }
                    }
                    .allowsHitTesting(model.playlistReady)
                    .help(playlist.isSmart ? "Live results are read-only. Double-click to play a track."
                        : "Insert before a matching row. A drop after the final matching entry appends to the end of the full playlist, including hidden entries.")
                    if model.playlistEntryFilterActive && !playlist.isSmart {
                        Text("Drops after the final match append to the full playlist. Hidden entries keep their order.")
                            .font(.caption).foregroundStyle(.secondary).padding(.horizontal, 8).padding(.top, 6)
                    }
                    HStack {
                        Text("\(model.selectedPlaylistEntries.count) selected").font(.caption)
                        if !playlist.isSmart {
                        Button("Remove") { model.removePlaylistSelection() }
                            .disabled(model.selectedPlaylistEntries.isEmpty)
                            .accessibilityIdentifier("playlist-remove")
                        Button("Move Up") { model.movePlaylistSelection(-1) }
                            .disabled(!model.canMovePlaylist(-1)).accessibilityIdentifier("playlist-up")
                        Button("Move Down") { model.movePlaylistSelection(1) }
                            .disabled(!model.canMovePlaylist(1)).accessibilityIdentifier("playlist-down")
                        }
                        Spacer()
                        Button(model.playlistEntryFilterActive ? "Enqueue All Tracks" : "Enqueue") { model.enqueuePlaylist(play: false) }
                            .disabled(playlist.isSmart ? (!model.playlistEntryFilterActive && model.playlistEntries?.total == 0) : playlist.entry_count == 0)
                            .accessibilityIdentifier("playlist-enqueue")
                            .help("Enqueue every track in this saved playlist, including entries hidden by the filter.")
                        Button(model.playlistEntryFilterActive ? "Play All Tracks" : "Play") { model.enqueuePlaylist(play: true) }
                            .disabled(playlist.isSmart ? (!model.playlistEntryFilterActive && model.playlistEntries?.total == 0) : playlist.entry_count == 0)
                            .accessibilityIdentifier("playlist-play")
                            .help("Enqueue and play the full saved playlist, including hidden entries. Double-click a row to play only that track.")
                    }.padding(8).disabled(!model.canMutate || !model.playlistReady)
                    PlaylistPaging(model: model, destination: .playlist(playlist.id), offset: model.playlistEntries?.offset ?? 0,
                        count: model.playlistEntries?.entries.count ?? 0, total: model.playlistEntries?.total ?? 0,
                        loading: model.playlistEntriesLoading, prefix: "playlist-entries", action: model.pagePlaylistEntries)
                } else {
                    ContentUnavailableView("Choose a Playlist", systemImage: "music.note.list",
                        description: Text("Create a saved playlist, then add selected library tracks. Saved playlists are independent of the playback queue."))
                }
                if let error = model.playlistError {
                    Text(error).foregroundStyle(.orange).textSelection(.enabled).padding(8)
                        .accessibilityIdentifier("playlist-error")
                } else if model.currentPlaylist != nil && model.playlistEntryFilterError == nil
                    && !model.playlistReady && !model.playlistEntriesLoading && !model.playlistsLoading {
                    Text("Playlist data is stale. Refresh to load its current revision.").foregroundStyle(.orange).padding(8)
                }
            }.frame(minWidth: 600, maxWidth: .infinity)
        }
        .sheet(isPresented: $editingRule) {
            if let dialogPlaylist {
                SmartPlaylistEditor(model: model, playlist: dialogPlaylist, initialQuery: dialogPlaylist.query ?? "")
            }
        }
        .sheet(isPresented: $snapshotting) {
            if let dialogPlaylist {
                SmartPlaylistSnapshotSheet(model: model, playlist: dialogPlaylist,
                    libraryRevision: snapshotLibraryRevision, catalogRevision: snapshotCatalogRevision)
            }
        }
        .sheet(isPresented: $editingName) {
            VStack(alignment: .leading, spacing: 16) {
                Text(creating ? "Create Playlist" : "Rename Playlist").font(.headline)
                TextField("Playlist name", text: $name).textFieldStyle(.roundedBorder)
                    .accessibilityIdentifier("playlist-name")
                Text("Names must be unique and contain 1–256 UTF-8 bytes.").font(.caption).foregroundStyle(.secondary)
                HStack {
                    Spacer()
                    Button("Cancel") { editingName = false }.keyboardShortcut(.cancelAction)
                    Button(creating ? "Create" : "Rename") {
                        if creating { model.createPlaylist(name) }
                        else if let dialogPlaylist { model.renamePlaylist(dialogPlaylist, name: name) }
                        editingName = false
                    }.keyboardShortcut(.defaultAction)
                        .disabled(!validPlaylistName(name) || !model.canMutate)
                        .accessibilityIdentifier("playlist-name-save")
                }
            }.padding(24).frame(width: 380)
        }
        .confirmationDialog("Delete \(dialogPlaylist?.name ?? "playlist")?", isPresented: $confirmingDelete) {
            Button("Delete Playlist", role: .destructive) {
                if let dialogPlaylist { model.deletePlaylist(dialogPlaylist) }
            }
                .accessibilityIdentifier("playlist-delete-confirm")
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Saved entries will be deleted. Already-enqueued tracks and playback are not changed.")
        }
    }

}

struct PlaylistCatalog: View {
    @Bindable var model: PlayerModel
    let identifier: String

    var body: some View {
        VStack(spacing: 0) {
            List(selection: $model.selectedPlaylist) {
                ForEach(model.playlists?.entries ?? []) { playlist in
                    VStack(alignment: .leading) {
                        Label(playlist.name, systemImage: playlist.isSmart ? "line.3.horizontal.decrease.circle" : "music.note.list")
                        Text(playlist.summary).font(.caption).foregroundStyle(.secondary)
                    }.tag(playlist.id)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .contentShape(Rectangle())
                    .accessibilityIdentifier("playlist-drop-\(playlist.id)")
                    .trackDragHover(model: model, enabled: !model.playlistsLoading && !playlist.isSmart, destination: .playlist(playlist.id),
                        hover: { model.selectedPlaylist = playlist.id },
                        drop: { model.dropTracks(token: $0, destination: .playlist(playlist.id)) })
                    .help(playlist.isSmart ? "Live rule results; edit the rule to change membership."
                        : "Drop library tracks to append, or hover to open this playlist and choose an insertion point.")
                }
            }.accessibilityIdentifier(identifier)
                .id(model.playlistFilter)
                .overlay {
                    if model.playlistsLoading { ProgressView("Loading playlists…") }
                    else if model.playlists?.entries.isEmpty == true {
                        Text(model.playlistFilter.isEmpty ? "No saved playlists" : "No matching playlists").foregroundStyle(.secondary)
                    }
                }
            PlaylistPaging(model: model, destination: nil, offset: model.playlists?.offset ?? 0, count: model.playlists?.entries.count ?? 0,
                total: model.playlists?.total ?? 0, loading: model.playlistsLoading,
                prefix: identifier, action: model.pagePlaylists)
        }
    }
}

struct AddToPlaylistSheet: View {
    @Bindable var model: PlayerModel
    @Environment(\.dismiss) private var dismiss
    @State private var newName = ""
    @State private var creationError: String?
    @State private var query = ""
    @State private var page: Page<Playlist>?
    @State private var offset = 0
    @State private var selection: UInt64?
    @State private var loading = false
    @State private var loadError: String?

    private var selected: Playlist? { page?.entries.first { $0.id == selection } }
    private var requestKey: String {
        "\(query)|\(offset)|\(model.snapshot?.playlists_revision ?? 0)|\(model.connected)|\(model.playlistConnectionGeneration)"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Add \(model.selectedTracks.count) Selected Tracks to Playlist").font(.headline)
            HStack {
                TextField("New playlist name", text: $newName)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityIdentifier("playlist-picker-name")
                Button("Create & Add Tracks") {
                    creationError = nil
                    model.createPlaylistAddingSelection(newName) { error in
                        if let error { creationError = error }
                        else { dismiss() }
                    }
                }
                .disabled(!model.canMutate || !model.libraryReady || model.selectedTracks.isEmpty || !validPlaylistName(newName))
                .accessibilityIdentifier("playlist-picker-create-add")
            }
            .disabled(model.busy)
            if let creationError {
                Text(creationError).foregroundStyle(.orange).textSelection(.enabled)
                    .accessibilityIdentifier("playlist-picker-create-error")
            }
            Divider()
            TextField("Filter playlists", text: $query)
                .textFieldStyle(.roundedBorder).accessibilityIdentifier("playlist-picker-filter")
                .onChange(of: query) { _, _ in offset = 0; selection = nil }
            List(selection: $selection) {
                ForEach(page?.entries ?? []) { playlist in
                    VStack(alignment: .leading) {
                        Text(playlist.name)
                        Text(playlist.summary).font(.caption).foregroundStyle(.secondary)
                    }.tag(playlist.id)
                }
            }.accessibilityIdentifier("playlist-picker-list")
                .overlay { if loading { ProgressView("Loading playlists…") } }
                .onChange(of: selection) { _, _ in if let selected { model.selectPlaylist(selected) } }
            PlaylistPaging(model: model, destination: nil, offset: page?.offset ?? 0,
                count: page?.entries.count ?? 0, total: page?.total ?? 0, loading: loading,
                prefix: "playlist-picker-list") { delta in
                    offset = max(0, offset + delta * PlayerModel.pageSize); selection = nil
                }
            if page?.total == 0 && query.isEmpty {
                Text("Create a regular playlist above to add your selected tracks.").foregroundStyle(.secondary)
            }
            if let loadError { Text(loadError).foregroundStyle(.orange).textSelection(.enabled) }
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction).disabled(model.busy)
                Button("Add Tracks") {
                    if let selected { model.addSelectionToPlaylist(selected); dismiss() }
                }.disabled(!model.canMutate || !model.libraryReady || selected == nil || loading
                    || page?.revision != model.snapshot?.playlists_revision || model.selectedTracks.isEmpty)
                    .accessibilityIdentifier("playlist-picker-add")
            }
        }.padding(20).frame(width: 500, height: 500)
            .interactiveDismissDisabled(model.busy)
            .task(id: requestKey) {
                let key = requestKey
                loading = true; loadError = nil; page = nil
                do {
                    let result = try await model.regularPlaylistPage(query: query, offset: offset)
                    guard !Task.isCancelled, key == requestKey else { return }
                    if result.entries.isEmpty && offset > 0 { offset = max(0, offset - PlayerModel.pageSize) }
                    else { page = result }
                } catch {
                    guard !Task.isCancelled, key == requestKey else { return }
                    loadError = error.localizedDescription
                }
                loading = false
            }
    }
}

private func validPlaylistName(_ name: String) -> Bool {
    let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
    return !trimmed.isEmpty && trimmed.utf8.count <= 256 && !trimmed.contains("\0")
}

private struct PlaylistPaging: View {
    let model: PlayerModel
    let destination: TrackDragSource?
    let offset: Int
    let count: Int
    let total: Int
    let loading: Bool
    let prefix: String
    let action: (Int) -> Void

    var body: some View {
        HStack {
            Button("Previous") { action(-1) }.disabled(offset == 0 || loading)
                .accessibilityIdentifier("\(prefix)-previous")
                .trackDragHover(model: model, enabled: offset > 0 && !loading, destination: destination) { action(-1) }
            Spacer()
            Text(total == 0 ? "0 items" : "\(offset + 1)–\(offset + count) of \(total)").font(.caption).monospacedDigit()
                .accessibilityIdentifier("\(prefix)-range")
            Spacer()
            Button("Next") { action(1) }.disabled(offset + count >= total || loading)
                .accessibilityIdentifier("\(prefix)-next")
                .trackDragHover(model: model, enabled: offset + count < total && !loading, destination: destination) { action(1) }
        }.padding(8)
    }
}

struct SmartPlaylistEditor: View {
    @Bindable var model: PlayerModel
    let playlist: Playlist?
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var query: String
    @State private var preview: Page<Track>?
    @State private var previewQuery: String?
    @State private var previewError: String?
    @State private var saveError: String?
    @State private var loading = true
    @State private var connectionGeneration: UInt64

    init(model: PlayerModel, playlist: Playlist?, initialQuery: String) {
        self.model = model; self.playlist = playlist
        _query = State(initialValue: initialQuery)
        _connectionGeneration = State(initialValue: model.playlistConnectionGeneration)
    }

    private var previewKey: String {
        "\(model.connected)|\(model.playlistConnectionGeneration)|\(model.snapshot?.library.revision ?? 0)|\(query)"
    }
    private var ready: Bool {
        !loading && previewError == nil && previewQuery == query
            && preview?.revision == model.snapshot?.library.revision
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(playlist == nil ? "Save as Smart Playlist" : "Edit Smart Playlist Rule").font(.headline)
            if playlist == nil {
                TextField("Playlist name", text: $name).textFieldStyle(.roundedBorder)
                    .accessibilityIdentifier("smart-name")
            } else { Text(playlist?.name ?? "").foregroundStyle(.secondary) }
            Text("This rule searches the entire available library, not the selected artist or album pane. An empty rule includes all available tracks. Results update when the library changes.")
                .font(.callout)
            HStack {
                TextField("Library rule", text: $query).textFieldStyle(.roundedBorder)
                    .accessibilityIdentifier("smart-query")
                TrackFilterHelp(identifier: "smart-filter-help")
            }
            if let previewError {
                Text(previewError).foregroundStyle(.orange).textSelection(.enabled)
                    .accessibilityIdentifier("smart-preview-error")
            }
            if ready, let preview {
                Text("\(preview.total) matching tracks across the library")
                    .accessibilityIdentifier("smart-preview-count").accessibilityValue("\(preview.total)")
                if preview.total == 0 { Text("No available tracks match. You can still save this rule.").foregroundStyle(.secondary) }
                List(preview.entries) { track in
                    VStack(alignment: .leading) {
                        Text(track.metadata.title)
                        Text("\(track.metadata.artist) — \(track.metadata.album)").font(.caption).foregroundStyle(.secondary)
                    }
                }
                Text("Preview shows up to \(PlayerModel.pageSize) tracks; the saved rule includes all matches.")
                    .font(.caption).foregroundStyle(.secondary)
            } else if loading { ProgressView("Previewing rule…").frame(maxWidth: .infinity, maxHeight: .infinity) }
            else { Spacer(minLength: 0) }
            if let saveError { Text(saveError).foregroundStyle(.orange).textSelection(.enabled) }
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction).accessibilityIdentifier("smart-cancel")
                Button("Save") {
                    saveError = nil
                    model.saveSmartPlaylist(playlist, name: name, query: query, connectionGeneration: connectionGeneration) { error in
                        if let error { saveError = error } else { dismiss() }
                    }
                }.keyboardShortcut(.defaultAction).accessibilityIdentifier("smart-save")
                    .disabled(!model.canMutate || !ready || (playlist == nil && !validPlaylistName(name)))
            }.disabled(model.busy)
        }.padding(24).frame(width: 600, height: 540)
            .interactiveDismissDisabled(model.busy)
            .task(id: previewKey) {
                let key = previewKey, rule = query
                loading = true; preview = nil; previewQuery = nil; previewError = nil
                do {
                    try await Task.sleep(for: .milliseconds(250))
                    let page = try await model.previewSmartRule(rule)
                    guard !Task.isCancelled, key == previewKey else { return }
                    preview = page; previewQuery = rule
                } catch {
                    guard !Task.isCancelled, key == previewKey else { return }
                    previewError = error.localizedDescription
                }
                loading = false
            }
    }
}

private struct SmartPlaylistSnapshotSheet: View {
    @Bindable var model: PlayerModel
    let playlist: Playlist
    let libraryRevision: UInt64
    let catalogRevision: UInt64
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var error: String?
    @State private var connectionGeneration: UInt64?

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Create Regular Playlist from Results").font(.headline)
            Text("Copy all current results of “\(playlist.name)” into an editable regular playlist, including results hidden by the entry filter. Empty results are allowed. Later library or rule changes will not change the copy.")
            TextField("Playlist name", text: $name).textFieldStyle(.roundedBorder)
                .accessibilityIdentifier("snapshot-name")
            if let error { Text(error).foregroundStyle(.orange).textSelection(.enabled) }
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
                    .accessibilityIdentifier("snapshot-cancel")
                Button("Create") {
                    model.snapshotSmartPlaylist(playlist, libraryRevision: libraryRevision,
                        catalogRevision: catalogRevision, name: name,
                        connectionGeneration: connectionGeneration ?? model.playlistConnectionGeneration) { failure in
                        if let failure { error = failure } else { dismiss() }
                    }
                }.keyboardShortcut(.defaultAction).accessibilityIdentifier("snapshot-create")
                    .disabled(!model.canMutate || !validPlaylistName(name))
            }.disabled(model.busy)
        }.padding(24).frame(width: 500).interactiveDismissDisabled(model.busy)
            .onAppear { if connectionGeneration == nil { connectionGeneration = model.playlistConnectionGeneration } }
    }
}
