import SwiftUI
import TyrannyCore

struct LyricsPane: View {
    @Environment(\.themeColours) private var colours
    let store: LyricsStore
    let transport: TransportModel

    private var active: Range<Int> { store.timeline.activeRange(at: transport.elapsed) }
    private var activeText: [String] { active.map { store.timeline.lines[$0].text } }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text("Lyrics").font(.headline)
                Spacer()
                Button { store.setVisible(false) } label: { Label("Close Lyrics", systemImage: "xmark") }
                    .buttonStyle(.borderless).labelStyle(.iconOnly)
                    .help("Close lyrics")
                    .accessibilityIdentifier("lyrics-close")
            }
            .padding(14)
            if let playing = transport.nowPlaying {
                VStack(alignment: .leading, spacing: 3) {
                    Text(playing.title).font(.subheadline.weight(.semibold)).lineLimit(2)
                    Text(transport.artist).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                }
                .padding(.horizontal, 14).padding(.bottom, 12)
            }
            Divider()
            content.frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            if store.state == .ready || store.state == .instrumental {
                Divider()
                if let url = store.payload?.attributionURL {
                    Link("Lyrics from LRCLIB", destination: url)
                        .font(.caption).padding(12)
                        .accessibilityIdentifier("lyrics-attribution")
                } else if store.payload?.source == .embedded {
                    Text("Lyrics embedded in this file").font(.caption).foregroundStyle(.secondary).padding(12)
                }
            }
        }
        .frame(width: 300)
        .background(.background)
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Lyrics")
        .accessibilityValue(store.state.rawValue)
        .accessibilityIdentifier("lyrics-pane")
        .onAppear {
            store.setMounted(true)
            reportRender(mounted: true)
        }
        .onDisappear {
            store.setMounted(false)
            reportRender(mounted: false)
        }
        .onChange(of: store.generation) { _, _ in reportRender(mounted: true) }
        .onChange(of: store.state) { _, _ in reportRender(mounted: true) }
        .onChange(of: active) { _, _ in reportRender(mounted: true) }
    }

    @ViewBuilder private var content: some View {
        switch store.state {
        case .loading:
            VStack(spacing: 12) {
                ProgressView().controlSize(.small)
                Text("Finding lyrics…").foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .accessibilityIdentifier("lyrics-loading")
        case .missing:
            status("No lyrics found", detail: transport.nowPlaying == nil ? "Play a track to see its lyrics." : "No embedded or matching online lyrics are available.")
        case .unavailable:
            status("Lyrics unavailable", detail: "The lyrics service or server could not be reached. Playback is unaffected.")
        case .instrumental:
            status("Instrumental", detail: "This track has no sung lyrics.")
        case .ready:
            if store.timeline.lines.isEmpty {
                ScrollView {
                    Text(store.payload?.plain_text ?? "")
                        .textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading)
                        .padding(16).accessibilityIdentifier("lyrics-plain-text")
                }
            } else {
                timedLines
            }
        }
    }

    private var timedLines: some View {
        let active = self.active
        return ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 14) {
                    ForEach(store.timeline.lines.indices, id: \.self) { index in
                        let line = store.timeline.lines[index]
                        Text(line.text.isEmpty ? " " : line.text)
                            .font(.body.weight(active.contains(index) ? .semibold : .regular))
                            .foregroundStyle(active.contains(index) ? colours.accent : Color.secondary)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .textSelection(.enabled)
                            .accessibilityValue(active.contains(index) ? "Current lyric" : "")
                            .accessibilityIdentifier("lyrics-line-\(line.id)")
                            .id(line.id)
                    }
                }
                .padding(16)
            }
            .onAppear { scrollToActive(proxy) }
            .onChange(of: active.lowerBound) { _, _ in scrollToActive(proxy) }
            .onChange(of: store.generation) { _, _ in scrollToActive(proxy) }
        }
        .accessibilityIdentifier("lyrics-synced-text")
    }

    private func scrollToActive(_ proxy: ScrollViewProxy) {
        guard let index = active.first else { return }
        proxy.scrollTo(store.timeline.lines[index].id, anchor: .center)
    }

    private func status(_ title: String, detail: String) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title).font(.headline)
            Text(detail).font(.callout).foregroundStyle(.secondary)
        }
        .padding(16)
        .accessibilityIdentifier("lyrics-status")
    }

    private func reportRender(mounted: Bool) {
        #if DEBUG
        NativeLyricsSmoke.observePane(mounted: mounted, state: store.state.rawValue, activeText: activeText)
        #endif
    }
}
