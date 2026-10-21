import SwiftUI
import TyrannyCore

/// The Settings window (⌘,): Connection, Library, Playback, Appearance and Artwork.
struct SettingsView: View {
    let model: PlayerModel
    @Bindable var navigation: NavigationModel

    var body: some View {
        TabView(selection: $navigation.settingsTab) {
            ConnectionSettingsPane(model: model)
                .tabItem { Label("Connection", systemImage: "network") }
                .tag(SettingsTab.connection)
            MusicFoldersPane(model: model)
                .tabItem { Label("Library", systemImage: "folder") }
                .tag(SettingsTab.library)
            PlaybackSettingsPane(transport: model.transport, notifications: model.notifications)
                .tabItem { Label("Playback", systemImage: "play.circle") }
                .tag(SettingsTab.playback)
            AppearanceSettingsPane(theme: model.theme)
                .tabItem { Label("Appearance", systemImage: "paintpalette") }
                .tag(SettingsTab.appearance)
            ArtworkSettingsPane(settings: model.artwork.settings)
                .tabItem { Label("Artwork", systemImage: "photo") }
                .tag(SettingsTab.artwork)
        }
        .frame(width: 600)
        .accessibilityIdentifier("settings")
    }
}

/// Server address, token file and managed server controls. `done` adds a Done
/// button when shown as a sheet.
struct ConnectionSettingsPane: View {
    @Bindable var model: PlayerModel
    var done: (() -> Void)?
    @State private var serverAction: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Server Connection").font(.title2)
            Text("Connect to a local Tyranny server. The token file is read when connecting; its contents are never displayed.")
                .foregroundStyle(.secondary)
            Form {
                TextField("WebSocket URL", text: $model.endpoint)
                    .accessibilityIdentifier("connection-url")
                TextField("Token file", text: $model.tokenPath)
                    .accessibilityIdentifier("connection-token-path")
            }.disabled(model.serverManagementBusy)
            Text("Use a literal loopback address, for example ws://127.0.0.1:9865/ws.")
                .font(.caption).foregroundStyle(.secondary)
            Text(model.connectionDetail).textSelection(.enabled)
                .accessibilityIdentifier("connection-detail")
            if model.managesServer {
                Divider()
                Text("Local Background Server").font(.headline)
                Text("Runs independently of this window and at login. Stop removes its login job; opening the app again starts it.")
                    .font(.caption).foregroundStyle(.secondary)
                if let path = model.managedLogPath {
                    Text("Log: \(path)").font(.caption).textSelection(.enabled)
                }
                HStack {
                    Button("Start", action: model.startManagedServer)
                        .accessibilityIdentifier("managed-start")
                    Button("Restart…") { serverAction = "restart" }
                        .accessibilityIdentifier("managed-restart")
                    Button("Stop…") { serverAction = "stop" }
                        .accessibilityIdentifier("managed-stop")
                    if model.serverManagementBusy { ProgressView().controlSize(.small) }
                }.disabled(model.serverManagementBusy || model.busy || model.connection == "connecting")
            }
            HStack {
                Button("Disconnect", action: model.disconnect)
                    .disabled(model.connection == "disconnected" || model.serverManagementBusy)
                    .accessibilityIdentifier("connection-disconnect")
                Spacer()
                if let done {
                    Button("Done", action: done).keyboardShortcut(.cancelAction)
                }
                Button("Connect / Retry", action: model.reconnect)
                    .disabled(model.connection == "connecting" || model.busy || model.serverManagementBusy)
                    .keyboardShortcut(.defaultAction)
                    .accessibilityIdentifier("connection-connect")
            }
        }
        .padding(24).frame(width: 550)
        .confirmationDialog(
            serverAction == "restart" ? "Restart the background server?" : "Stop the background server?",
            isPresented: Binding(get: { serverAction != nil }, set: { if !$0 { serverAction = nil } })
        ) {
            if serverAction == "restart" {
                Button("Restart Server", role: .destructive, action: model.restartManagedServer)
            } else {
                Button("Stop Server", role: .destructive, action: model.stopManagedServer)
            }
        } message: {
            Text("Playback will stop. Your library, queue, and settings are retained.")
        }
    }
}

/// Music folders: add, rescan and review.
struct MusicFoldersPane: View {
    @Environment(\.themeColours) private var colours
    let model: PlayerModel
    var done: (() -> Void)?
    @AppStorage(TaggingModel.acoustIDKeyDefault) private var acoustIDKey = ""

    var body: some View {
        ScrollView {
        VStack(alignment: .leading, spacing: 12) {
            Text("Music Folders").font(.title2)
            Text("Folders must be readable by the server. Rescan to import changes or retry failed and interrupted scans.")
                .foregroundStyle(.secondary)
            List(model.roots?.entries ?? []) { root in
                HStack(alignment: .top) {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(root.path).textSelection(.enabled)
                        if let scan = root.last_scan {
                            Text("\(scan.phase.capitalized) · \(scan.processed) processed · \(scan.failures) failed")
                                .font(.caption).foregroundStyle(.secondary)
                            if let error = scan.last_error {
                                Text(error).font(.caption).foregroundStyle(colours.warning).textSelection(.enabled)
                            }
                        }
                    }
                    Spacer()
                    Button("Rescan") { model.rescan(root.id) }
                        .disabled(!model.canMutate || model.scanActive)
                        .accessibilityIdentifier("rescan-root-\(root.id)")
                }.padding(.vertical, 4)
            }
            .frame(height: 150)
            .accessibilityIdentifier("roots-list")
            .overlay {
                if model.roots?.entries.isEmpty ?? true {
                    Text(model.connected ? "No music folders" : "Not connected")
                        .foregroundStyle(.secondary).allowsHitTesting(false)
                }
            }
            HStack {
                Spacer()
                Button("Choose Music Folder…", action: model.chooseFolder)
                    .disabled(!model.canMutate || model.scanActive)
                    .accessibilityIdentifier("roots-choose-folder")
                if let done {
                    Button("Done", action: done).keyboardShortcut(.cancelAction)
                }
            }
            Divider()
            tagging
        }
        .padding(20)
        }
        .frame(width: 600, height: 640)
        .task { await model.tagging.refreshEdits() }
    }

    @ViewBuilder private var tagging: some View {
        Text("MusicBrainz Tags").font(.title2)
        TextField("AcoustID API key", text: $acoustIDKey)
            .accessibilityIdentifier("settings-acoustid-key")
        Text("Get a key at acoustid.org/new-application")
            .font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
        if !acoustIDKey.isEmpty, AcoustIDKey.normalized(acoustIDKey) == nil {
            Text("Use the letters and digits of your AcoustID application key.")
                .font(.caption).foregroundStyle(.orange)
        }
        Text("Recent Tag Edits").font(.headline).padding(.top, 8)
        if let error = model.tagging.editsError {
            Text(error).font(.caption).foregroundStyle(.orange).textSelection(.enabled)
        }
        if model.tagging.edits.isEmpty {
            Text("No tag edits yet").foregroundStyle(.secondary)
        }
        ForEach(model.tagging.edits.prefix(50)) { edit in
            HStack {
                Text(edit.rowText(timeZone: .current)).textSelection(.enabled)
                Spacer()
                Button("Revert") { Task { await model.tagging.revert(edit.edit_id) } }
                    .disabled(edit.reverted || !model.connected)
                    .accessibilityIdentifier("tag-edit-revert-\(edit.edit_id)")
            }
        }
    }
}

/// Gain mode, resume on start, and what playing from a list does to Up Next.
struct PlaybackSettingsPane: View {
    @Environment(\.themeColours) private var colours
    let transport: TransportModel
    @Bindable var notifications: TrackNotifications
    @AppStorage("upNextOnPlay") private var upNextOnPlay = UpNextPolicy.ask.rawValue
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        Form {
            Picker("ReplayGain", selection: Binding(get: { transport.gainMode }, set: { transport.setGain($0) })) {
                Text("Off").tag("off")
                Text("Track").tag("track")
                Text("Album").tag("album")
            }
            .disabled(!transport.canMutate)
            .accessibilityIdentifier("gain-mode")
            Toggle("Resume playback when the server starts", isOn: Binding(
                get: { transport.resumeOnStart }, set: { transport.setResume($0) }))
                .disabled(!transport.canMutate)
                .accessibilityIdentifier("resume-on-start")
            Button("Open Equalizer…") { openWindow(id: "equalizer") }
                .accessibilityIdentifier("settings-open-equalizer")
            Picker("When playing from a list with Up Next not empty", selection: $upNextOnPlay) {
                Text("Ask").tag(UpNextPolicy.ask.rawValue)
                Text("Replace Up Next").tag(UpNextPolicy.replace.rawValue)
                Text("Keep Up Next").tag(UpNextPolicy.keep.rawValue)
            }
            .accessibilityIdentifier("settings-up-next-on-play")
            Text("Double-click or Return plays from the clicked track to the end of the list. Keep plays it now and the old Up Next entries follow.")
                .font(.caption).foregroundStyle(.secondary)
            Section("Track notifications") {
                Toggle("Notify when the track changes", isOn: Binding(
                    get: { notifications.enabled }, set: { notifications.setEnabled($0) }))
                    .disabled(notifications.requesting)
                    .accessibilityIdentifier("notifications-enabled")
                Toggle("Show while Tyranny is frontmost", isOn: $notifications.showWhileActive)
                    .disabled(!notifications.enabled)
                    .accessibilityIdentifier("notifications-while-active")
                Text(notifications.authorization).font(.caption).foregroundStyle(.secondary)
                    .accessibilityIdentifier("notifications-authorization")
                if let error = notifications.error {
                    Text(error).font(.caption).foregroundStyle(colours.error)
                }
            }
        }
        .formStyle(.grouped)
        .padding(12).frame(width: 600)
        .task { await notifications.refreshAuthorization() }
    }
}

struct ArtworkSettingsPane: View {
    @Environment(\.themeColours) private var colours
    let settings: ArtworkSettings
    @State private var token = ""

    var body: some View {
        Form {
            Section("Remote artwork fallback") {
                Text("Embedded and folder artwork always come first. Enable remote sources and use the arrows to set their fallback order.")
                    .font(.caption).foregroundStyle(.secondary)
                ForEach(settings.preferences.order, id: \.self) { provider in
                    HStack {
                        Toggle(provider.title, isOn: Binding(
                            get: { settings.preferences.enabled.contains(provider) },
                            set: { settings.setEnabled(provider, $0) }))
                            .accessibilityIdentifier("artwork-enabled-\(provider.rawValue)")
                        Spacer()
                        Button { settings.move(provider, by: -1) } label: { Image(systemName: "arrow.up") }
                            .disabled(settings.preferences.order.first == provider)
                            .accessibilityLabel("Move \(provider.title) earlier")
                            .accessibilityIdentifier("artwork-up-\(provider.rawValue)")
                        Button { settings.move(provider, by: 1) } label: { Image(systemName: "arrow.down") }
                            .disabled(settings.preferences.order.last == provider)
                            .accessibilityLabel("Move \(provider.title) later")
                            .accessibilityIdentifier("artwork-down-\(provider.rawValue)")
                    }
                }
            }
            Section("Discogs personal access token") {
                SecureField("Token", text: $token)
                    .accessibilityIdentifier("artwork-discogs-token")
                HStack {
                    Button("Save in Keychain") {
                        if settings.saveToken(token) { token = "" }
                    }
                    .accessibilityIdentifier("artwork-discogs-save")
                    Button("Remove token", action: settings.removeToken)
                        .disabled(!settings.hasToken)
                        .accessibilityIdentifier("artwork-discogs-remove")
                    Text(settings.hasToken ? "Token saved in Keychain" : "No token saved")
                        .font(.caption).foregroundStyle(.secondary)
                }
                Link("Create a personal token at Discogs", destination: URL(string: "https://www.discogs.com/settings/developers")!)
                if let error = settings.error {
                    Text(error).foregroundStyle(colours.error).font(.caption)
                        .accessibilityIdentifier("artwork-settings-error")
                }
            }
            Section {
                Text("Deezer images are displayed only while needed, not cached for reuse. Discogs images expire within six hours and are hidden if they cannot be refreshed. Both are excluded from notification attachments.")
                Text("Remote lookup sends album and artist metadata to the enabled providers. Artwork remains subject to its owners’ rights; Deezer and Discogs access is intended for personal, noncommercial use.")
                Text("This application uses Discogs’ API but is not affiliated with, sponsored or endorsed by Discogs. ‘Discogs’ is a trademark of Zink Media, LLC.")
            }
            .font(.caption).foregroundStyle(.secondary)
        }
        .formStyle(.grouped)
        .padding(12).frame(width: 600, height: 570)
        .onDisappear { token = "" }
    }
}
