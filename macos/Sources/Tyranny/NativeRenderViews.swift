#if DEBUG
import AppKit
import SwiftUI
import TyrannyCore

/// Debug-only self-render harness (`--render-views <dir>`). It walks every destination and writes
/// the window's own view tree to PNG with `cacheDisplay`. That needs no Screen Recording
/// permission and no Accessibility driver. The images are self-rendered, not composited
/// screenshots: window chrome shadows and some toolbar items can differ from the real window.
/// `scripts/render_views.py` prepares the server (fixture library, playlist, queue).
@MainActor
enum NativeRenderViews {
    private static var started = false

    static func startIfRequested(_ model: PlayerModel, navigation: NavigationModel) {
        let args = CommandLine.arguments
        guard !started, let index = args.firstIndex(of: "--render-views"), index + 1 < args.count else { return }
        started = true
        let output = URL(fileURLWithPath: args[index + 1], isDirectory: true)
        Task {
            var log: [String] = []
            do {
                try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
                let deadline = Date().addingTimeInterval(60)
                while !(model.connected && model.snapshot?.library.scan.phase == "complete"
                        && (model.snapshot?.library.tracks ?? 0) > 0) {
                    if Date() > deadline { throw NSError(domain: "render", code: 1, userInfo: [NSLocalizedDescriptionKey: "connect timeout"]) }
                    try await Task.sleep(for: .milliseconds(100))
                }
                if let window = NSApp.windows.first(where: { $0.title == "Tyranny" }) {
                    window.setContentSize(NSSize(width: 1180, height: 740))
                    window.makeKeyAndOrderFront(nil)
                }
                navigation.upNextDrawerVisible = false
                @MainActor func shot(_ name: String, settle: Double = 2.0) async {
                    try? await Task.sleep(for: .seconds(settle))
                    log.append(render(name, into: output))
                }
                // These are the user's real preferences; restore them when done.
                let saved = (browser: navigation.columnBrowserVisible,
                             drawer: navigation.upNextDrawerVisible)
                navigation.columnBrowserVisible = false
                navigation.setGrouping(.none, for: .songs)
                navigation.destination = .songs
                await shot("01-songs-ungrouped")
                navigation.setGrouping(.album, for: .songs)
                await shot("02-songs-grouped-by-album")
                navigation.upNextDrawerVisible = true
                await shot("03-songs-with-up-next-drawer")
                navigation.upNextDrawerVisible = false
                navigation.setGrouping(.none, for: .songs)
                navigation.columnBrowserVisible = true
                await shot("03b-songs-column-browser")
                navigation.columnBrowserVisible = false
                navigation.destination = .albums
                await shot("04-albums-grid", settle: 3)
                if let grid = NativeAlbumsSmoke.grid, let album = grid.album(at: 2) {
                    grid.toggleExpansion(album)
                    await shot("05-albums-grid-expanded", settle: 3)
                    grid.collapse()
                } else { log.append("albums: no grid or album at 2") }
                navigation.destination = .artists
                await shot("06-artists")
                navigation.destination = .genres
                await shot("07-genres")
                navigation.destination = .recent
                await shot("08-recently-added")
                navigation.destination = .history
                await shot("09-history")
                navigation.destination = .upNext
                await shot("10-up-next")
                if let playlist = model.savedPlaylists.rows.first(where: { !$0.isSmart }) {
                    navigation.destination = .playlist(playlist.id)
                    await shot("11-playlist-regular")
                } else { log.append("playlist: none") }
                log.append(renderSettings(model, navigation, into: output))
                navigation.setGrouping(.none, for: .songs) // the Songs default; earlier runs may have left another value
                navigation.columnBrowserVisible = saved.browser
                navigation.upNextDrawerVisible = saved.drawer
            } catch {
                log.append("error: \(error.localizedDescription)")
            }
            try? log.joined(separator: "\n").write(to: output.appendingPathComponent("render.log"), atomically: true, encoding: .utf8)
            NSApp.terminate(nil)
        }
    }

    private static func write(_ view: NSView, _ name: String, into output: URL) -> String {
        guard let bitmap = view.bitmapImageRepForCachingDisplay(in: view.bounds) else { return "\(name): no bitmap" }
        view.cacheDisplay(in: view.bounds, to: bitmap)
        guard let data = bitmap.representation(using: .png, properties: [:]) else { return "\(name): no png" }
        do { try data.write(to: output.appendingPathComponent(name + ".png")) } catch { return "\(name): \(error)" }
        return "\(name): ok \(Int(view.bounds.width))x\(Int(view.bounds.height)) \(data.count) bytes"
    }

    private static func render(_ name: String, into output: URL) -> String {
        guard let window = NSApp.windows.first(where: { $0.title == "Tyranny" }),
              let frame = window.contentView?.superview else { return "\(name): no window" }
        return write(frame, name, into: output)
    }

    /// Settings lives in its own scene; render its tabs in an off-screen hosting view.
    private static func renderSettings(_ model: PlayerModel, _ navigation: NavigationModel, into output: URL) -> String {
        var lines: [String] = []
        for (tab, name) in [(SettingsTab.connection, "12-settings-connection"), (.library, "13-settings-library"),
                            (.playback, "14-settings-playback")] {
            navigation.settingsTab = tab
            let host = NSHostingView(rootView: SettingsView(model: model, navigation: navigation))
            let size = host.fittingSize
            host.frame = NSRect(origin: .zero, size: NSSize(width: max(size.width, 600), height: max(size.height, 200)))
            let window = NSWindow(contentRect: host.frame, styleMask: [.titled], backing: .buffered, defer: false)
            window.contentView = host
            host.layoutSubtreeIfNeeded()
            lines.append(write(host, name, into: output))
        }
        return lines.joined(separator: "\n")
    }
}
#endif
