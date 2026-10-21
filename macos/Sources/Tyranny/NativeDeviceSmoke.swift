#if DEBUG
import AppKit
import Foundation
import TyrannyCore

/// Read-only unless --device-smoke-track explicitly authorizes one disposable fixture.
/// The write path requires an isolated daemon containing exactly that single local file.
@MainActor
enum NativeDeviceSmoke {
    private static var started = false

    static func startIfRequested(_ model: PlayerModel, navigation: NavigationModel) {
        let args = CommandLine.arguments
        guard !started, let index = args.firstIndex(of: "--device-smoke-output"), index + 1 < args.count else { return }
        started = true
        let output = URL(fileURLWithPath: args[index + 1], isDirectory: true)
        let fixture = args.firstIndex(of: "--device-smoke-track").flatMap { $0 + 1 < args.count ? args[$0 + 1] : nil }
        let kit = NativePlaylistKit(model: model, navigation: navigation, output: output, tag: "DEVICE_SMOKE")
        Task {
            var failure: String?
            do {
                try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
                try await kit.wait("USB device", seconds: 90) { !model.devices.devices.isEmpty }
                let target = model.devices.devices[0]
                if let window = NSApp.windows.first(where: { $0.title == "Tyranny" }) {
                    window.setContentSize(NSSize(width: 1180, height: 740))
                    window.makeKeyAndOrderFront(nil)
                    NSApp.activate(ignoringOtherApps: true)
                }
                try await kit.open(.device(target.id))
                try await kit.wait("device inventory", seconds: 180) {
                    !model.devices.busy && model.devices.hasInventory(target.id)
                }
                try kit.check(model.devices.error == nil, model.devices.error ?? "Device inventory failed")
                try capture("device", output: output)
                try await kit.drive("exists", "device-songs-table")
                try await kit.drive("exists", "device-capacity-bar")
                let initial = model.devices.device(target.id)!
                kit.observations["device"] = try JSONSerialization.jsonObject(with: JSONEncoder().encode(initial))
                kit.passed("real-usb-discovery-sidebar-inventory-capacity")
                if initial.storages.count > 1 {
                    let alternate = initial.storages[1]
                    try await kit.drive("choose", "device-storage-picker", alternate.name)
                    try await kit.wait("storage selection") { model.devices.storageID(for: initial) == alternate.id }
                    try capture("device-alternate-storage", output: output)
                    kit.passed("native-storage-picker")
                    try await kit.drive("choose", "device-storage-picker", initial.storages[0].name)
                }
                if let fixture { try await exerciseWrite(kit, target: initial, fixture: fixture) }
                model.disconnect()
                try await kit.drive("exists", "device-songs-table")
                kit.passed("device-management-independent-of-server")
            } catch {
                failure = error.localizedDescription
                try? capture("failure", output: output)
            }
            try? kit.finish(automation: "Real connected USB device; native sidebar navigation, storage picker and inventory. Writes only with explicit single-fixture argument.", error: failure)
            NativePlaylistKit.endSheets()
            NSApp.terminate(nil)
        }
    }

    private static func exerciseWrite(_ kit: NativePlaylistKit, target: MTPDevice, fixture: String) async throws {
        let model = kit.model
        try kit.requireIsolatedDaemon()
        try await kit.wait("fixture library") { model.connected && model.snapshot?.library.tracks == 1 }
        let rows = try await kit.songs(1)
        guard let row = rows.first else { throw DeviceSyncFailure("Fixture library is empty") }
        try kit.check(row.track.path == fixture && URL(fileURLWithPath: fixture).lastPathComponent.hasPrefix("Tyranny-sync-smoke-"),
                      "Write smoke only accepts the explicitly named disposable fixture")
        let initialIDs = Set(target.songs.map { "\($0.storage_id):\($0.id)" })
        try await kit.open(.songs)
        try await kit.wait("fixture Songs row") { NativeSongsSmoke.songsList?.cache.loadedRow(at: 0) != nil }
        try await kit.select("track-table-songs", [0])
        try await kit.drag("track-table-songs", row: 0, steps: [NativePlaylistKit.target(Destination.device(target.id).identifier)])
        try await kit.wait("first copy", seconds: 180) {
            !model.devices.busy && (model.devices.status.hasPrefix("Copied ") || model.devices.error != nil)
        }
        try kit.check(model.devices.error == nil, model.devices.error ?? "Copy failed")
        let copied = model.devices.device(target.id)!.songs.filter { !initialIDs.contains("\($0.storage_id):\($0.id)") }
        try kit.check(copied.count == 1, "Expected exactly one new device song")
        kit.passed("native-sidebar-drop-copies-one-song")
        // Repeating the same drag must leave the inventory and free space unchanged.
        try await kit.open(.songs)
        try await kit.wait("synced column", seconds: 180) { model.devices.syncStatus(for: row.track, deviceID: target.id) == "Synced" }
        try await kit.drive("scroll-horizontal-fraction", "track-table-songs", "1")
        try capture("songs-synced", output: kit.output)
        try await kit.drive("scroll-horizontal-fraction", "track-table-songs", "0")
        let afterFirst = model.devices.device(target.id)!
        try await kit.drag("track-table-songs", row: 0, steps: [NativePlaylistKit.target(Destination.device(target.id).identifier)])
        try await kit.wait("duplicate copy", seconds: 180) {
            !model.devices.busy && (model.devices.status == "Copied 0; already on device 1." || model.devices.error != nil)
        }
        try kit.check(model.devices.error == nil, model.devices.error ?? "Duplicate copy failed")
        try kit.check(model.devices.device(target.id)?.songs == afterFirst.songs, "Repeated drop duplicated or changed music")
        kit.passed("repeated-native-drop-is-idempotent-and-column-synced")
        try await kit.open(.device(target.id))
        try await kit.wait("refreshed inventory", seconds: 180) { !model.devices.busy }
        guard let added = copied.first else { throw DeviceSyncFailure("Missing copied fixture") }
        let storage = target.storages.first { $0.id == added.storage_id }!
        try await kit.drive("choose", "device-storage-picker", storage.name)
        try await kit.value("device-search", added.name)
        try await kit.drive("expect-row-count", "device-songs-table", "1")
        try await kit.select("device-songs-table", [0])
        try await kit.press("device-delete-selected")
        try await kit.press("device-delete-cancel")
        try kit.check(model.devices.device(target.id)?.songs.contains(where: { $0.id == added.id && $0.storage_id == added.storage_id }) == true,
                      "Cancel removed a device song")
        try await kit.press("device-delete-selected")
        try await kit.press("device-delete-confirm")
        try await kit.wait("fixture deletion", seconds: 180) {
            !model.devices.busy && (model.devices.status.hasPrefix("Removed 1 ") || model.devices.error != nil)
        }
        try kit.check(model.devices.error == nil, model.devices.error ?? "Deletion failed")
        let final = model.devices.device(target.id)!
        try kit.check(Set(final.songs.map { "\($0.storage_id):\($0.id)" }) == initialIDs, "Existing music inventory changed")
        try kit.check(FileManager.default.fileExists(atPath: fixture), "Deletion removed the library original")
        kit.passed("native-delete-confirmation-removes-only-fixture-and-preserves-library")
    }

    private static func capture(_ name: String, output: URL) throws {
        guard let view = NSApp.windows.first(where: { $0.title == "Tyranny" })?.contentView?.superview,
              let bitmap = view.bitmapImageRepForCachingDisplay(in: view.bounds) else {
            throw DeviceSyncFailure("Native device view unavailable")
        }
        view.cacheDisplay(in: view.bounds, to: bitmap)
        guard let data = bitmap.representation(using: .png, properties: [:]) else { throw DeviceSyncFailure("Cannot render device view") }
        try data.write(to: output.appendingPathComponent(name + ".png"))
    }
}
#endif
