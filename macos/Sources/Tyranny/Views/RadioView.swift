import AppKit
import SwiftUI
import TyrannyCore
import UniformTypeIdentifiers

/// Radio: the saved stations, by name. Double-click or Return plays a station; the context menu
/// plays, adds to Up Next, edits and removes. The header adds a station by URL or imports a
/// `.pls` or `.m3u` file.
struct RadioView: View {
    @Environment(\.themeColours) private var colours
    let model: StationsModel
    let player: PlayerModel
    @State private var selection: Station.ID?
    @State private var sheet: StationSheet.Mode?
    @State private var finding = false
    @State private var removing: Station?

    private var canEdit: Bool { player.canMutate && model.isLoaded }

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            if model.isLoaded, model.stations.isEmpty {
                empty
            } else {
                table
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("main-radio")
        .sheet(item: $sheet) { StationSheet(model: model, mode: $0) }
        .sheet(isPresented: $finding) { DirectorySheet(model: model) }
        .alert("Remove \(removing.map { "“\($0.name)”" } ?? "station")?", isPresented: Binding(
            get: { removing != nil }, set: { if !$0 { removing = nil } }), presenting: removing
        ) { station in
            Button("Remove", role: .destructive) { Task { await model.remove(station.id) } }
                .accessibilityIdentifier("radio-remove-confirm")
            Button("Cancel", role: .cancel) {}
                .accessibilityIdentifier("radio-remove-cancel")
        } message: { _ in
            Text("Up Next entries made from this station are not changed.")
        }
    }

    private var header: some View {
        HStack(spacing: 10) {
            Text("Radio")
                .font(.title2.weight(.semibold))
                .accessibilityAddTraits(.isHeader)
            Text(countText)
                .foregroundStyle(.secondary)
                .monospacedDigit()
                .accessibilityIdentifier("radio-count")
            if let error = model.error {
                Text(error)
                    .font(.callout).foregroundStyle(colours.error)
                    .lineLimit(2)
                    .textSelection(.enabled)
                    .accessibilityIdentifier("radio-error")
            }
            Spacer(minLength: 0)
            Button("Find Stations…") { finding = true }
                .disabled(!canEdit)
                .accessibilityIdentifier("radio-find")
            Button("Add Station…") { sheet = .add }
                .disabled(!canEdit)
                .accessibilityIdentifier("radio-add")
            Button("Import…", action: chooseFile)
                .disabled(!canEdit)
                .accessibilityIdentifier("radio-import")
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
    }

    private var countText: String {
        guard model.isLoaded else { return "" }
        let total = model.stations.count
        return "\(DisplayFormat.count(total)) \(total == 1 ? "station" : "stations")"
    }

    private var empty: some View {
        ContentUnavailableView {
            Label("No stations", systemImage: Destination.radio.systemImage)
        } description: {
            Text("Add a station by its stream URL, or import a .pls or .m3u file.")
        } actions: {
            Button("Find Stations…") { finding = true }
                .disabled(!canEdit)
                .accessibilityIdentifier("radio-empty-find")
            Button("Add Station…") { sheet = .add }
                .disabled(!canEdit)
                .accessibilityIdentifier("radio-empty-add")
            Button("Import…", action: chooseFile)
                .disabled(!canEdit)
                .accessibilityIdentifier("radio-empty-import")
        }
        .accessibilityIdentifier("radio-empty")
    }

    private var table: some View {
        Table(model.stations, selection: $selection) {
            TableColumn("Name", value: \.name)
            TableColumn("URL") { station in
                Text(station.url).foregroundStyle(.secondary)
            }
        }
        .contextMenu(forSelectionType: Station.ID.self) { ids in
            if let station = station(ids) {
                Button("Play") { Task { await model.play(station) } }
                    .disabled(!player.canMutate)
                    .accessibilityIdentifier("radio-station-play")
                Button("Add to Up Next") { Task { await model.enqueue(station) } }
                    .disabled(!player.canMutate)
                    .accessibilityIdentifier("radio-station-enqueue")
                Divider()
                Button("Edit…") { sheet = .edit(station) }
                    .disabled(!canEdit)
                    .accessibilityIdentifier("radio-station-edit")
                Button("Remove…", role: .destructive) { removing = station }
                    .disabled(!canEdit)
                    .accessibilityIdentifier("radio-station-remove")
            }
        } primaryAction: { ids in
            // Double-click and Return.
            guard let station = station(ids), player.canMutate else { return }
            Task { await model.play(station) }
        }
        .accessibilityIdentifier("radio-table")
    }

    private func station(_ ids: Set<Station.ID>) -> Station? {
        guard ids.count == 1, let id = ids.first else { return nil }
        return model.stations.first { $0.id == id }
    }

    /// Import…: one `.pls`, `.m3u` or `.m3u8` file, chosen in a sheet on the window.
    private func chooseFile() {
        guard let window = NSApp.keyWindow ?? NSApp.mainWindow else { return }
        let panel = NSOpenPanel()
        panel.title = "Import Stations"
        panel.prompt = "Import"
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.allowedContentTypes = ["pls", "m3u", "m3u8"].compactMap { UTType(filenameExtension: $0) }
        Task {
            let response = await withCheckedContinuation { continuation in
                panel.beginSheetModal(for: window) { continuation.resume(returning: $0) }
            }
            guard response == .OK, let url = panel.url else { return }
            await model.importFile(url)
        }
    }
}

/// Add Station… and Edit…: a stream URL and a name. The server's failure stays in the sheet.
struct StationSheet: View {
    @Environment(\.themeColours) private var colours
    enum Mode: Identifiable {
        case add, edit(Station)

        var id: String {
            switch self {
            case .add: "add"
            case .edit(let station): "edit-\(station.id)"
            }
        }
    }

    let model: StationsModel
    let mode: Mode
    @Environment(\.dismiss) private var dismiss
    @State private var url = ""
    @State private var name = ""
    @State private var failure: String?
    @State private var working = false

    private var trimmedURL: String { url.trimmingCharacters(in: .whitespacesAndNewlines) }

    private var title: String {
        if case .edit = mode { return "Edit Station" }
        return "Add Station"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(title).font(.headline)
            Text("Enter a direct HTTP or HTTPS stream URL. A blank name uses the URL's host.")
                .font(.callout).foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            TextField("Stream URL", text: $url)
                .textFieldStyle(.roundedBorder)
                .onSubmit(save)
                .accessibilityIdentifier("radio-sheet-url")
            TextField("Name (optional)", text: $name)
                .textFieldStyle(.roundedBorder)
                .onSubmit(save)
                .accessibilityIdentifier("radio-sheet-name")
            if let failure {
                Text(failure).font(.callout).foregroundStyle(colours.error)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
                    .accessibilityIdentifier("radio-sheet-error")
            }
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                    .disabled(working)
                    .accessibilityIdentifier("radio-sheet-cancel")
                Button("Save", action: save)
                    .keyboardShortcut(.defaultAction)
                    .disabled(trimmedURL.isEmpty || working)
                    .accessibilityIdentifier("radio-sheet-save")
            }
        }
        .frame(width: 420)
        .padding(20)
        .interactiveDismissDisabled(working)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("radio-sheet")
        .onAppear {
            if case .edit(let station) = mode {
                url = station.url
                name = station.name
            }
        }
    }

    private func save() {
        guard !trimmedURL.isEmpty, !working else { return }
        working = true
        failure = nil
        let url = trimmedURL
        let name = name.trimmingCharacters(in: .whitespacesAndNewlines)
        Task {
            let saved: Bool
            switch mode {
            case .add: saved = await model.add(name: name, url: url)
            case .edit(let station): saved = await model.update(station.id, name: name, url: url)
            }
            working = false
            if saved {
                dismiss()
            } else {
                // The sheet shows the failure; the view behind it need not.
                failure = model.error
                model.error = nil
            }
        }
    }
}
