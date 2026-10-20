#if DEBUG
import AppKit
import SwiftUI
import TyrannyCore

/// Debug-only self-render harness (`--render-views <dir>`). It walks every destination and writes
/// the window's own view tree to PNG with `cacheDisplay`. That needs no Screen Recording
/// permission and no Accessibility driver. The images are self-rendered, not composited
/// screenshots: window chrome shadows and some toolbar items can differ from the real window.
/// `scripts/render_views.py` prepares the server (fixture library, playlist, queue) and passes
/// `--render-error-source <file>`, an undecodable file the harness plays to show a playback error.
@MainActor
enum NativeRenderViews {
    private static var started = false
    /// Tables skip column autosave while rendering, so the images show the default widths and
    /// leave the user's saved widths alone.
    static let isRendering = CommandLine.arguments.contains("--render-views")

    static func startIfRequested(_ model: PlayerModel, navigation: NavigationModel) {
        let args = CommandLine.arguments
        guard !started, let index = args.firstIndex(of: "--render-views"), index + 1 < args.count else { return }
        started = true
        let output = URL(fileURLWithPath: args[index + 1], isDirectory: true)
        let errorSource = args.firstIndex(of: "--render-error-source").flatMap { $0 + 1 < args.count ? args[$0 + 1] : nil }
        Task {
            var log: [String] = []
            // These are the user's real preferences. Capture them before any change, and restore
            // them however the run ends.
            let saved = (browser: navigation.columnBrowserVisible,
                         drawer: navigation.upNextDrawerVisible,
                         art: navigation.nowPlayingArtExpanded,
                         grouping: navigation.grouping(for: .songs))
            var savedSidebarCollapsed: Bool?
            var savedFrame: NSRect?
            do {
                try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
                try await waitForLibrary(model)
                if let window = NSApp.windows.first(where: { $0.title == "Tyranny" }) {
                    savedFrame = window.frame
                    window.setContentSize(NSSize(width: 1180, height: 740))
                    // Render at the sharpest scale on hand, so images match across machines'
                    // display arrangements.
                    if let screen = NSScreen.screens.max(by: { $0.backingScaleFactor < $1.backingScaleFactor }),
                       window.screen != screen {
                        let visible = screen.visibleFrame
                        window.setFrameTopLeftPoint(NSPoint(x: visible.minX + 40, y: visible.maxY - 40))
                    }
                    NSApp.activate(ignoringOtherApps: true)
                    window.makeKeyAndOrderFront(nil)
                }
                // Every render but 18 shows the sidebar.
                if let sidebar = sidebarProbe()?.sidebarItem() {
                    savedSidebarCollapsed = sidebar.isCollapsed
                    sidebar.isCollapsed = false
                }
                navigation.upNextDrawerVisible = false
                @MainActor func shot(_ name: String, settle: Double = 2.0) async {
                    try? await Task.sleep(for: .seconds(settle))
                    log.append(render(name, into: output))
                }
                navigation.nowPlayingArtExpanded = false
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
                // The player bar: thumbnail in the bar, art expanded into the sidebar, minimum width.
                navigation.destination = .songs
                navigation.nowPlayingArtExpanded = false
                await shot("15-player-bar-collapsed")
                navigation.nowPlayingArtExpanded = true
                await shot("16-player-bar-art-expanded")
                // Collapse the sidebar through AppKit, not a SwiftUI binding: the thumbnail returns.
                // The probe is unmounted while the sidebar collapses, then mounted again, as at a
                // launch with the sidebar already collapsed: only its initial report can bring the
                // thumbnail back.
                if let probe = sidebarProbe(), let sidebar = probe.sidebarItem(), let parent = probe.superview {
                    probe.removeFromSuperview()
                    sidebar.isCollapsed = true
                    try? await Task.sleep(for: .seconds(1))
                    parent.addSubview(probe)
                    await shot("18-player-bar-sidebar-hidden")
                    sidebar.isCollapsed = false
                    try? await Task.sleep(for: .seconds(1))
                } else { log.append("sidebar hidden: no probe or sidebar item") }
                navigation.nowPlayingArtExpanded = false
                if let window = NSApp.windows.first(where: { $0.title == "Tyranny" }) {
                    window.setContentSize(NSSize(width: 940, height: 560))
                    await shot("17-player-bar-min-width")
                    window.setContentSize(NSSize(width: 1180, height: 740))
                } else { log.append("player bar min width: no window") }
                // Disconnected: blank bar, disabled controls. Then connect again for what follows.
                model.disconnect()
                await shot("19-player-bar-disconnected")
                model.start()
                try await waitForLibrary(model)
                if let errorSource {
                    try await showPlaybackError(model, source: errorSource) {
                        await shot("20-player-bar-error")
                    }
                } else { log.append("player bar error: no --render-error-source") }
                log.append(renderSettings(model, navigation, into: output))
            } catch {
                log.append("error: \(error.localizedDescription)")
            }
            navigation.setGrouping(saved.grouping, for: .songs)
            navigation.columnBrowserVisible = saved.browser
            navigation.upNextDrawerVisible = saved.drawer
            navigation.nowPlayingArtExpanded = saved.art
            if let savedSidebarCollapsed { sidebarProbe()?.sidebarItem()?.isCollapsed = savedSidebarCollapsed }
            if let savedFrame { NSApp.windows.first(where: { $0.title == "Tyranny" })?.setFrame(savedFrame, display: false) }
            try? log.joined(separator: "\n").write(to: output.appendingPathComponent("render.log"), atomically: true, encoding: .utf8)
            NSApp.terminate(nil)
        }
    }

    private struct RenderFailure: LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }

    /// Waits until the model is connected to a fully scanned, non-empty library.
    private static func waitForLibrary(_ model: PlayerModel) async throws {
        try await waitFor("connect", model, seconds: 60) {
            model.connected && model.snapshot?.library.scan.phase == "complete"
                && (model.snapshot?.library.tracks ?? 0) > 0
        }
    }

    private static func waitFor(_ what: String, _ model: PlayerModel, seconds: Double, _ condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(seconds)
        while !condition() {
            if Date() > deadline {
                let state = model.snapshot.map { "\($0.transport) current \($0.current_id.map(String.init) ?? "none") at \($0.position_frames)" }
                throw RenderFailure(message: "\(what) timeout (\(model.connection), \(state ?? "no snapshot"))")
            }
            try await Task.sleep(for: .milliseconds(100))
        }
    }

    /// Puts the transport into its error state, runs `shot`, then puts the queue back.
    /// The undecodable `source` is appended after the last entry. Playing that last entry makes
    /// the player open `source` when it reaches it, which fails. Afterwards the appended entry is
    /// removed, and the previously current entry is selected and paused again.
    private static func showPlaybackError(_ model: PlayerModel, source: String,
                                          shot: () async -> Void) async throws {
        guard let state = model.snapshot, let resumeID = state.current_id else {
            throw RenderFailure(message: "player bar error: nothing current")
        }
        let appended: Applied = try await model.send("append", [
            "expected_revision": .uint(state.queue_revision), "path": .string(source)])
        let queue: Page<QueueItem> = try await model.send("queue_page", [
            "revision": .null, "offset": .int(0), "limit": .int(500)])
        guard let broken = appended.entry_id ?? queue.entries.last?.id,
              let index = queue.entries.firstIndex(where: { $0.id == broken }), index > 0 else {
            throw RenderFailure(message: "player bar error: appended entry not found")
        }
        let last = queue.entries[index - 1].id
        let _: Applied = try await model.send("play", ["entry_id": .uint(last)])
        let failed = { model.snapshot?.transport == "error" && model.snapshot?.error != nil }
        // The decoder reads ahead, so a short last entry reaches the undecodable one at once.
        // A long one is first skipped to near its end. A seek during the error would clear it,
        // so it is sent only while that entry still plays.
        if (try? await waitFor("playback error", model, seconds: 5, failed)) == nil {
            guard model.snapshot?.transport == "playing", model.snapshot?.current_id == last,
                  let frames = model.snapshot?.now_playing?.duration_frames, frames > 4800 else {
                throw RenderFailure(message: "player bar error: the last entry is not playing")
            }
            let _: Applied = try await model.send("seek", [
                "entry_id": .uint(last), "position_frames": .uint(frames - 4800)])
            try await waitFor("playback error", model, seconds: 15, failed)
        }
        await shot()
        // Remove the undecodable entry first, so the read-ahead cannot reach it again.
        try await model.refreshSnapshot()
        let _: Applied = try await model.send("remove", [
            "expected_revision": .uint(model.snapshot?.queue_revision ?? 0), "entry_id": .uint(broken)])
        let _: Applied = try await model.send("play", ["entry_id": .uint(resumeID)])
        let _: Applied = try await model.send("pause")
        try await waitFor("error cleared", model, seconds: 10) {
            model.snapshot?.transport == "paused" && model.snapshot?.error == nil
                && model.snapshot?.current_id == resumeID
        }
    }

    /// The main window's sidebar visibility probe, found in the view tree.
    private static func sidebarProbe() -> SidebarVisibilityProbe.ProbeView? {
        func probe(in view: NSView) -> SidebarVisibilityProbe.ProbeView? {
            if let probe = view as? SidebarVisibilityProbe.ProbeView { return probe }
            for child in view.subviews { if let found = probe(in: child) { return found } }
            return nil
        }
        guard let window = NSApp.windows.first(where: { $0.title == "Tyranny" }),
              let root = window.contentView?.superview else { return nil }
        return probe(in: root)
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
#else
enum NativeRenderViews {
    static let isRendering = false
}
#endif
