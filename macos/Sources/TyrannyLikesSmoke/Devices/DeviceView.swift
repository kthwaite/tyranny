import SwiftUI
import TyrannyLikesSmokeCore

/// Local USB music management. Neither inventory nor removal depends on the playback server.
struct DeviceView: View {
    @Environment(\.themeColours) private var colours
    let model: DeviceSyncModel
    let player: PlayerModel
    let deviceID: String
    @State private var search = ""
    @State private var selection: Set<UInt32> = []
    @State private var removing: Removal?

    private struct Removal {
        let storageID: UInt32
        let storageName: String
        let deviceName: String
        let songs: [MTPSong]
    }

    // MTP object IDs are storage-scoped. Only the selected storage's rows enter this table.
    private struct SongRow: Identifiable {
        let song: MTPSong
        var id: UInt32 { song.id }
    }

    private var device: MTPDevice? { model.device(deviceID) }
    private var storageID: UInt32? { device.flatMap { model.storageID(for: $0) } }
    private var storage: MTPStorage? { device?.storages.first { $0.id == storageID } }
    private var songs: [MTPSong] { device?.songs.filter { $0.storage_id == storageID } ?? [] }
    private var visibleSongs: [MTPSong] {
        let query = search.trimmingCharacters(in: .whitespacesAndNewlines)
        return songs.filter { query.isEmpty || $0.name.localizedStandardContains(query) || $0.path.localizedStandardContains(query) }
            .sorted { left, right in
                let comparison = left.name.localizedStandardCompare(right.name)
                return comparison == .orderedSame ? left.id < right.id : comparison == .orderedAscending
            }
    }
    private var canRemove: Bool { device != nil && storage != nil && !model.busy && !selection.isEmpty }

    var body: some View {
        VStack(spacing: 0) {
            header
            status
            Divider()
            if let device {
                connectedContents(device)
            } else {
                ContentUnavailableView {
                    Label("Device Disconnected", systemImage: "cable.connector.slash")
                } description: {
                    Text("Reconnect the device and enable USB file transfer to browse its music. No songs can be changed while it is disconnected.")
                } actions: {
                    Button("Find Devices") { Task { await model.refresh() } }
                        .disabled(model.busy)
                        .accessibilityIdentifier("device-reconnect")
                }
                .accessibilityIdentifier("device-disconnected")
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("main-device")
        .task(id: device != nil) {
            if device != nil { await model.inventory(deviceID, onlyIfNeeded: true) }
        }
        .onChange(of: storageID) { _, _ in clearSelection() }
        .onChange(of: device?.songs) { _, _ in clearSelection() }
        .onChange(of: search) { _, _ in clearSelection() }
        .onChange(of: model.busy) { _, busy in if busy { removing = nil } }
        .alert("Delete \(removing?.songs.count ?? 0) \(removing?.songs.count == 1 ? "song" : "songs") from device?",
               isPresented: Binding(get: { removing != nil }, set: { if !$0 { removing = nil } }),
               presenting: removing) { removal in
            Button("Delete from Device", role: .destructive) { confirmRemoval(removal) }
                .disabled(model.busy || device == nil || storageID != removal.storageID)
                .accessibilityIdentifier("device-delete-confirm")
            Button("Cancel", role: .cancel) {}
                .accessibilityIdentifier("device-delete-cancel")
        } message: { removal in
            Text("This permanently deletes the selected \(removal.songs.count == 1 ? "song" : "songs") from \(removal.storageName) on \(removal.deviceName). This cannot be undone. Your library's original files are not changed.")
        }
    }

    private var header: some View {
        HStack(spacing: 10) {
            Label(device?.name ?? "Device", systemImage: "ipod")
                .font(.title2.weight(.semibold))
                .lineLimit(1)
                .accessibilityAddTraits(.isHeader)
                .accessibilityIdentifier("device-name")
            Spacer(minLength: 8)
            Button {
                Task {
                    if device != nil { await model.inventory(deviceID) }
                    else { await model.refresh() }
                }
            } label: {
                Label("Refresh", systemImage: "arrow.clockwise")
            }
            .disabled(model.busy)
            .help("Read the device's current storage and music inventory")
            .accessibilityIdentifier("device-refresh")
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
    }

    @ViewBuilder private var status: some View {
        if model.busy || !model.status.isEmpty || model.error != nil {
            VStack(alignment: .leading, spacing: 6) {
                if model.busy || !model.status.isEmpty {
                    HStack(spacing: 8) {
                        if model.busy {
                            ProgressView().controlSize(.small)
                                .accessibilityLabel("Device operation in progress")
                        }
                        Text(model.status.isEmpty ? "Working with device…" : model.status)
                            .font(.callout)
                            .foregroundStyle(.secondary)
                            .textSelection(.enabled)
                    }
                    .accessibilityElement(children: .combine)
                    .accessibilityIdentifier("device-status")
                }
                if let error = model.error {
                    HStack(alignment: .top, spacing: 8) {
                        Label(error, systemImage: "exclamationmark.triangle")
                            .font(.callout)
                            .foregroundStyle(colours.error)
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                        Button { model.error = nil } label: {
                            Image(systemName: "xmark")
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel("Dismiss Device Error")
                        .accessibilityIdentifier("device-error-dismiss")
                    }
                    .accessibilityIdentifier("device-error")
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 16)
            .padding(.bottom, 10)
        }
    }

    private func connectedContents(_ device: MTPDevice) -> some View {
        VStack(spacing: 0) {
            HStack(spacing: 12) {
                Picker("Storage", selection: Binding<UInt32?>(
                    get: { storageID },
                    set: { model.selectedStorage[deviceID] = $0 }
                )) {
                    ForEach(device.storages, id: \.id) { storage in
                        Text(storage.name).tag(Optional(storage.id))
                    }
                }
                .frame(maxWidth: 280)
                .disabled(model.busy || device.storages.isEmpty)
                .accessibilityIdentifier("device-storage-picker")
                Spacer(minLength: 0)
                TextField("Search Device Music", text: $search)
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 200)
                    .accessibilityLabel("Search Device Music")
                    .accessibilityIdentifier("device-search")
                Button("Delete Selected…", role: .destructive) { requestRemoval(selection) }
                    .disabled(!canRemove)
                    .accessibilityIdentifier("device-delete-selected")
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 8)
            Text("Drag songs from your library onto this device to copy them to the selected storage. Existing music stays in place.")
                .font(.callout)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 16)
                .padding(.bottom, 10)
            Divider()
            songTable
            Divider()
            if let storage {
                DeviceStorageBar(storage: storage, songs: songs, selectedCount: selection.count)
            } else {
                Text(model.busy ? "Reading device storage…" : "No storage available. Unlock the device and enable USB file transfer, then refresh.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(16)
                    .accessibilityIdentifier("device-storage-unavailable")
            }
        }
        .trackDragHover(model: player, enabled: !model.busy && storage != nil, destination: nil,
                        hover: {}, drop: { player.dropTracksOnDevice(token: $0, deviceID: deviceID) })
    }

    private var songTable: some View {
        let rows = visibleSongs.map { SongRow(song: $0) }
        return Table(rows, selection: $selection) {
            TableColumn("Song") { row in
                Text(row.song.name).help(row.song.name)
            }
            TableColumn("Location") { row in
                Text(row.song.path).foregroundStyle(.secondary).help(row.song.path)
            }
            TableColumn("Size") { row in
                Text(DeviceStorageBar.bytes(row.song.size))
                    .monospacedDigit()
                    .frame(maxWidth: .infinity, alignment: .trailing)
            }
            .width(min: 80, ideal: 100, max: 130)
        }
        .contextMenu(forSelectionType: UInt32.self) { ids in
            if !ids.isEmpty {
                Button("Delete from Device…", role: .destructive) { requestRemoval(ids) }
                    .disabled(model.busy || storage == nil)
                    .accessibilityIdentifier("device-song-delete")
            }
        }
        .onDeleteCommand { requestRemoval(selection) }
        // Storage IDs define separate row identities. Recreate the table rather
        // than diffing unrelated card contents through AppKit's row-height cache.
        .id(storageID)
        .overlay {
            if rows.isEmpty {
                ContentUnavailableView {
                    Label(model.busy ? "Reading Device Music" : search.isEmpty ? "No Songs to Display" : "No Matching Songs",
                          systemImage: search.isEmpty ? "music.note" : "magnifyingglass")
                } description: {
                    Text(model.busy ? "Keep the device connected while the operation completes."
                         : search.isEmpty ? "Refresh to read existing music, or drag library songs onto the device to copy them."
                         : "Try a different song name or location.")
                }
                .allowsHitTesting(false)
            }
        }
        .accessibilityIdentifier("device-songs-table")
    }

    private func clearSelection() {
        selection.removeAll()
        removing = nil
    }

    private func requestRemoval(_ ids: Set<UInt32>) {
        guard !model.busy, let device, let storage else { return }
        let requested = visibleSongs.filter { ids.contains($0.id) }
        guard !requested.isEmpty else { return }
        removing = Removal(storageID: storage.id, storageName: storage.name, deviceName: device.name, songs: requested)
    }

    private func confirmRemoval(_ removal: Removal) {
        guard !model.busy, let device, storageID == removal.storageID else { return }
        // Confirmation applies only to the exact inventory shown, never a subsequently reused object ID.
        guard removal.songs.allSatisfy({ requested in
            device.songs.contains {
                $0.storage_id == removal.storageID && $0.id == requested.id && $0.path == requested.path
                    && $0.size == requested.size && $0.sha256 == requested.sha256
            }
        }) else {
            model.error = "The device's music changed. Refresh and select the songs again before deleting."
            clearSelection()
            return
        }
        clearSelection()
        Task { await model.remove(deviceID: deviceID, storageID: removal.storageID, songs: removal.songs) }
    }
}

/// Actual storage usage: audio bytes from the device inventory, non-audio used space, and free space.
private struct DeviceStorageBar: View {
    @Environment(\.themeColours) private var colours
    let storage: MTPStorage
    let songs: [MTPSong]
    let selectedCount: Int

    var body: some View {
        let usage = DeviceStorageUsage(storage: storage, songs: songs)
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text("\(DisplayFormat.count(songs.count)) \(songs.count == 1 ? "song" : "songs")")
                if selectedCount > 0 { Text("· \(DisplayFormat.count(selectedCount)) selected") }
                Spacer()
                Text(storage.capacity == 0 ? "Capacity unavailable" : "\(Self.bytes(storage.capacity)) total")
            }
            .font(.callout)
            .foregroundStyle(.secondary)
            .monospacedDigit()
            .accessibilityIdentifier("device-song-count")
            if storage.capacity > 0 {
                GeometryReader { geometry in
                    HStack(spacing: 0) {
                        Rectangle().fill(colours.accent)
                            .frame(width: width(usage.music, in: geometry.size.width))
                        Rectangle().fill(Color.secondary.opacity(0.6))
                            .frame(width: width(usage.other, in: geometry.size.width))
                        Rectangle().fill(Color.secondary.opacity(0.15))
                            .frame(width: width(usage.free, in: geometry.size.width))
                    }
                    .clipShape(RoundedRectangle(cornerRadius: 4))
                }
                .frame(height: 14)
                .accessibilityElement(children: .ignore)
                .accessibilityAddTraits(.isImage)
                .accessibilityLabel("\(storage.name) storage usage")
                .accessibilityValue("Music \(Self.bytes(usage.music)), Other \(Self.bytes(usage.other)), Free \(Self.bytes(usage.free))")
                .accessibilityIdentifier("device-capacity-bar")
                HStack(spacing: 18) {
                    legend("Music", bytes: usage.music, colour: colours.accent)
                    legend("Other", bytes: usage.other, colour: .secondary.opacity(0.6))
                    Spacer(minLength: 0)
                    legend("Free", bytes: usage.free, colour: .secondary.opacity(0.15))
                }
                .font(.caption)
                .monospacedDigit()
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
        .background(.bar)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("device-storage-summary")
    }

    private func width(_ bytes: UInt64, in available: CGFloat) -> CGFloat {
        CGFloat(Double(bytes) / Double(storage.capacity)) * max(0, available)
    }

    private func legend(_ title: String, bytes: UInt64, colour: Color) -> some View {
        HStack(spacing: 5) {
            RoundedRectangle(cornerRadius: 2).fill(colour).frame(width: 8, height: 8)
                .accessibilityHidden(true)
            Text("\(title) \(Self.bytes(bytes))")
        }
        .accessibilityElement(children: .combine)
    }

    static func bytes(_ value: UInt64) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(clamping: value), countStyle: .file)
    }
}
