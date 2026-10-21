import SwiftUI
import TyrannyCore

/// Review and write the tags MusicBrainz proposes for one album.
struct TagLookupSheet: View {
    let model: TaggingModel
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Look Up on MusicBrainz").font(.headline)
            if let title = model.target?.title {
                Text(title).foregroundStyle(.secondary).lineLimit(1)
            }
            content
        }
        .padding(20)
        .frame(width: 640)
        .interactiveDismissDisabled(model.isEditing)
        .accessibilityIdentifier("tag-lookup-sheet")
    }

    @ViewBuilder private var content: some View {
        switch model.phase {
        case .lookingUp, .applying, .reverting:
            HStack(spacing: 8) {
                ProgressView().controlSize(.small).accessibilityIdentifier("tag-lookup-progress")
                Text(progressText)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            if !model.isEditing {
                HStack {
                    Spacer()
                    cancelButton
                }
            }
        case .reviewing:
            reviewing
        case .applied(let result):
            applied(result)
        case .failed(let message):
            Text(message).foregroundStyle(.red)
                .fixedSize(horizontal: false, vertical: true).textSelection(.enabled)
                .accessibilityIdentifier("tag-lookup-error")
            HStack {
                Spacer()
                cancelButton
                Button("Try Again") { model.retry() }.keyboardShortcut(.defaultAction)
            }
        }
    }

    private var progressText: String {
        switch model.phase {
        case .applying: "Writing tags…"
        case .reverting: "Undoing…"
        default: "Asking MusicBrainz…"
        }
    }

    private var cancelButton: some View {
        Button("Cancel") { close() }
            .keyboardShortcut(.cancelAction)
            .accessibilityIdentifier("tag-lookup-cancel")
    }

    private func close() {
        model.target = nil
        dismiss()
    }

    // MARK: - Reviewing

    @ViewBuilder private var reviewing: some View {
        if let proposal = model.proposal {
            Picker("Release", selection: Binding(
                get: { proposal.release_id },
                set: { id in
                    guard id != proposal.release_id else { return }
                    model.lookUp(releaseID: id, fingerprint: false)
                }
            )) {
                ForEach(model.candidates) { Text($0.label).tag($0.release_id) }
            }
            .accessibilityIdentifier("tag-lookup-release-picker")
            ScrollView {
                VStack(alignment: .leading, spacing: 10) {
                    ForEach(proposal.tracks) { row($0) }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .frame(minHeight: 160, maxHeight: 360)
        }
        HStack {
            if model.hasAcoustIDKey {
                Button("Identify by Audio Fingerprint") { model.lookUp(releaseID: nil, fingerprint: true) }
                    .accessibilityIdentifier("tag-lookup-fingerprint")
            }
            Spacer()
            cancelButton
            Button(model.selection?.writeButtonTitle ?? "Write Tags to 0 Files") { model.apply() }
                .keyboardShortcut(.defaultAction)
                .disabled(model.selection?.selected.isEmpty ?? true)
                .accessibilityIdentifier("tag-lookup-write")
        }
    }

    private func row(_ track: TrackProposal) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Toggle(track.file_name, isOn: Binding(
                get: { model.selection?.selected.contains(track.track_id) ?? false },
                set: { _ in model.selection?.toggle(track.track_id) }
            ))
            .labelsHidden()
            .disabled(!(model.selection?.isEnabled(track.track_id) ?? false))
            Text(track.track_number.map(String.init) ?? "–")
                .monospacedDigit().foregroundStyle(.secondary).frame(width: 28, alignment: .trailing)
            VStack(alignment: .leading, spacing: 2) {
                Text(track.file_name).lineLimit(1).truncationMode(.middle)
                ForEach(Array(track.changes.map(\.line).enumerated()), id: \.offset) { Text($0.element).font(.callout) }
                ForEach(Array(track.notes.enumerated()), id: \.offset) { Text($0.element).font(.callout).foregroundStyle(.secondary) }
            }
            Spacer(minLength: 0)
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("tag-lookup-row-\(track.track_id)")
    }

    // MARK: - Applied

    private func applied(_ result: TagApplyResult) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            ScrollView {
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(result.files, id: \.track_id) { Text($0.line).textSelection(.enabled) }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .frame(minHeight: 60, maxHeight: 300)
            HStack {
                Spacer()
                Button("Undo") { model.undo() }
                    .disabled(result.edit_id == nil || model.isUndone)
                    .accessibilityIdentifier("tag-lookup-undo")
                Button("Done") { close() }.keyboardShortcut(.defaultAction)
            }
        }
    }
}
