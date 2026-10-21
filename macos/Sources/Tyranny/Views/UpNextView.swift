import SwiftUI
import TyrannyCore

/// Up Next (spec 5): the queue in three sections, HISTORY, NOW PLAYING and UP NEXT, with
/// Shuffle Upcoming, Save as Playlist… and Clear in the header.
struct UpNextView: View {
    let model: UpNextModel
    let player: PlayerModel
    @State private var saving = false
    @State private var addingStream = false

    private var upcoming: Int? { model.upcomingCount }
    private var canEdit: Bool { player.canMutate }

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            UpNextTable(model: model, player: player, colours: player.theme.tableColours, generation: model.cache.generation, epoch: model.epoch,
                        context: model.context, selected: model.selection, revealToken: model.revealRequest?.token)
                .overlay {
                    if model.cache.revision != nil, model.cache.total == 0 {
                        Text("Nothing in Up Next")
                            .font(.title3)
                            .foregroundStyle(.secondary)
                            .allowsHitTesting(false)
                            .accessibilityIdentifier("up-next-empty")
                    }
                }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("main-up-next")
        .onAppear {
            model.setViewShown(true)
            model.sync()
        }
        .onDisappear { model.setViewShown(false) }
        .sheet(isPresented: $saving) { SavePlaylistSheet(model: model) }
        .sheet(isPresented: $addingStream) { AddStreamSheet(model: model) }
    }

    private var header: some View {
        HStack(spacing: 10) {
            Text("Up Next")
                .font(.title2.weight(.semibold))
                .accessibilityAddTraits(.isHeader)
            Text(countText)
                .foregroundStyle(.secondary)
                .monospacedDigit()
                .accessibilityIdentifier("up-next-count")
            Spacer(minLength: 0)
            Button("Add Stream…") { addingStream = true }
                .disabled(!canEdit)
                .accessibilityIdentifier("up-next-add-stream")
            Button("Shuffle Upcoming") { model.shuffleUpcoming() }
                .disabled(!canEdit || upcoming == 0)
                .accessibilityIdentifier("up-next-shuffle")
            Button("Save as Playlist…") { saving = true }
                .disabled(!canEdit || upcoming == 0)
                .accessibilityIdentifier("up-next-save")
            Button("Clear") { model.clearUpcoming() }
                .disabled(!canEdit || upcoming == 0)
                .accessibilityIdentifier("up-next-clear")
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
    }

    private var countText: String {
        guard let upcoming else { return "" }
        return "\(DisplayFormat.count(upcoming)) upcoming"
    }
}

/// The name prompt of Save as Playlist…. The server's failure stays in the sheet.
private struct SavePlaylistSheet: View {
    @Environment(\.themeColours) private var colours
    let model: UpNextModel
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var failure: String?
    @State private var working = false

    private var trimmed: String { name.trimmingCharacters(in: .whitespacesAndNewlines) }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Save Up Next as Playlist").font(.headline)
            TextField("Playlist name", text: $name)
                .textFieldStyle(.roundedBorder)
                .frame(width: 280)
                .onSubmit(save)
                .accessibilityIdentifier("up-next-save-name")
            if let failure {
                Text(failure)
                    .font(.callout)
                    .foregroundStyle(colours.error)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier("up-next-save-error")
            }
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                    .accessibilityIdentifier("up-next-save-cancel")
                Button("Save", action: save)
                    .keyboardShortcut(.defaultAction)
                    .disabled(trimmed.isEmpty || working)
                    .accessibilityIdentifier("up-next-save-confirm")
            }
        }
        .padding(20)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("up-next-save-sheet")
    }

    private func save() {
        guard !trimmed.isEmpty, !working else { return }
        working = true
        failure = nil
        let name = trimmed
        Task {
            let problem = await model.savePlaylist(name: name)
            working = false
            if let problem { failure = problem } else { dismiss() }
        }
    }
}

private struct AddStreamSheet: View {
    @Environment(\.themeColours) private var colours
    let model: UpNextModel
    @Environment(\.dismiss) private var dismiss
    @State private var url = ""
    @State private var title = ""
    @State private var failure: String?
    @State private var working = false

    private var trimmedURL: String { url.trimmingCharacters(in: .whitespacesAndNewlines) }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Add Stream").font(.headline)
            Text("Enter a direct HTTP or HTTPS MP3/Ogg stream URL, not a playlist or web page.")
                .font(.callout).foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            TextField("Stream URL", text: $url)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier("stream-url")
            TextField("Title (optional)", text: $title)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier("stream-title")
            if let failure {
                Text(failure).font(.callout).foregroundStyle(colours.error)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier("stream-error")
            }
            HStack {
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                    .disabled(working)
                Spacer()
                Button("Add & Play") { add(play: true) }
                    .disabled(trimmedURL.isEmpty || working)
                    .accessibilityIdentifier("stream-add-play")
                Button("Add to Up Next") { add(play: false) }
                    .keyboardShortcut(.defaultAction)
                    .disabled(trimmedURL.isEmpty || working)
                    .accessibilityIdentifier("stream-add")
            }
        }
        .frame(width: 420)
        .padding(20)
        .interactiveDismissDisabled(working)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("add-stream-sheet")
    }

    private func add(play: Bool) {
        guard !working, !trimmedURL.isEmpty else { return }
        working = true
        failure = nil
        let url = trimmedURL
        let title = title.trimmingCharacters(in: .whitespacesAndNewlines)
        Task {
            let problem = await model.addStream(url: url, title: title, play: play)
            working = false
            if let problem { failure = problem } else { dismiss() }
        }
    }
}
