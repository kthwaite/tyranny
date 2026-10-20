import SwiftUI
import TyrannyCore

/// The main area while the first music folder imports, and after it ends.
struct ImportProgressView: View {
    let progress: ImportProgress
    /// The session root's path, abbreviated. Nil until the root list has it.
    let folder: String?
    let model: PlayerModel
    @Bindable var navigation: NavigationModel

    private var symbol: String {
        switch progress.stage {
        case .preparing, .discovering, .indexing: "music.note.house"
        case .done: "checkmark.circle.fill"
        case .empty: "questionmark.folder"
        case .stopped: "exclamationmark.triangle"
        }
    }

    private var isRunning: Bool {
        switch progress.stage {
        case .preparing, .discovering, .indexing: true
        case .done, .empty, .stopped: false
        }
    }

    var body: some View {
        VStack(spacing: 12) {
            Image(systemName: symbol)
                .font(.system(size: 56))
                .foregroundStyle(progress.stage == .done ? Color.green : Color.secondary)
                .accessibilityHidden(true)
            Text(progress.title).font(.title2.weight(.semibold))
            if let folder {
                Text(folder).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle)
            }
            if isRunning {
                bar.frame(width: 360).padding(.vertical, 4)
            }
            if let detail = progress.detail {
                Text(detail).multilineTextAlignment(.center)
            }
            if let failureLine = progress.failureLine {
                Text(failureLine).foregroundStyle(.secondary)
            }
            buttons.padding(.top, 8)
        }
        .padding(24)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("import-progress")
    }

    @ViewBuilder private var bar: some View {
        if let fraction = progress.fraction {
            ProgressView(value: fraction)
                .accessibilityLabel("Import progress")
                .accessibilityValue(progress.detail ?? "")
                .accessibilityIdentifier("import-progress-bar")
        } else {
            ProgressView().progressViewStyle(.linear)
                .accessibilityLabel("Import progress")
                .accessibilityValue(progress.detail ?? "")
                .accessibilityIdentifier("import-progress-bar")
        }
    }

    @ViewBuilder private var buttons: some View {
        switch progress.stage {
        case .preparing, .discovering, .indexing:
            browse
        case .done:
            Button("Show Songs") {
                model.endImport()
                navigation.destination = .songs
            }
            .buttonStyle(.borderedProminent)
            .keyboardShortcut(.defaultAction)
            .accessibilityIdentifier("import-show-songs")
        case .empty:
            chooseAnother
        case .stopped:
            HStack {
                Button("Try Again", action: model.retryImport)
                    .disabled(!model.canMutate)
                    .accessibilityIdentifier("import-retry")
                browse
            }
        }
    }

    private var browse: some View {
        Button("Browse Library", action: model.endImport)
            .accessibilityIdentifier("import-browse")
    }

    private var chooseAnother: some View {
        Button("Choose Another Folder…", action: model.chooseFolder)
            .buttonStyle(.borderedProminent)
            .disabled(!model.canMutate || model.scanActive)
            .accessibilityIdentifier("import-choose-another")
    }
}
