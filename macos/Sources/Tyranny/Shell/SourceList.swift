import SwiftUI
import TyrannyCore

/// The native sidebar: LIBRARY, PLAYING and PLAYLISTS. Playlists arrive in Task 12.
struct SourceList: View {
    let model: PlayerModel
    @Bindable var navigation: NavigationModel
    @State private var counts = SourceCounts()

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
                EmptyView()
            }
        }
        .listStyle(.sidebar)
        .accessibilityIdentifier("source-list")
        .task(id: CountKey(connection: model.playlistConnectionGeneration,
                           connected: model.connected, revision: model.snapshot?.library.revision)) {
            await counts.refresh(model)
        }
    }

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

    private func count(_ destination: Destination) -> UInt64? {
        guard model.connected, let library = model.snapshot?.library else { return nil }
        switch destination {
        case .songs: return UInt64(library.tracks)
        case .albums: return counts.albums
        case .artists: return counts.artists
        case .genres: return counts.genres
        // Up Next counts need the queue's played state (Task 11); History has none.
        case .recent, .upNext, .history, .playlist: return nil
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
