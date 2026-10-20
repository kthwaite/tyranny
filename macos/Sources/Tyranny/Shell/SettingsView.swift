import SwiftUI
import TyrannyCore

/// The Settings window (⌘,): Connection, Library and Playback.
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
    let model: PlayerModel
    var done: (() -> Void)?

    var body: some View {
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
                                Text(error).font(.caption).foregroundStyle(.orange).textSelection(.enabled)
                            }
                        }
                    }
                    Spacer()
                    Button("Rescan") { model.rescan(root.id) }
                        .disabled(!model.canMutate || model.scanActive)
                        .accessibilityIdentifier("rescan-root-\(root.id)")
                }.padding(.vertical, 4)
            }
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
        }
        .padding(20).frame(width: 600, height: 430)
    }
}

/// Gain mode, resume on start, and what playing from a list does to Up Next.
struct PlaybackSettingsPane: View {
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
                    Text(error).font(.caption).foregroundStyle(.red)
                }
            }
        }
        .formStyle(.grouped)
        .padding(12).frame(width: 600)
        .task { await notifications.refreshAuthorization() }
    }
}
