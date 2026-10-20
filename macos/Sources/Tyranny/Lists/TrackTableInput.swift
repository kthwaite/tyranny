import AppKit
import TyrannyCore

/// A table that plays on Return and toggles play/pause on Space, and builds its context menu
/// for the row under the pointer. Used by every track table.
final class ActivatingTableView: NSTableView {
    var onActivate: ((ActivationAction) -> Void)?
    var onPlayPause: (() -> Void)?
    /// Delete and ⌘Delete. Nil in lists that cannot remove rows.
    var onRemove: (() -> Void)?
    /// Escape. Nil in tables that pass it on.
    var onCancel: (() -> Void)?
    /// Builds the menu for the row under the pointer, selecting it first when it is not selected.
    var contextMenu: ((Int) -> NSMenu?)?

    override func keyDown(with event: NSEvent) {
        let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
            .subtracting([.capsLock, .numericPad, .function])
        switch event.keyCode {
        case 36, 76:
            // Return and keypad Enter. A modifier other than ⌥ is ignored, not read as plain Return.
            if let action = ActivationKey.action(option: flags.contains(.option),
                                                 otherModifiers: !flags.subtracting(.option).isEmpty) {
                onActivate?(action)
            }
        case 51 where onRemove != nil && PlaylistRemoval.isRemoveKey(keyCode: 51, otherModifiers: !flags.subtracting(.command).isEmpty),
             117 where onRemove != nil && PlaylistRemoval.isRemoveKey(keyCode: 117, otherModifiers: !flags.subtracting(.command).isEmpty):
            onRemove?()
        case 49 where flags.isEmpty && onPlayPause != nil:
            onPlayPause?()
        case 53 where flags.isEmpty && onCancel != nil:
            onCancel?()
        default:
            super.keyDown(with: event)
        }
    }

    override func menu(for event: NSEvent) -> NSMenu? {
        let row = row(at: convert(event.locationInWindow, from: nil))
        return contextMenu?(row)
    }
}

extension VirtualTrackTable.Coordinator {
    func configureInput(_ table: ActivatingTableView) {
        table.onActivate = { [weak self] action in self?.parent.list.activateSelected(action, asRun: true) }
        table.onPlayPause = { [weak self] in self?.parent.player.transport.playPause() }
        table.onRemove = { [weak self] in
            guard let list = self?.parent.list, list.canRemoveEntries else { return }
            list.removeSelected()
        }
        table.contextMenu = { [weak self] row in self?.menu(forRow: row) }
    }

    /// ⌘L: select the playing row and bring it into view.
    func applyReveal(_ token: UInt64?) {
        guard let seen = shownReveal else {
            shownReveal = .some(token)
            return
        }
        guard seen != token else { return }
        shownReveal = .some(token)
        guard let table, let request = parent.list.revealRequest,
              let display = layout.displayIndex(ofPosition: request.position) else { return }
        table.selectRowIndexes(IndexSet(integer: display), byExtendingSelection: false)
        table.scrollRowToVisible(display)
    }

    // MARK: Context menu

    private func menu(forRow row: Int) -> NSMenu? {
        guard let table, case .track? = layout.row(at: row) else { return nil }
        if !table.selectedRowIndexes.contains(row) {
            table.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
        }
        let menu = NSMenu(title: "Tracks")
        let busy = !parent.player.canMutate
        add(to: menu, "Play", "track-menu-play", #selector(menuPlay), enabled: !busy)
        add(to: menu, "Play Next", "track-menu-play-next", #selector(menuPlayNext), enabled: !busy)
        add(to: menu, "Add to Up Next", "track-menu-add-to-up-next", #selector(menuAddToUpNext), enabled: !busy)
        menu.addItem(.separator())
        add(to: menu, "Add to Playlist…", "track-menu-add-to-playlist", #selector(menuAddToPlaylist), enabled: !busy)
        if parent.list.canRemoveEntries {
            add(to: menu, "Remove from Playlist", "track-menu-remove-from-playlist", #selector(menuRemove), enabled: !busy)
        }
        menu.addItem(.separator())
        add(to: menu, "Show in Album", "track-menu-show-album", #selector(menuShowAlbum), enabled: true)
        add(to: menu, "Show in Artist", "track-menu-show-artist", #selector(menuShowArtist), enabled: true)
        return menu
    }

    private func add(to menu: NSMenu, _ title: String, _ identifier: String, _ action: Selector, enabled: Bool) {
        let item = NSMenuItem(title: title, action: enabled ? action : nil, keyEquivalent: "")
        item.target = self
        item.setAccessibilityIdentifier(identifier)
        menu.addItem(item)
    }

    // The menu plays the selected rows only, not a run to the end of the list.
    @objc private func menuPlay() { parent.list.activateSelected(.play, asRun: false) }
    @objc private func menuPlayNext() { parent.list.activateSelected(.playNext) }
    @objc private func menuAddToUpNext() { parent.list.activateSelected(.addToUpNext) }
    @objc private func menuAddToPlaylist() { parent.list.requestPlaylistPicker() }
    @objc private func menuRemove() { parent.list.removeSelected() }
    @objc private func menuShowAlbum() { parent.list.showInAlbum() }
    @objc private func menuShowArtist() { parent.list.showInArtist() }
}
