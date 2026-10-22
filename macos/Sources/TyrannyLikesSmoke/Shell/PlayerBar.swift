import SwiftUI
import TyrannyLikesSmokeCore

/// The full-width bar above the status bar. Left: art thumbnail, title, artist and album.
/// Centre: shuffle, previous, play/pause, next, repeat, and the scrubber. Right: format and
/// gain, the Up Next drawer toggle, and volume. The left and right sections are fixed; the
/// scrubber shrinks first. A playback error replaces the left section.
struct PlayerBar: View {
    @Environment(\.themeColours) private var colours
    let model: PlayerModel
    @Bindable var navigation: NavigationModel
    let sidebarVisible: Bool
    @State private var seekPreview: Double?
    @State private var showingError = false
    @State private var hoveringArt = false

    static let height: CGFloat = 74
    private static let sideWidth: CGFloat = 250

    private var transport: TransportModel { model.transport }
    private var nowPlaying: NowPlaying? { transport.nowPlaying }
    private var artInBar: Bool {
        NowPlayingArt.placement(expanded: navigation.nowPlayingArtExpanded, sidebarVisible: sidebarVisible) == .bar
    }

    var body: some View {
        HStack(spacing: 14) {
            leading.frame(width: Self.sideWidth, alignment: .leading)
            controls.frame(maxWidth: .infinity)
            trailing.frame(width: Self.sideWidth, alignment: .trailing)
        }
        .padding(.horizontal, 14)
        .frame(height: Self.height)
        .frame(maxWidth: .infinity)
        .background(.bar)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("player-bar")
        .onChange(of: transport.currentID) { _, _ in seekPreview = nil }
        .onChange(of: transport.playbackSession) { _, _ in seekPreview = nil }
        .onChange(of: transport.canMutate) { _, enabled in if !enabled { seekPreview = nil } }
        .onChange(of: transport.error) { _, error in if error == nil { showingError = false } }
    }

    // MARK: - Left: now playing

    @ViewBuilder private var leading: some View {
        if let error = transport.error {
            errorSummary(error)
        } else {
            HStack(spacing: 10) {
                if artInBar { thumbnail }
                VStack(alignment: .leading, spacing: 1) {
                    HStack(spacing: 4) {
                        Button { navigation.revealPlaying() } label: {
                            Text(nowPlaying?.title ?? "").lineLimit(1)
                        }
                        .buttonStyle(.plain)
                        .font(.system(size: 13, weight: .semibold))
                        .disabled(nowPlaying?.track_id == nil)
                        .help("Show the playing track (⌘L)")
                        .accessibilityIdentifier("now-playing-title")
                        if let warning = transport.metadataError {
                            Image(systemName: "exclamationmark.triangle.fill").font(.system(size: 10))
                                .foregroundStyle(colours.warning).help("Track metadata: \(warning)")
                                .accessibilityIdentifier("now-playing-metadata-warning")
                        }
                    }
                    VStack(alignment: .leading, spacing: 1) {
                        RevealLink(text: transport.artist, target: nowPlaying?.artistLinkID.map { .artist($0) },
                                   help: "Show the artist", identifier: "now-playing-artist-link",
                                   navigation: navigation)
                        RevealLink(text: transport.album, target: nowPlaying?.album_id.map { .album($0) },
                                   help: "Show the album", identifier: "now-playing-album-link",
                                   navigation: navigation)
                    }
                    .font(.system(size: 11)).foregroundStyle(.secondary)
                    .accessibilityElement(children: .contain)
                    .accessibilityLabel(transport.artistAlbum)
                    .accessibilityIdentifier("now-playing-subtitle")
                    if artInBar { ArtworkAttributionView(artwork: model.artwork.nowPlaying) }
                }
                Spacer(minLength: 0)
            }
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier("now-playing-display")
        }
    }

    /// The 52 pt thumbnail. Hover shows ⤢; clicking moves the art into the sidebar.
    private var thumbnail: some View {
        ZStack {
            ArtworkView(store: model.artwork, request: model.artwork.nowPlayingRequest,
                        identifier: "now-playing-art", dimension: 52)
            Button { navigation.nowPlayingArtExpanded = true } label: {
                Color.clear
                    .contentShape(Rectangle())
                    .overlay(alignment: .topTrailing) {
                        if hoveringArt && nowPlaying != nil {
                            ArtBadge(systemImage: "arrow.up.left.and.arrow.down.right")
                        }
                    }
            }
            .buttonStyle(.plain)
            .disabled(nowPlaying == nil)
            .help("Show the artwork in the sidebar")
            .accessibilityLabel("Expand Artwork")
            .accessibilityIdentifier("now-playing-art-expand")
        }
        .frame(width: 52, height: 52)
        .onHover { hoveringArt = $0 }
    }

    private func errorSummary(_ error: String) -> some View {
        Button { showingError = true } label: {
            HStack(spacing: 8) {
                Image(systemName: "exclamationmark.triangle.fill").font(.system(size: 16)).foregroundStyle(colours.error)
                VStack(alignment: .leading, spacing: 2) {
                    Text("Playback Error").font(.system(size: 13, weight: .semibold))
                    Text(error).font(.system(size: 11)).foregroundStyle(.secondary).lineLimit(1)
                }
                Spacer(minLength: 0)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help("Show the playback error")
        .accessibilityIdentifier("now-playing-error-summary")
        .popover(isPresented: $showingError, arrowEdge: .top) {
            VStack(alignment: .leading, spacing: 8) {
                Text("Playback Error").font(.headline)
                Text(error).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
            }
            .padding(14).frame(width: 360)
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier("now-playing-error")
        }
    }

    // MARK: - Centre: transport and scrubber

    private var controls: some View {
        VStack(spacing: 5) {
            HStack(spacing: 20) {
                Toggle(isOn: Binding(get: { transport.shuffle }, set: { transport.setShuffle($0) })) {
                    Label("Shuffle", systemImage: "shuffle")
                        .foregroundStyle(transport.shuffle ? colours.accent : Color.secondary)
                }
                .toggleStyle(.button)
                .disabled(!transport.canChangeModes)
                .help(transport.shuffle ? "Shuffle on" : "Shuffle off")
                .accessibilityValue(transport.shuffle ? "on" : "off")
                .accessibilityIdentifier("transport-shuffle")
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
                .buttonStyle(PlayPauseButtonStyle())
                .disabled(!transport.canPlayPause)
                .help(transport.isPlaying ? "Pause" : "Play")
                .accessibilityIdentifier("transport-play-pause")
                Button(action: transport.next) {
                    Label("Next", systemImage: "forward.fill")
                }
                .disabled(!transport.canNext)
                .help("Next")
                .accessibilityIdentifier("transport-next")
                Button(action: transport.cycleRepeat) {
                    Label(repeatTitle, systemImage: transport.repeat == .one ? "repeat.1" : "repeat")
                        .foregroundStyle(transport.repeat == .off ? Color.secondary : colours.accent)
                }
                .disabled(!transport.canChangeModes)
                .help(repeatTitle)
                .accessibilityValue(transport.repeat.rawValue)
                .accessibilityIdentifier("transport-repeat")
            }
            .buttonStyle(.borderless)
            .labelStyle(.iconOnly)
            .font(.system(size: 14))
            HStack(spacing: 8) {
                Text(seekPreview.map(DisplayFormat.clock) ?? transport.elapsedText)
                    .frame(width: 40, alignment: .trailing)
                    .accessibilityIdentifier("now-playing-elapsed")
                NativeSeekSlider(transport: transport, preview: $seekPreview, identifier: "now-playing-scrubber")
                Text(seekPreview.map { DisplayFormat.remaining(elapsed: $0, duration: transport.duration) }
                     ?? transport.remainingText)
                    .frame(width: 44, alignment: .leading)
                    .accessibilityIdentifier("now-playing-remaining")
            }
            .font(.system(size: 10).monospacedDigit()).foregroundStyle(.secondary)
            .frame(maxWidth: 520)
        }
    }

    private var repeatTitle: String {
        switch transport.repeat {
        case .off: "Repeat Off"
        case .all: "Repeat All"
        case .one: "Repeat One"
        }
    }

    // MARK: - Right: format, drawer, volume

    private var trailing: some View {
        HStack(spacing: 12) {
            // "FLAC 16/44.1" over "RG album −6.2 dB".
            VStack(alignment: .trailing, spacing: 1) {
                Text(nowPlaying == nil ? "" : transport.format)
                if nowPlaying != nil, let gain = transport.gainLabel { Text(gain) }
            }
            .font(.system(size: 10)).foregroundStyle(.secondary).lineLimit(1)
            .frame(maxWidth: .infinity, alignment: .trailing)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(transport.formatLine)
            .accessibilityIdentifier("now-playing-format")
            Toggle(isOn: Binding(get: { model.lyrics.isVisible }, set: { model.lyrics.setVisible($0) })) {
                Label("Lyrics", systemImage: "quote.bubble")
                    .foregroundStyle(model.lyrics.isVisible ? colours.accent : Color.secondary)
            }
            .toggleStyle(.button)
            .buttonStyle(.borderless)
            .labelStyle(.iconOnly)
            .help("Show or hide lyrics (⌥⌘L)")
            .keyboardShortcut("l", modifiers: [.option, .command])
            .accessibilityValue(model.lyrics.isVisible ? "shown" : "hidden")
            .accessibilityIdentifier("lyrics-toggle")
            Toggle(isOn: $navigation.upNextDrawerVisible) {
                Label("Up Next Drawer", systemImage: "sidebar.right")
                    .foregroundStyle(navigation.upNextDrawerVisible ? colours.accent : Color.secondary)
            }
            .toggleStyle(.button)
            .buttonStyle(.borderless)
            .labelStyle(.iconOnly)
            .help("Show or hide the Up Next drawer (⌥⌘U)")
            .accessibilityValue(navigation.upNextDrawerVisible ? "shown" : "hidden")
            .accessibilityIdentifier("up-next-drawer-toggle")
            HStack(spacing: 4) {
                Image(systemName: "speaker.fill").font(.system(size: 10)).foregroundStyle(.secondary)
                    .accessibilityHidden(true)
                VolumeSlider(transport: transport).frame(width: 84)
            }
        }
    }
}

/// Artist or album text that opens the artist or album. Without an id it is plain text.
private struct RevealLink: View {
    let text: String
    let target: NavigationModel.RevealTarget?
    let help: String
    let identifier: String
    let navigation: NavigationModel
    @State private var hovering = false

    var body: some View {
        if let target {
            Button { navigation.reveal = target } label: {
                Text(text).lineLimit(1).underline(hovering)
            }
            .buttonStyle(.plain)
            .onHover { hovering = $0 }
            .help(help)
            .accessibilityIdentifier(identifier)
        } else {
            Text(text).lineLimit(1)
                .accessibilityIdentifier(identifier)
        }
    }
}

/// The ⤢ or ⤡ badge shown while hovering the art.
struct ArtBadge: View {
    let systemImage: String

    var body: some View {
        Image(systemName: systemImage)
            .font(.system(size: 8, weight: .bold))
            .foregroundStyle(.white)
            .frame(width: 16, height: 16)
            .background(Circle().fill(.black.opacity(0.55)))
            .padding(3)
            .accessibilityHidden(true)
    }
}

/// The prominent circular play/pause button.
private struct PlayPauseButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        PlayPauseCircle(configuration: configuration)
    }

    private struct PlayPauseCircle: View {
        let configuration: ButtonStyleConfiguration
        @Environment(\.isEnabled) private var isEnabled

        var body: some View {
            configuration.label
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(Color(nsColor: .windowBackgroundColor))
                .frame(width: 30, height: 30)
                .background(Circle().fill(Color.primary))
                .opacity(isEnabled ? (configuration.isPressed ? 0.7 : 1) : 0.35)
                .contentShape(Circle())
        }
    }
}
