import SwiftUI
import TyrannyCore

/// The Up Next drawer (⌥⌘U, 244 pt): the now-playing card, the upcoming entries, and a target
/// that appends dropped tracks.
struct UpNextDrawer: View {
    static let width: CGFloat = 244

    let model: UpNextModel
    let player: PlayerModel

    var body: some View {
        VStack(spacing: 0) {
            card
            Divider()
            upcomingList
            Divider()
            dropTarget
        }
        .frame(width: Self.width)
        .background(.background)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("up-next-drawer")
    }

    // MARK: Now playing

    private var card: some View {
        let playing = player.snapshot?.now_playing
        return HStack(spacing: 10) {
            ArtworkView(store: player.artwork, request: player.artwork.nowPlayingRequest,
                        identifier: "up-next-drawer-art", dimension: 52)
            VStack(alignment: .leading, spacing: 2) {
                if let playing {
                    Text(playing.title.isEmpty ? "Unknown Title" : playing.title)
                        .font(.system(size: 12, weight: .semibold)).lineLimit(1)
                        .accessibilityIdentifier("up-next-drawer-title")
                    Text(playing.artist)
                        .font(.system(size: 11)).foregroundStyle(.secondary).lineLimit(1)
                        .accessibilityIdentifier("up-next-drawer-artist")
                    Text(albumLine(playing))
                        .font(.system(size: 11)).foregroundStyle(.secondary).lineLimit(1)
                        .accessibilityIdentifier("up-next-drawer-album")
                } else {
                    Text("Nothing playing")
                        .font(.system(size: 12)).foregroundStyle(.secondary)
                        .accessibilityIdentifier("up-next-drawer-title")
                }
            }
            Spacer(minLength: 0)
        }
        .padding(10)
        .task(id: playing?.album_id) {
            if let album = playing?.album_id { model.facts.want([album]) } else { model.facts.want([]) }
        }
    }

    /// "Album · Year", with the year once the album's summary arrives.
    private func albumLine(_ playing: NowPlaying) -> String {
        _ = model.facts.generation
        let year = playing.album_id.flatMap { model.facts.summary($0)?.year }.map { String($0) }
        return [playing.album.isEmpty ? nil : playing.album, year].compactMap { $0 }.joined(separator: " · ")
    }

    // MARK: Upcoming

    private var upcomingList: some View {
        let rows = model.upcoming(limit: UpNextModel.drawerLimit)
        let more = max(0, (model.upcomingCount ?? rows.count) - rows.count)
        return ScrollView {
            LazyVStack(alignment: .leading, spacing: 0) {
                ForEach(rows) { row in
                    entry(row)
                }
                if more > 0 {
                    Text("and \(DisplayFormat.count(more)) more…")
                        .font(.caption).foregroundStyle(.secondary)
                        .padding(.horizontal, 10).padding(.vertical, 6)
                        .accessibilityIdentifier("up-next-drawer-more")
                }
            }
        }
        .overlay {
            if rows.isEmpty {
                Text("Nothing up next")
                    .font(.callout).foregroundStyle(.secondary)
                    .allowsHitTesting(false)
                    .accessibilityIdentifier("up-next-drawer-empty")
            }
        }
        .frame(maxHeight: .infinity)
    }

    private func entry(_ item: QueueItem) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            VStack(alignment: .leading, spacing: 1) {
                Text(item.title.isEmpty ? (item.path as NSString).lastPathComponent : item.title)
                    .font(.system(size: 12)).lineLimit(1)
                Text(item.artist)
                    .font(.system(size: 11)).foregroundStyle(.secondary).lineLimit(1)
            }
            Spacer(minLength: 0)
            Text(item.duration_ms.map { DisplayFormat.clock(Double($0) / 1000) } ?? "")
                .font(.system(size: 11).monospacedDigit()).foregroundStyle(.secondary)
        }
        .padding(.horizontal, 10).padding(.vertical, 4)
        .contentShape(Rectangle())
        .onTapGesture(count: 2) { model.play(item.id) }
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("up-next-drawer-row-\(item.id)")
    }

    // MARK: Drop target

    private var dropTarget: some View {
        Text("Drop tracks here")
            .font(.callout)
            .foregroundStyle(.secondary)
            .frame(maxWidth: .infinity, minHeight: 44)
            .overlay(
                RoundedRectangle(cornerRadius: 6)
                    .strokeBorder(style: StrokeStyle(lineWidth: 1, dash: [4, 3]))
                    .foregroundStyle(.tertiary)
            )
            .padding(10)
            // Dropped tracks are appended to the queue.
            .trackDragHover(model: player, destination: .queue, hover: {},
                            drop: { player.dropTracks(token: $0, destination: .queue) })
            .accessibilityElement(children: .ignore)
            .accessibilityLabel("Drop tracks here")
            .accessibilityIdentifier("up-next-drawer-drop")
    }
}
