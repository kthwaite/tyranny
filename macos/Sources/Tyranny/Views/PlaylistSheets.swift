import SwiftUI
import TyrannyCore

/// A playlist name the server accepts: 1 to 256 UTF-8 bytes after trimming, no NUL.
func isValidPlaylistName(_ name: String) -> Bool {
    let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
    return !trimmed.isEmpty && trimmed.utf8.count <= 256 && !trimmed.contains("\0")
}

/// The filter syntax popover, next to rule and filter fields.
struct TrackFilterHelp: View {
    let identifier: String
    @State private var showing = false

    var body: some View {
        Button("Filter syntax", systemImage: "line.3.horizontal.decrease.circle") { showing = true }
            .labelStyle(.iconOnly)
            .accessibilityIdentifier(identifier)
            .help("Filter syntax and examples")
            .popover(isPresented: $showing) {
                VStack(alignment: .leading, spacing: 10) {
                    Text("Live Track Filters").font(.headline)
                    Text("Use title:, artist: (track artist), or album:. Text matches literal substrings, ignoring case. Bare words search all three fields; quote a phrase.")
                    Text(#"artist:"Miles Davis" & !album:"Kind of Blue""#).font(.system(.callout, design: .monospaced))
                    Text(#"(artist:"Miles Davis" | artist:"Ornette Coleman") & !album:"Kind of Blue""#)
                        .font(.system(.callout, design: .monospaced))
                    Text(#"artist:["Miles Davis", "Ornette Coleman"]"#).font(.system(.callout, design: .monospaced))
                    Text("Use ! or NOT, then & or AND, then | or OR in order of precedence. Parentheses group expressions. Adjacent terms mean AND. Word operators must be uppercase. Lists mean OR; !artist:[…] excludes every listed artist.")
                    Text(#"Values cannot be empty. Inside quotes, use \" for a quote and \\ for a backslash. Filters are limited to 1,024 UTF-8 bytes. Clear the filter to show all tracks."#)
                        .foregroundStyle(.secondary)
                }
                .textSelection(.enabled).padding(18).frame(width: 480)
            }
    }
}

// MARK: - Create and rename

/// The name prompt of New Playlist and Rename….
struct PlaylistNameSheet: View {
    enum Mode { case create, rename(Playlist) }

    let playlists: PlaylistsModel
    let mode: Mode
    /// Called with the playlist a successful create made.
    var created: (Playlist) -> Void = { _ in }
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var failure: String?
    @State private var working = false

    private var title: String {
        if case .rename = mode { return "Rename Playlist" }
        return "New Playlist"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(title).font(.headline)
            TextField("Playlist name", text: $name)
                .textFieldStyle(.roundedBorder)
                .frame(width: 300)
                .onSubmit(save)
                .accessibilityIdentifier("playlist-name")
            Text("Names must be unique and contain 1–256 UTF-8 bytes.")
                .font(.caption).foregroundStyle(.secondary)
            if let failure {
                Text(failure).font(.callout).foregroundStyle(.red)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
                    .accessibilityIdentifier("playlist-name-error")
            }
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                    .accessibilityIdentifier("playlist-name-cancel")
                Button(saveTitle, action: save)
                    .keyboardShortcut(.defaultAction)
                    .disabled(!isValidPlaylistName(name) || working)
                    .accessibilityIdentifier("playlist-name-save")
            }
        }
        .padding(20)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("playlist-name-sheet")
        .onAppear { if case .rename(let playlist) = mode { name = playlist.name } }
    }

    private var saveTitle: String {
        if case .rename = mode { return "Rename" }
        return "Create"
    }

    private func save() {
        guard isValidPlaylistName(name), !working else { return }
        working = true
        failure = nil
        let name = name.trimmingCharacters(in: .whitespacesAndNewlines)
        Task {
            var made: Playlist?
            let problem = await playlists.run {
                switch mode {
                case .create: made = try await playlists.create(name: name)
                case .rename(let playlist): try await playlists.rename(playlist, name: name)
                }
            }
            working = false
            if let problem { failure = problem; return }
            if let made { created(made) }
            dismiss()
        }
    }
}

// MARK: - Smart rules

/// New Smart Playlist… and Edit Rule…. The rule is searched against the whole library, with a
/// live preview of the first 100 matches.
struct SmartPlaylistEditor: View {
    let playlists: PlaylistsModel
    let player: PlayerModel
    /// nil creates a new smart playlist.
    let playlist: Playlist?
    var created: (Playlist) -> Void = { _ in }
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var rule: String
    @State private var preview: PlaylistsModel.RulePreview?
    @State private var previewedRule: String?
    @State private var previewError: String?
    @State private var saveError: String?
    @State private var loading = true
    @State private var working = false

    init(playlists: PlaylistsModel, player: PlayerModel, playlist: Playlist?, created: @escaping (Playlist) -> Void = { _ in }) {
        self.playlists = playlists
        self.player = player
        self.playlist = playlist
        self.created = created
        _rule = State(initialValue: playlist?.query ?? "")
    }

    private var previewKey: String {
        "\(player.connected)|\(player.playlistConnectionGeneration)|\(player.snapshot?.library.revision ?? 0)|\(rule)"
    }

    private var ready: Bool {
        !loading && previewError == nil && previewedRule == rule
            && preview?.revision == player.snapshot?.library.revision
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(playlist == nil ? "New Smart Playlist" : "Edit Smart Playlist Rule").font(.headline)
            if playlist == nil {
                TextField("Playlist name", text: $name).textFieldStyle(.roundedBorder)
                    .accessibilityIdentifier("smart-name")
            } else {
                Text(playlist?.name ?? "").foregroundStyle(.secondary)
            }
            Text("This rule searches the entire available library, not the list on screen. An empty rule includes all available tracks. Results update when the library changes.")
                .font(.callout)
            HStack {
                TextField("Library rule", text: $rule).textFieldStyle(.roundedBorder)
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
                if preview.total == 0 {
                    Text("No available tracks match. You can still save this rule.").foregroundStyle(.secondary)
                }
                List(preview.rows) { row in
                    VStack(alignment: .leading) {
                        Text(row.track.title)
                        Text("\(row.track.artist) — \(row.track.album)").font(.caption).foregroundStyle(.secondary)
                    }
                }
                Text("Preview shows up to 100 tracks; the saved rule includes all matches.")
                    .font(.caption).foregroundStyle(.secondary)
            } else if loading {
                ProgressView("Previewing rule…").frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                Spacer(minLength: 0)
            }
            if let saveError { Text(saveError).foregroundStyle(.orange).textSelection(.enabled) }
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction).accessibilityIdentifier("smart-cancel")
                Button("Save", action: save).keyboardShortcut(.defaultAction).accessibilityIdentifier("smart-save")
                    .disabled(!player.canMutate || !ready || working || (playlist == nil && !isValidPlaylistName(name)))
            }
        }
        .padding(24).frame(width: 600, height: 540)
        .interactiveDismissDisabled(working)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("smart-sheet")
        .task(id: previewKey) {
            let key = previewKey, text = rule
            loading = true; preview = nil; previewedRule = nil; previewError = nil
            do {
                try await Task.sleep(for: .milliseconds(250))
                let result = try await playlists.preview(rule: text)
                guard !Task.isCancelled, key == previewKey else { return }
                preview = result; previewedRule = text
            } catch {
                guard !Task.isCancelled, key == previewKey else { return }
                previewError = (error as? ServerFailure)?.message ?? error.localizedDescription
            }
            loading = false
        }
    }

    private func save() {
        guard ready, !working else { return }
        working = true
        saveError = nil
        let text = rule, trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        Task {
            var made: Playlist?
            let problem = await playlists.run {
                let saved = try await playlists.saveSmart(playlist, name: trimmed, rule: text)
                if playlist == nil { made = saved }
            }
            working = false
            if let problem { saveError = problem; return }
            if let made { created(made) }
            dismiss()
        }
    }
}

/// Create Regular Playlist from Results…: copies a smart playlist's results.
struct SmartPlaylistSnapshotSheet: View {
    let playlists: PlaylistsModel
    let playlist: Playlist
    var created: (Playlist) -> Void = { _ in }
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var failure: String?
    @State private var working = false

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Create Regular Playlist from Results").font(.headline)
            Text("Copy all current results of “\(playlist.name)” into an editable regular playlist. Empty results are allowed. Later library or rule changes will not change the copy.")
                .fixedSize(horizontal: false, vertical: true)
            TextField("Playlist name", text: $name).textFieldStyle(.roundedBorder)
                .accessibilityIdentifier("snapshot-name")
            if let failure { Text(failure).foregroundStyle(.orange).textSelection(.enabled) }
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
                    .accessibilityIdentifier("snapshot-cancel")
                Button("Create", action: create).keyboardShortcut(.defaultAction)
                    .accessibilityIdentifier("snapshot-create")
                    .disabled(!isValidPlaylistName(name) || working)
            }
        }
        .padding(24).frame(width: 500)
        .interactiveDismissDisabled(working)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("snapshot-sheet")
    }

    private func create() {
        guard isValidPlaylistName(name), !working else { return }
        working = true
        failure = nil
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        Task {
            var made: Playlist?
            let problem = await playlists.run { made = try await playlists.snapshotSmart(playlist, name: trimmed) }
            working = false
            if let problem { failure = problem; return }
            if let made { created(made) }
            dismiss()
        }
    }
}

// MARK: - Add to Playlist

/// The add-to-playlist picker: the regular playlists, or a new one, for a track selection.
struct AddToPlaylistSheet: View {
    let playlists: PlaylistsModel
    let selection: TrackSelection
    let count: Int
    @Environment(\.dismiss) private var dismiss
    @State private var filter = ""
    @State private var newName = ""
    @State private var chosen: UInt64?
    @State private var failure: String?
    @State private var working = false

    private var regular: [Playlist] {
        let text = filter.trimmingCharacters(in: .whitespacesAndNewlines)
        return playlists.rows.filter { !$0.isSmart && (text.isEmpty || $0.name.localizedCaseInsensitiveContains(text)) }
    }

    private var destination: Playlist? { chosen.flatMap { id in regular.first { $0.id == id } } }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Add \(DisplayFormat.count(count)) \(count == 1 ? "Track" : "Tracks") to Playlist").font(.headline)
            HStack {
                TextField("New playlist name", text: $newName)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityIdentifier("playlist-picker-name")
                Button("Create & Add Tracks", action: createAndAdd)
                    .disabled(!isValidPlaylistName(newName) || working)
                    .accessibilityIdentifier("playlist-picker-create-add")
            }
            if let failure {
                Text(failure).foregroundStyle(.orange).textSelection(.enabled)
                    .accessibilityIdentifier("playlist-picker-create-error")
            }
            Divider()
            TextField("Filter playlists", text: $filter)
                .textFieldStyle(.roundedBorder).accessibilityIdentifier("playlist-picker-filter")
            List(regular, selection: $chosen) { playlist in
                VStack(alignment: .leading) {
                    Text(playlist.name)
                    Text(playlist.summary).font(.caption).foregroundStyle(.secondary)
                }
                .tag(playlist.id)
            }
            .accessibilityIdentifier("playlist-picker-list")
            .overlay {
                if regular.isEmpty {
                    Text(filter.isEmpty ? "Create a regular playlist above." : "No matching playlists")
                        .foregroundStyle(.secondary)
                }
            }
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
                    .disabled(working)
                    .accessibilityIdentifier("playlist-picker-cancel")
                Button("Add Tracks", action: add)
                    .disabled(destination == nil || working)
                    .accessibilityIdentifier("playlist-picker-add")
            }
        }
        .padding(20).frame(width: 500, height: 500)
        .interactiveDismissDisabled(working)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("playlist-picker")
    }

    private func add() {
        guard let destination, !working else { return }
        working = true
        failure = nil
        Task {
            let problem = await playlists.run {
                try await playlists.addSelection(to: destination, selection: selection, count: count)
            }
            working = false
            if let problem { failure = problem } else { dismiss() }
        }
    }

    private func createAndAdd() {
        guard isValidPlaylistName(newName), !working else { return }
        working = true
        failure = nil
        let name = newName.trimmingCharacters(in: .whitespacesAndNewlines)
        Task {
            let problem = await playlists.run {
                _ = try await playlists.createAdding(name: name, selection: selection, count: count)
            }
            working = false
            if let problem { failure = problem } else { dismiss() }
        }
    }
}
