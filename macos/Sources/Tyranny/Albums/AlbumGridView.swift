import AppKit
import SwiftUI
import TyrannyCore

/// The Albums destination (spec 4.4): the view header with the sort and the cover size,
/// then the covers-first grid with its inline album panel.
struct AlbumGridView: View {
    let grid: AlbumGridModel
    let player: PlayerModel
    @Bindable var navigation: NavigationModel
    @AppStorage("albumGridCoverSize") private var coverSize = 140.0

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            AlbumCollection(
                grid: grid, player: player, generation: grid.cache.generation, epoch: grid.epoch,
                coverSize: CGFloat(coverSize), expandedID: grid.expansion?.albumID,
                artworkGeneration: player.artwork.generation)
                .overlay { emptyState }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("main-albums")
        .onChange(of: syncKey, initial: true) { _, _ in grid.sync() }
        .onChange(of: grid.cache.refreshes) { _, _ in grid.refreshed() }
        .onChange(of: player.scanActive) { _, _ in grid.refreshed() }
        .onChange(of: grid.filterError, initial: true) { _, error in navigation.filterError = error }
        .onDisappear { navigation.filterError = nil }
        #if DEBUG
        .onAppear { NativeAlbumsSmoke.grid = grid }
        #endif
    }

    private var syncKey: SyncKey {
        SyncKey(connected: player.connected, session: player.playlistConnectionGeneration,
                library: player.snapshot?.library.revision, filter: navigation.filter)
    }

    private struct SyncKey: Equatable {
        let connected: Bool
        let session: UInt64
        let library: UInt64?
        let filter: String
    }

    private var header: some View {
        HStack(spacing: 10) {
            Text("Albums")
                .font(.title2.weight(.semibold))
                .accessibilityAddTraits(.isHeader)
            Text(countText)
                .foregroundStyle(.secondary)
                .monospacedDigit()
                .accessibilityIdentifier("list-count")
            Spacer(minLength: 12)
            Picker("Sort", selection: Binding(get: { grid.sort }, set: { grid.setSort($0) })) {
                Text("Album Artist").tag(AlbumSort.albumArtist)
                Text("Year").tag(AlbumSort.year)
                Text("Date Added").tag(AlbumSort.dateAdded)
            }
            .pickerStyle(.menu)
            .fixedSize()
            .help("Sort")
            .accessibilityIdentifier("album-sort")
            HStack(spacing: 4) {
                Image(systemName: "square.grid.3x3").font(.system(size: 9)).foregroundStyle(.secondary)
                Slider(value: $coverSize, in: AlbumGridGeometry.coverRange)
                    .frame(width: 110)
                    .help("Cover Size")
                    .accessibilityLabel("Cover Size")
                    .accessibilityIdentifier("album-size")
                Image(systemName: "square.grid.2x2").font(.system(size: 12)).foregroundStyle(.secondary)
            }
        }
        .controlSize(.small)
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
    }

    private var countText: String {
        guard grid.cache.revision != nil else { return "" }
        let total = grid.cache.total
        return "\(DisplayFormat.count(total)) \(total == 1 ? "album" : "albums")"
    }

    @ViewBuilder private var emptyState: some View {
        if grid.cache.revision != nil, grid.cache.total == 0 {
            Text(grid.isNarrowed ? "No matching albums" : "No albums")
                .font(.title3)
                .foregroundStyle(.secondary)
                .allowsHitTesting(false)
        }
    }
}

/// The virtualized grid: an `NSCollectionView` flow layout over the model's window cache.
/// Only displayed items exist; each retains its cover while displayed.
///
/// The items follow `AlbumGridLayout`: albums up to the end of the expanded row, the panel,
/// then the other albums. Positions map to items through it for cells, selection, clicks and
/// the visible range. Selection is one album, kept by ID. Click opens or moves the panel
/// (clicking the open album closes it); Escape closes it; Return plays the selected album
/// and ⌥Return adds it to Up Next. Arrow keys move the selection, and the panel with it.
struct AlbumCollection: NSViewRepresentable {
    let grid: AlbumGridModel
    let player: PlayerModel
    /// Read in the parent's body so SwiftUI updates the grid when they change.
    let generation: UInt64
    let epoch: UInt64
    let coverSize: CGFloat
    let expandedID: UInt64?
    let artworkGeneration: UInt64

    static let identifier = "album-grid"

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.borderType = .noBorder
        let collection = AlbumCollectionView()
        collection.setAccessibilityIdentifier(Self.identifier)
        let flow = NSCollectionViewFlowLayout()
        flow.scrollDirection = .vertical
        flow.minimumInteritemSpacing = AlbumGridGeometry.spacing
        flow.minimumLineSpacing = AlbumGridGeometry.lineSpacing
        collection.collectionViewLayout = flow
        collection.isSelectable = true
        collection.allowsMultipleSelection = false
        collection.allowsEmptySelection = true
        collection.backgroundColors = [.controlBackgroundColor]
        collection.register(AlbumGridItem.self, forItemWithIdentifier: AlbumGridItem.reuse)
        collection.register(AlbumExpansionItem.self, forItemWithIdentifier: AlbumExpansionItem.reuse)
        collection.dataSource = context.coordinator
        collection.delegate = context.coordinator
        collection.coordinator = context.coordinator
        scroll.documentView = collection
        scroll.contentView.postsBoundsChangedNotifications = true
        scroll.contentView.postsFrameChangedNotifications = true
        NotificationCenter.default.addObserver(
            context.coordinator, selector: #selector(Coordinator.scrolled(_:)),
            name: NSView.boundsDidChangeNotification, object: scroll.contentView)
        NotificationCenter.default.addObserver(
            context.coordinator, selector: #selector(Coordinator.resized(_:)),
            name: NSView.frameDidChangeNotification, object: scroll.contentView)
        context.coordinator.attach(collection: collection, scroll: scroll)
        context.coordinator.update(self)
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        context.coordinator.update(self)
    }

    static func dismantleNSView(_ scroll: NSScrollView, coordinator: Coordinator) {
        NotificationCenter.default.removeObserver(coordinator)
        coordinator.dismantle()
    }

    @MainActor final class Coordinator: NSObject, @MainActor NSCollectionViewDataSource,
        @MainActor NSCollectionViewDelegateFlowLayout {
        var parent: AlbumCollection
        private weak var collection: AlbumCollectionView?
        private weak var scroll: NSScrollView?
        private var geometry = AlbumGridGeometry(width: 800, coverSize: 140)
        /// The layout the collection's items show. Replaced only with a reload.
        private var layout = AlbumGridLayout(count: 0, columns: 1, expanded: nil)
        private var shownEpoch: UInt64?
        private var shownGeneration: UInt64?
        private var shownArtwork: UInt64?
        private var reported: Range<Int>?
        /// The first album in view and its offset from the top of the view, restored after a
        /// refresh pass moves it.
        private var anchor: (id: UInt64, offset: CGFloat)?
        private var dismantled = false
        private var updating = false
        private var applying = false
        private var relayoutScheduled = false

        init(_ parent: AlbumCollection) { self.parent = parent }

        func attach(collection: AlbumCollectionView, scroll: NSScrollView) {
            self.collection = collection
            self.scroll = scroll
        }

        private var cache: ListWindowCache<AlbumSummary> { parent.grid.cache }
        private var store: ArtworkStore { parent.player.artwork }

        // MARK: Updates

        func update(_ parent: AlbumCollection) {
            self.parent = parent
            guard collection != nil, !dismantled else { return }
            updating = true
            defer { updating = false }
            let epochChanged = parent.epoch != shownEpoch
            let generationChanged = parent.generation != shownGeneration
            let artworkChanged = parent.artworkGeneration != shownArtwork
            shownEpoch = parent.epoch
            shownGeneration = parent.generation
            shownArtwork = parent.artworkGeneration
            if epochChanged {
                reported = nil
                anchor = nil
            }
            let relayout = applyLayout()
            if epochChanged {
                scroll?.contentView.scroll(to: .zero)
                scroll.map { $0.reflectScrolledClipView($0.contentView) }
            }
            if !relayout, generationChanged || artworkChanged { reconfigureVisible() }
            if generationChanged { restoreAnchor() }
            syncSelection()
            reportVisible()
        }

        /// Rebuilds the layout from the width, cover size, total and panel. Returns true when
        /// the items were reloaded.
        @discardableResult
        private func applyLayout() -> Bool {
            guard !applying else {
                relayoutSoon()
                return false
            }
            applying = true
            defer { applying = false }
            return applyLayoutOnce()
        }

        /// The clip view's width changed, for example when a reload shows or hides a legacy
        /// scroller. Reloading inside the scroll view's tiling leaves the collection view's
        /// frame stale, so the relayout waits for the next turn of the run loop.
        private func relayoutSoon() {
            guard !relayoutScheduled else { return }
            relayoutScheduled = true
            Task { @MainActor [weak self] in
                guard let self, !self.dismantled else { return }
                self.relayoutScheduled = false
                if self.applyLayout() { self.syncSelection() }
                self.reportVisible()
            }
        }

        private func applyLayoutOnce() -> Bool {
            guard let collection, let scroll else { return false }
            let next = AlbumGridGeometry(width: Double(scroll.contentView.bounds.width), coverSize: Double(parent.coverSize))
            var expanded: Int?
            if let id = parent.expandedID {
                expanded = cache.position(of: id)
                if expanded == nil, cache.revision != nil {
                    // The album left the list or its window: close the panel after this update.
                    Task { @MainActor [weak self] in
                        guard let self, self.parent.grid.expansion?.albumID == id else { return }
                        self.parent.grid.collapse()
                    }
                }
            }
            let nextLayout = AlbumGridLayout(count: cache.total, columns: next.columns, expanded: expanded)
            let geometryChanged = next != geometry
            let layoutChanged = nextLayout != layout
            let panelMoved = nextLayout.expanded != layout.expanded
            geometry = next
            guard geometryChanged || layoutChanged else { return false }
            let sameItems = nextLayout.hasSameItems(as: layout)
            layout = nextLayout
            collection.collectionViewLayout?.invalidateLayout()
            if sameItems {
                // A cover size or width change: re-layout and resize the items in place. They keep
                // their artwork requests, so in-flight covers are not cancelled.
                reconfigureVisible()
                return false
            }
            collection.reloadData()
            if panelMoved, let expanded = nextLayout.expanded { revealPanel(below: expanded) }
            return true
        }

        /// Redraws displayed items in place: rows arrived, or the artwork context changed.
        private func reconfigureVisible() {
            guard let collection else { return }
            for path in collection.indexPathsForVisibleItems() {
                guard let item = collection.item(at: path) else { continue }
                configure(item, at: path)
            }
        }

        private func syncSelection() {
            guard let collection else { return }
            let wanted: Set<IndexPath>
            if let id = parent.grid.selectedID, let position = cache.position(of: id),
               let location = layout.location(ofAlbum: position) {
                wanted = [IndexPath(item: location.item, section: location.section)]
            } else {
                wanted = []
            }
            if collection.selectionIndexPaths != wanted { collection.selectionIndexPaths = wanted }
        }

        /// Scrolls so the panel shows, keeping the clicked row in view.
        private func revealPanel(below position: Int) {
            guard let collection, let scroll, let row = layout.location(ofAlbum: position) else { return }
            collection.layoutSubtreeIfNeeded()
            guard let cell = collection.layoutAttributesForItem(at: IndexPath(item: row.item, section: row.section)),
                  let panel = collection.layoutAttributesForItem(at: IndexPath(item: 0, section: 1)) else { return }
            let visible = scroll.contentView.bounds
            guard panel.frame.maxY > visible.maxY || cell.frame.minY < visible.minY else { return }
            let y = max(0, min(panel.frame.maxY + 8 - visible.height, cell.frame.minY - 8))
            scroll.contentView.scroll(to: NSPoint(x: 0, y: y))
            scroll.reflectScrolledClipView(scroll.contentView)
        }

        /// After a refresh pass, puts the anchor album back at its offset from the top.
        private func restoreAnchor() {
            guard cache.anchorPosition != nil else { return }
            cache.anchorPosition = nil
            guard let anchor, let collection, let scroll, let position = cache.position(of: anchor.id),
                  let location = layout.location(ofAlbum: position) else { return }
            collection.layoutSubtreeIfNeeded()
            guard let frame = collection.layoutAttributesForItem(
                at: IndexPath(item: location.item, section: location.section))?.frame else { return }
            let y = max(0, frame.minY - anchor.offset)
            guard abs(y - scroll.contentView.bounds.minY) > 0.5 else { return }
            scroll.contentView.scroll(to: NSPoint(x: 0, y: y))
            scroll.reflectScrolledClipView(scroll.contentView)
        }

        @objc func scrolled(_ notification: Notification) { reportVisible() }

        @objc func resized(_ notification: Notification) {
            guard !dismantled else { return }
            relayoutSoon()
        }

        private func reportVisible() {
            guard !dismantled, let collection, let scroll else { return }
            let paths = collection.indexPathsForVisibleItems()
            let locations = paths.map { AlbumGridLayout.Location(section: $0.section, item: $0.item) }
            guard let range = layout.albumRange(of: locations) else { return }
            recordAnchor(in: paths, top: scroll.contentView.bounds.minY)
            guard range != reported else { return }
            reported = range
            parent.grid.visible(range)
        }

        private func recordAnchor(in paths: Set<IndexPath>, top: CGFloat) {
            guard let collection else { return }
            var best: (position: Int, frame: NSRect)?
            for path in paths {
                guard case .album(let position)? = layout.item(section: path.section, item: path.item),
                      cache.loadedRow(at: position) != nil,
                      let frame = collection.layoutAttributesForItem(at: path)?.frame,
                      frame.maxY > top else { continue }
                if best == nil || position < best!.position { best = (position, frame) }
            }
            guard let best, let album = cache.loadedRow(at: best.position) else { return }
            anchor = (album.id, best.frame.minY - top)
        }

        func dismantle() {
            dismantled = true
            reported = nil
            guard let collection else { return }
            for item in collection.visibleItems() {
                (item as? AlbumGridItem)?.releaseArtwork()
                (item as? AlbumExpansionItem)?.releaseArtwork()
            }
        }

        // MARK: Data source

        func numberOfSections(in collectionView: NSCollectionView) -> Int { AlbumGridLayout.sections }

        func collectionView(_ collectionView: NSCollectionView, numberOfItemsInSection section: Int) -> Int {
            layout.numberOfItems(inSection: section)
        }

        func collectionView(_ collectionView: NSCollectionView,
                            itemForRepresentedObjectAt indexPath: IndexPath) -> NSCollectionViewItem {
            let identifier = layout.item(section: indexPath.section, item: indexPath.item) == .expansion
                ? AlbumExpansionItem.reuse : AlbumGridItem.reuse
            let item = collectionView.makeItem(withIdentifier: identifier, for: indexPath)
            configure(item, at: indexPath)
            return item
        }

        private func configure(_ item: NSCollectionViewItem, at path: IndexPath) {
            switch layout.item(section: path.section, item: path.item) {
            case .album(let position)?:
                guard let cell = item as? AlbumGridItem else { return }
                let album = cache.row(at: position)
                cell.show(album, position: position, coverSize: CGFloat(geometry.coverSize), store: store,
                          retainArtwork: !dismantled)
                cell.onPress = { [weak self] in self?.clicked(position: position) }
            case .expansion?:
                guard let panel = item as? AlbumExpansionItem, let expansion = parent.grid.expansion else { return }
                let caret = CGFloat(geometry.caretX(column: layout.expandedColumn ?? 0))
                panel.show(expansion, grid: parent.grid, player: parent.player, caretX: caret)
            case nil:
                break
            }
        }

        // MARK: Delegate

        func collectionView(_ collectionView: NSCollectionView, didEndDisplaying item: NSCollectionViewItem,
                            forRepresentedObjectAt indexPath: IndexPath) {
            (item as? AlbumGridItem)?.releaseArtwork()
            (item as? AlbumExpansionItem)?.releaseArtwork()
        }

        func collectionView(_ collectionView: NSCollectionView, layout collectionViewLayout: NSCollectionViewLayout,
                            sizeForItemAt indexPath: IndexPath) -> NSSize {
            if layout.item(section: indexPath.section, item: indexPath.item) == .expansion {
                let count = parent.grid.expansion?.album.track_count ?? 0
                // Until a width change is applied, stay within the current width.
                let live = collectionView.bounds.width - CGFloat(2 * AlbumGridGeometry.minimumInset) - 1
                return NSSize(width: max(1, min(CGFloat(geometry.panelWidth), live)),
                              height: AlbumExpansionMetrics.height(trackCount: count))
            }
            return NSSize(width: geometry.itemWidth, height: geometry.itemHeight)
        }

        func collectionView(_ collectionView: NSCollectionView, layout collectionViewLayout: NSCollectionViewLayout,
                            insetForSectionAt section: Int) -> NSEdgeInsets {
            guard layout.numberOfItems(inSection: section) > 0 else { return NSEdgeInsetsZero }
            let left = CGFloat(geometry.horizontalInset), right = CGFloat(geometry.trailingInset)
            let edge = CGFloat(AlbumGridGeometry.minimumInset)
            let panelOpen = layout.numberOfItems(inSection: 1) > 0
            switch section {
            case 0: return NSEdgeInsets(top: edge, left: left, bottom: panelOpen ? 6 : edge, right: right)
            case 1: return NSEdgeInsets(top: 0, left: edge, bottom: CGFloat(AlbumGridGeometry.lineSpacing), right: edge)
            default: return NSEdgeInsets(top: 0, left: left, bottom: edge, right: right)
            }
        }

        func collectionView(_ collectionView: NSCollectionView,
                            shouldSelectItemsAt indexPaths: Set<IndexPath>) -> Set<IndexPath> {
            indexPaths.filter {
                guard case .album(let position)? = layout.item(section: $0.section, item: $0.item) else { return false }
                return cache.loadedRow(at: position) != nil
            }
        }

        func collectionView(_ collectionView: NSCollectionView, didSelectItemsAt indexPaths: Set<IndexPath>) {
            guard !updating else { return }
            selectionChanged()
        }

        func collectionView(_ collectionView: NSCollectionView, didDeselectItemsAt indexPaths: Set<IndexPath>) {
            guard !updating else { return }
            selectionChanged()
        }

        private func selectionChanged() {
            guard let collection else { return }
            let positions = collection.selectionIndexPaths.compactMap { path -> Int? in
                guard case .album(let position)? = layout.item(section: path.section, item: path.item) else { return nil }
                return position
            }
            parent.grid.selectedID = positions.first.flatMap { cache.loadedRow(at: $0)?.id }
        }

        // MARK: Input

        /// A click on an album selects it and opens, moves or closes the panel.
        func clicked(at path: IndexPath) {
            guard case .album(let position)? = layout.item(section: path.section, item: path.item) else { return }
            clicked(position: position)
        }

        private func clicked(position: Int) {
            guard let album = cache.loadedRow(at: position) else { return }
            parent.grid.selectedID = album.id
            parent.grid.toggleExpansion(album)
        }

        /// Return, ⌥Return, Escape and the arrow keys. Returns false for keys it does not handle.
        func key(_ event: NSEvent) -> Bool {
            let option = event.modifierFlags.contains(.option)
            switch event.keyCode {
            case 36, 76:  // Return, Enter
                guard let album = selectedAlbum else { return false }
                if option { parent.grid.addToUpNext(album) } else { parent.grid.play(album) }
                return true
            case 53:  // Escape
                guard parent.grid.expansion != nil else { return false }
                parent.grid.collapse()
                return true
            case 123: return move(by: -1)
            case 124: return move(by: 1)
            case 125: return move(by: layout.columns)
            case 126: return move(by: -layout.columns)
            default: return false
            }
        }

        private var selectedAlbum: AlbumSummary? {
            guard let id = parent.grid.selectedID else { return nil }
            if let position = cache.position(of: id), let album = cache.loadedRow(at: position) { return album }
            return parent.grid.expansion?.albumID == id ? parent.grid.expansion?.album : nil
        }

        private func move(by delta: Int) -> Bool {
            guard layout.count > 0, let collection else { return false }
            let current = parent.grid.selectedID.flatMap { cache.position(of: $0) }
            let target = current.map { min(max($0 + delta, 0), layout.count - 1) } ?? 0
            guard target != current, let album = cache.loadedRow(at: target) else { return true }
            parent.grid.selectedID = album.id
            // The panel follows the selection while it is open.
            if parent.grid.expansion != nil { parent.grid.toggleExpansion(album) }
            if let location = layout.location(ofAlbum: target) {
                collection.scrollToItems(at: [IndexPath(item: location.item, section: location.section)],
                                         scrollPosition: .nearestHorizontalEdge)
            }
            syncSelection()
            return true
        }
    }
}

/// Routes clicks and keys to the coordinator. Selection itself stays native.
final class AlbumCollectionView: NSCollectionView {
    weak var coordinator: AlbumCollection.Coordinator?

    override var acceptsFirstResponder: Bool { true }

    override func mouseDown(with event: NSEvent) {
        let point = convert(event.locationInWindow, from: nil)
        let path = indexPathForItem(at: point)
        window?.makeFirstResponder(self)
        super.mouseDown(with: event)
        if let path { coordinator?.clicked(at: path) }
    }

    override func keyDown(with event: NSEvent) {
        if coordinator?.key(event) == true { return }
        super.keyDown(with: event)
    }
}
