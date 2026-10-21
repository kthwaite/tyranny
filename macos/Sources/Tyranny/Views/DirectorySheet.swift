import SwiftUI
import TyrannyCore

/// Find Stations…: searches radio-browser.info through the server. Return searches. Each result
/// plays now or is saved to Radio. The directory's failures stay in the sheet.
struct DirectorySheet: View {
    let model: StationsModel
    @Environment(\.dismiss) private var dismiss

    private var directory: StationDirectory { model.directory }

    var body: some View {
        @Bindable var directory = model.directory
        VStack(alignment: .leading, spacing: 12) {
            Text("Find Stations").font(.headline)
            HStack {
                TextField("Search radio-browser.info", text: $directory.query)
                    .textFieldStyle(.roundedBorder)
                    .onSubmit { Task { await directory.search() } }
                    .accessibilityIdentifier("radio-directory-query")
                if directory.searching {
                    ProgressView().controlSize(.small)
                }
            }
            if let error = directory.error {
                Text(error).font(.callout).foregroundStyle(.red)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
                    .accessibilityIdentifier("radio-directory-error")
            }
            List(directory.results) { station in
                row(station)
            }
            .frame(minHeight: 260)
            .accessibilityIdentifier("radio-directory-results")
            HStack {
                Text("Station data from radio-browser.info")
                    .font(.caption).foregroundStyle(.secondary)
                Spacer()
                Button("Done") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                    .accessibilityIdentifier("radio-directory-done")
            }
        }
        .frame(width: 520)
        .padding(20)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("radio-directory-sheet")
    }

    private func row(_ station: DirectoryStation) -> some View {
        let saved = directory.isSaved(station)
        return HStack {
            VStack(alignment: .leading, spacing: 2) {
                Text(station.name).lineLimit(1)
                let detail = Self.detail(station)
                if !detail.isEmpty {
                    Text(detail).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                }
            }
            Spacer(minLength: 8)
            Button("Play") { Task { await directory.play(station) } }
            Button(saved ? "Saved" : "Add") { Task { _ = await model.save(station) } }
                .disabled(saved)
        }
    }

    /// `GB · MP3 · 128 kbps · jazz, smooth`, without the parts the directory left empty.
    static func detail(_ station: DirectoryStation) -> String {
        var parts: [String] = []
        if !station.country_code.isEmpty { parts.append(station.country_code) }
        if !station.codec.isEmpty { parts.append(station.codec) }
        if station.bitrate > 0 { parts.append("\(station.bitrate) kbps") }
        if !station.tags.isEmpty { parts.append(station.tags.joined(separator: ", ")) }
        return parts.joined(separator: " · ")
    }
}
