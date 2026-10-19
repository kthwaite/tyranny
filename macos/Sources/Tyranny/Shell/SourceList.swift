import SwiftUI
import TyrannyCore

/// The native sidebar: LIBRARY, PLAYING and PLAYLISTS.
///
/// Tracks dropped on Up Next are appended to the queue. Tracks dropped on a regular playlist
/// are appended to it; hovering a playlist during a drag opens it, to choose an insertion
/// point. Smart playlists refuse drops.
struct SourceList: View {
    let model: PlayerModel
    @Bindable var navigation: NavigationModel
    @State private var counts = SourceCounts()
    @State private var sheet: PlaylistSheet?
    @State private var deleting: Playlist?

    private var playlists: PlaylistsModel { model.savedPlaylists }

    /// The sheets the "+" menu and the row menus open.
    enum PlaylistSheet: Identifiable {
        case create, createSmart, rename(Playlist), editRule(Playlist)

        var id: String {
            switch self {
            case .create: "create"
            case .createSmart: "create-smart"
            case .rename(let playlist): "rename-\(playlist.id)"
            case .editRule(let playlist): "rule-\(playlist.id)"
            }
        }
    }

    var body: some View {
        List(selection: Binding<Destination?>(
            get: { navigation.destination },
            set: { if let next = $0 { navigation.destination = next } }
        )) {
            Section("Library") {
                ForEach(Destination.library, id: \.self) { row($0) }
            }
            Section("Playing") {
                ForEach(Destination.playing, id: \.self) { row($0) }
            }
            Section("Playlists") {
                ForEach(playlists.rows) { playlistRow($0) }
            }
        }
        .listStyle(.sidebar)
        .accessibilityIdentifier("source-list")
        .safeAreaInset(edge: .bottom, spacing: 0) { addBar }
        .task(id: CountKey(connection: model.playlistConnectionGeneration,
                           connected: model.connected, revision: model.snapshot?.library.revision)) {
            await counts.refresh(model)
        }
        .onChange(of: playlists.cache.generation, initial: true) { _, _ in playlists.loadAll() }
        .sheet(item: $sheet) { sheet in
            switch sheet {
            case .create:
                PlaylistNameSheet(playlists: playlists, mode: .create, created: open)
            case .createSmart:
                SmartPlaylistEditor(playlists: playlists, player: model, playlist: nil, created: open)
            case .rename(let playlist):
                PlaylistNameSheet(playlists: playlists, mode: .rename(playlist))
            case .editRule(let playlist):
                SmartPlaylistEditor(playlists: playlists, player: model, playlist: playlist)
            }
        }
        .alert("Delete \(deleting.map { "“\($0.name)”" } ?? "playlist")?", isPresented: Binding(
            get: { deleting != nil }, set: { if !$0 { deleting = nil } }), presenting: deleting
        ) { playlist in
            Button("Delete", role: .destructive) { delete(playlist) }
                .accessibilityIdentifier("playlist-delete-confirm")
            Button("Cancel", role: .cancel) {}
                .accessibilityIdentifier("playlist-delete-cancel")
        } message: { _ in
            Text("The playlist's entries are deleted. The queue and playback are not changed.")
        }
    }

    private func open(_ playlist: Playlist) { navigation.destination = .playlist(playlist.id) }

    private func delete(_ playlist: Playlist) {
        model.mutate {
            try await playlists.delete(playlist)
            if navigation.destination == .playlist(playlist.id) { navigation.destination = .songs }
        }
    }

    // MARK: Rows

    private func row(_ destination: Destination) -> some View {
        Label(destination.title, systemImage: destination.systemImage)
            .badge(count(destination).map { Text(DisplayFormat.count($0)) })
            .frame(maxWidth: .infinity, alignment: .leading)
            // Tracks dropped on Up Next are appended to the queue.
            .trackDragHover(model: model, enabled: destination == .upNext, destination: .queue, hover: {},
                            drop: destination == .upNext ? { model.dropTracks(token: $0, destination: .queue) } : nil)
            .tag(destination)
            .accessibilityElement(children: .combine)
            .accessibilityIdentifier(destination.identifier)
    }

    private func playlistRow(_ playlist: Playlist) -> some View {
        let destination = Destination.playlist(playlist.id)
        let accepts = PlaylistDrop.onRow(playlist.isSmart ? .smartPlaylist : .regularPlaylist) == .addSelection
        return Label(playlist.name, systemImage: playlist.isSmart ? "gearshape" : "music.note.list")
            .lineLimit(1)
            // Counts are for regular playlists; a smart playlist's size is its rule's business.
            .badge(playlist.isSmart ? nil : playlist.entry_count.map { Text(DisplayFormat.count(UInt64($0))) })
            .frame(maxWidth: .infinity, alignment: .leading)
            .trackDragHover(model: model, enabled: accepts, destination: .playlist(playlist.id),
                            hover: { navigation.destination = destination },
                            drop: accepts ? { model.dropTracks(token: $0, destination: .playlist(playlist.id)) } : nil)
            .tag(destination)
            .accessibilityElement(children: .combine)
            .accessibilityIdentifier(destination.identifier)
            .help(playlist.isSmart ? "Live rule results; edit the rule to change what it holds."
                                   : "Drop tracks to add them, or hold over it to open it and choose where.")
            .contextMenu {
                Button("Rename…") { sheet = .rename(playlist) }
                    .accessibilityIdentifier("source-playlist-rename")
                if playlist.isSmart {
                    Button("Edit Rule…") { sheet = .editRule(playlist) }
                        .accessibilityIdentifier("source-playlist-edit-rule")
                }
                Divider()
                Button("Delete…", role: .destructive) { deleting = playlist }
                    .accessibilityIdentifier("source-playlist-delete")
            }
    }

    private var addBar: some View {
        HStack {
            Menu {
                Button("New Playlist") { sheet = .create }
                    .accessibilityIdentifier("source-add-playlist")
                Button("New Smart Playlist…") { sheet = .createSmart }
                    .accessibilityIdentifier("source-add-smart-playlist")
            } label: {
                Image(systemName: "plus")
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .fixedSize()
            .disabled(!model.canMutate)
            .help("New Playlist")
            .accessibilityLabel("Add")
            .accessibilityIdentifier("source-add")
            Spacer()
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 6)
        .background(.bar)
    }

    private func count(_ destination: Destination) -> UInt64? {
        guard model.connected, let library = model.snapshot?.library else { return nil }
        switch destination {
        case .songs: return UInt64(library.tracks)
        case .albums: return counts.albums
        case .artists: return counts.artists
        case .genres: return counts.genres
        case .upNext: return model.upNext.upcomingCount.map { UInt64($0) }
        // History has no count.
        case .recent, .history, .playlist: return nil
        }
    }

    private struct CountKey: Equatable {
        let connection: UInt64
        let connected: Bool
        let revision: UInt64?
    }
}

/// Album, artist and genre totals for the sidebar badges. One-row list requests,
/// at most once a second while the library revision changes during a scan.
@MainActor @Observable
final class SourceCounts {
    private(set) var albums: UInt64?
    private(set) var artists: UInt64?
    private(set) var genres: UInt64?

    func refresh(_ model: PlayerModel) async {
        guard model.connected, let revision = model.snapshot?.library.revision else {
            albums = nil; artists = nil; genres = nil
            return
        }
        // Debounce: a newer revision cancels this task before it sends anything.
        do { try await Task.sleep(for: .seconds(1)) } catch { return }
        // A cancelled request would tear down the shared socket, so send outside
        // this cancellable task and drop stale replies instead.
        let reply = await Task { @MainActor () -> (UInt64, UInt64, UInt64)? in
            do {
                let list = try JSONValue.encoding(ListDescription(source: .library))
                let albums: ListAlbumsResponse = try await model.send("list_albums", [
                    "list": list, "sort": .string(AlbumSort.albumArtist.rawValue), "descending": .bool(false),
                    "revision": .uint(revision), "offset": .int(0), "limit": .int(1)])
                let artists: ListArtistsResponse = try await model.send("list_artists", [
                    "query": .string(""), "list": .null, "revision": .uint(revision), "offset": .int(0), "limit": .int(1)])
                let genres: ListGenresResponse = try await model.send("list_genres", [
                    "query": .string(""), "revision": .uint(revision), "offset": .int(0), "limit": .int(1)])
                return (albums.total, artists.total, genres.total)
            } catch {
                // A stale revision or lost connection: the next revision retries.
                return nil
            }
        }.value
        guard !Task.isCancelled, let reply, model.snapshot?.library.revision == revision else { return }
        (albums, artists, genres) = reply
    }
}
